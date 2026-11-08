//! 原生 SSH：逐跳核对主机密钥、加密私钥与多轮交互认证；不启动外部 ssh 程序。
use crate::{business::remote::RemoteSession, connections::ConnectionTarget, http::AppState};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// 将服务器密钥验证转换为有期限的会话挑战。
pub struct HostHandler {
    session: Arc<RemoteSession>,
    target: ConnectionTarget,
    hop: usize,
    accepted_host_key: Option<String>,
}
impl russh::client::Handler for HostHandler {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let (fingerprint, algorithm) = match key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => (
                key.fingerprint(russh::keys::HashAlg::Sha256).to_string(),
                key.algorithm().to_string(),
            ),
            russh::keys::PublicKeyOrCertificate::Certificate(c) => (
                c.public_key()
                    .fingerprint(russh::keys::HashAlg::Sha256)
                    .to_string(),
                c.algorithm().to_string(),
            ),
        };
        let expected = self.accepted_host_key.as_deref().or_else(|| {
            (!self.target.host_key.is_empty()).then_some(self.target.host_key.as_str())
        });
        if let Some(expected) = expected {
            return Ok(fingerprint == expected);
        }
        let answer=self.session.ask("host_key",json!({"host":self.target.host,"port":self.target.port,"hop_index":self.hop,"algorithm":algorithm,"fingerprint":fingerprint,"previous_fingerprint":null,"changed":false})).await;
        let accepted = answer.is_ok_and(|a| a["decision"] == "accept_once");
        if accepted {
            self.accepted_host_key = Some(fingerprint);
        }
        Ok(accepted)
    }
}

async fn authenticate(
    handle: &mut russh::client::Handle<HostHandler>,
    target: &ConnectionTarget,
    hop: usize,
    session: &Arc<RemoteSession>,
    state: &Arc<AppState>,
    initial: Option<&str>,
) -> Result<(), String> {
    let credential = if let Some(id) = initial {
        Some(
            state
                .business
                .remote
                .resolve_credential(session.owner.session_id, id)
                .map_err(|e| e.message)?,
        )
    } else {
        None
    };
    let credential = match credential {
        Some(c) => c,
        None => {
            let answer=session.ask("credentials",json!({"host":target.host,"port":target.port,"hop_index":hop,"username":target.username,"methods":["password","private_key","keyboard_interactive"]})).await?;
            if answer["credential_ref"] == "keyboard_interactive" {
                return interactive(handle, target, hop, session).await;
            }
            let id = answer["credential_ref"]
                .as_str()
                .ok_or("credential reference required")?;
            state
                .business
                .remote
                .resolve_credential(session.owner.session_id, id)
                .map_err(|e| e.message)?
        }
    };
    let auth = if credential.kind == "password" {
        handle
            .authenticate_password(&target.username, &credential.secret)
            .await
            .map_err(|_| "SSH password authentication failed".to_string())?
    } else {
        let secret = credential.secret.clone();
        let passphrase = credential.passphrase.clone();
        let key = tokio::task::spawn_blocking(move || {
            russh::keys::decode_secret_key(&secret, passphrase.as_deref())
        })
        .await
        .map_err(|_| "key decoding task failed")?
        .map_err(|_| "private key or passphrase is invalid")?;
        let hash = key
            .algorithm()
            .is_rsa()
            .then_some(russh::keys::HashAlg::Sha256);
        handle
            .authenticate_publickey(
                &target.username,
                russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash),
            )
            .await
            .map_err(|_| "SSH key authentication failed".to_string())?
    };
    if auth.success() {
        Ok(())
    } else if matches!(
        auth,
        russh::client::AuthResult::Failure {
            partial_success: true,
            ..
        }
    ) {
        interactive(handle, target, hop, session).await
    } else {
        Err("SSH credentials rejected".into())
    }
}

async fn interactive(
    handle: &mut russh::client::Handle<HostHandler>,
    target: &ConnectionTarget,
    hop: usize,
    session: &RemoteSession,
) -> Result<(), String> {
    use russh::client::KeyboardInteractiveAuthResponse as A;
    let mut result = handle
        .authenticate_keyboard_interactive_start(&target.username, None::<String>)
        .await
        .map_err(|_| "interactive authentication failed")?;
    for round in 1..=32 {
        match result {
            A::Success => return Ok(()),
            A::Failure { .. } => return Err("interactive authentication rejected".into()),
            A::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                if prompts.len() > 32 {
                    return Err("too many authentication prompts".into());
                }
                let items:Vec<Value>=prompts.iter().enumerate().map(|(i,p)|json!({"id":format!("{round}-{i}"),"prompt":p.prompt,"echo":p.echo})).collect();
                let answer=session.ask("keyboard_interactive",json!({"host":target.host,"port":target.port,"hop_index":hop,"name":name,"instruction":instructions,"round":round,"prompts":items})).await?;
                let answers = answer["answers"].as_array().ok_or("answers required")?;
                if answers.len() != prompts.len() {
                    return Err("authentication answer count mismatch".into());
                }
                let mut values = Vec::new();
                for (i, a) in answers.iter().enumerate() {
                    if a["id"] != format!("{round}-{i}") {
                        return Err("stale authentication answer".into());
                    }
                    values.push(
                        a["value"]
                            .as_str()
                            .ok_or("answer value required")?
                            .to_string(),
                    );
                }
                result = handle
                    .authenticate_keyboard_interactive_respond(values)
                    .await
                    .map_err(|_| "interactive response failed")?;
            }
        }
    }
    Err("authentication round limit reached".into())
}

/// 建立跳板链并复用最后一条连接的终端和 SFTP 通道。
pub async fn prepare(
    session: Arc<RemoteSession>,
    state: Arc<AppState>,
    credential: Option<String>,
) {
    let work = async {
        let mut chain: Vec<ConnectionTarget> = session.jumps.clone();
        chain.push(session.target.clone());
        let config = Arc::new(russh::client::Config {
            inactivity_timeout: Some(Duration::from_secs(180)),
            keepalive_interval: Some(Duration::from_secs(30)),
            keepalive_max: 3,
            ..Default::default()
        });
        let mut handles: Vec<russh::client::Handle<HostHandler>> = vec![];
        for (hop, target) in chain.iter().enumerate() {
            let handler = HostHandler {
                session: session.clone(),
                target: target.clone(),
                hop,
                accepted_host_key: None,
            };
            let connected = async {
                if let Some(previous) = handles.last() {
                    let channel = previous
                        .channel_open_direct_tcpip(&target.host, target.port as u32, "127.0.0.1", 0)
                        .await
                        .map_err(|_| "SSH jump forwarding rejected")?;
                    russh::client::connect_stream(config.clone(), channel.into_stream(), handler)
                        .await
                        .map_err(|_| "SSH handshake or host key verification failed")
                } else {
                    russh::client::connect(
                        config.clone(),
                        (target.host.as_str(), target.port),
                        handler,
                    )
                    .await
                    .map_err(|_| "SSH connection or host key verification failed")
                }
            };
            // 主机密钥挑战可能需要用户核对；有独立120秒期限。
            let mut handle = tokio::time::timeout(Duration::from_secs(150), connected)
                .await
                .map_err(|_| "SSH handshake timed out")??;
            session.transition("authenticating");
            authenticate(
                &mut handle,
                target,
                hop,
                &session,
                &state,
                if hop + 1 == chain.len() {
                    credential.as_deref()
                } else {
                    None
                },
            )
            .await?;
            handles.push(handle);
        }
        let handle = handles.last().ok_or("SSH chain is empty")?;
        let sftp = async {
            let mut channel = handle
                .channel_open_session()
                .await
                .map_err(|_| "SFTP channel rejected")?;
            channel
                .request_subsystem(true, "sftp")
                .await
                .map_err(|_| "SFTP subsystem rejected")?;
            wait_channel_request(&mut channel).await?;
            russh_sftp::client::SftpSession::new(channel.into_stream())
                .await
                .map_err(|_| "SFTP initialization failed")
        };
        match tokio::time::timeout(Duration::from_secs(15), sftp).await {
            Ok(Ok(s)) => {
                *session.sftp.lock().await = Some(Arc::new(s));
            }
            _ => {
                *session.error.lock().unwrap() = Some("该服务器没有可用的 SFTP 子系统".into());
            }
        }
        *session.ssh.lock().await = handles;
        session.transition("transport_ready");
        Ok::<(), String>(())
    };
    let result = tokio::select! {biased; _=session.cancelled()=>Err("connection cancelled".into()),result=work=>result};
    if let Err(error) = result {
        if !*session.cancel.borrow() {
            *session.error.lock().unwrap() = Some(error);
            session.transition("failed");
        }
    } else {
        let mut lease = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {biased; _=session.cancelled()=>break,_=lease.tick()=>{if state.access.resolve(session.owner.session_id,session.owner.mode).is_err(){session.close();break;}}}
        }
    }
    let sftp = session.sftp.lock().await.take();
    if let Some(sftp) = sftp {
        let cleanup = async {
            let slots = {
                let _creation = session.transfers.1.lock().await;
                std::mem::take(&mut *session.transfers.0.lock().await)
            };
            crate::sftp::cleanup_transfers(&sftp, slots.into_values()).await;
            let _ = sftp.close().await;
        };
        if tokio::time::timeout(Duration::from_secs(15), cleanup)
            .await
            .is_err()
        {
            tracing::warn!("SFTP shutdown cleanup timed out");
        }
    }
    let handles = std::mem::take(&mut *session.ssh.lock().await);
    let disconnect = async {
        for handle in handles.into_iter().rev() {
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "session closed", "")
                .await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(5), disconnect)
        .await
        .is_err()
    {
        tracing::warn!("SSH disconnect timed out");
    }
}

pub(crate) async fn wait_channel_request(
    channel: &mut russh::Channel<russh::client::Msg>,
) -> Result<(), &'static str> {
    loop {
        match channel.wait().await {
            Some(russh::ChannelMsg::Success) => return Ok(()),
            Some(russh::ChannelMsg::WindowAdjusted { .. }) => {}
            _ => return Err("SSH channel request was rejected"),
        }
    }
}

/// terminal.v1：输入只收二进制，控制帧严格限定 resize/close。
pub async fn stream(mut socket: axum::extract::ws::WebSocket, session: Arc<RemoteSession>) {
    use axum::extract::ws::Message;
    use russh::ChannelMsg;
    let setup = async {
        let mut channel = {
            let handles = session.ssh.lock().await;
            let handle = handles.last().ok_or("SSH connection closed")?;
            handle
                .channel_open_session()
                .await
                .map_err(|_| "terminal channel rejected")?
        };
        let requests = async {
            channel
                .request_pty(true, "xterm-256color", 100, 30, 0, 0, &[])
                .await
                .map_err(|_| "PTY request failed")?;
            wait_channel_request(&mut channel)
                .await
                .map_err(|_| "PTY rejected")?;
            channel
                .request_shell(true)
                .await
                .map_err(|_| "shell request failed")?;
            wait_channel_request(&mut channel)
                .await
                .map_err(|_| "shell rejected")?;
            Ok::<_, &str>(())
        };
        let result = tokio::time::timeout(Duration::from_secs(10), requests)
            .await
            .map_err(|_| "terminal request timed out")
            .and_then(|result| result);
        if let Err(error) = result {
            let _ = tokio::time::timeout(Duration::from_secs(2), channel.close()).await;
            return Err(error);
        }
        Ok(channel)
    };
    let setup = tokio::select! {
        biased;
        _ = session.cancelled() => return,
        result = tokio::time::timeout(Duration::from_secs(15), setup) =>
            result.map_err(|_| "terminal setup timed out").and_then(|result| result),
    };
    let mut channel = match setup {
        Ok(c) => c,
        Err(error) => {
            let _ = crate::http::send_ws_message(
                &mut socket,
                Message::Text(
                    json!({"v":1,"type":"terminal.error","message":error})
                        .to_string()
                        .into(),
                ),
            )
            .await;
            return;
        }
    };
    session.transition("active");
    if crate::http::send_ws_message(
        &mut socket,
        Message::Text(json!({"v":1,"type":"terminal.ready"}).to_string().into()),
    )
    .await
    .is_err()
    {
        let _ = tokio::time::timeout(Duration::from_secs(2), channel.close()).await;
        return;
    }
    loop {
        if *session.cancel.borrow() {
            break;
        }
        tokio::select! {
            _=session.cancelled()=>break,
            result=channel.wait()=>match result{
                Some(ChannelMsg::Data{data})|Some(ChannelMsg::ExtendedData{data,..})=>{if tokio::time::timeout(Duration::from_secs(15),socket.send(Message::Binary(data.to_vec().into()))).await.map_or(true,|r|r.is_err()){break;}},
                Some(ChannelMsg::ExitStatus{exit_status})=>{let _=crate::http::send_ws_message(&mut socket, Message::Text(json!({"v":1,"type":"terminal.exit","exit_code":exit_status,"signal":null}).to_string().into())).await;break;},Some(ChannelMsg::Eof|ChannelMsg::Close)|None=>break,_=>{}
            },
            message=socket.recv()=>match message{
                Some(Ok(Message::Binary(data)))=>{if tokio::time::timeout(Duration::from_secs(15),channel.data(&data[..])).await.map_or(true,|r|r.is_err()){break;}},
                Some(Ok(Message::Text(text))) => {
                    if text.len()>16384 { break; }
                    let Ok(v)=serde_json::from_str::<Value>(&text) else { break; };
                    if v["v"]!=1 || v["type"]!="terminal.resize" { break; }
                    let (Some(cols),Some(rows))=(v["cols"].as_u64(),v["rows"].as_u64()) else { break; };
                    if !(1..=1000).contains(&cols) || !(1..=1000).contains(&rows) { break; }
                    if !matches!(tokio::time::timeout(Duration::from_secs(5), channel.window_change(cols as u32,rows as u32,0,0)).await, Ok(Ok(()))) { break; }
                },
                Some(Ok(Message::Ping(p)))=>{if crate::http::send_ws_message(&mut socket, Message::Pong(p)).await.is_err(){break;}},_=>break
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        let _ = channel.eof().await;
        let _ = channel.close().await;
    })
    .await;
    let _ = crate::http::send_ws_message(&mut socket, Message::Close(None)).await;
}

#[cfg(test)]
mod channel_tests {
    use super::*;

    struct Client;
    impl russh::client::Handler for Client {
        type Error = russh::Error;
        async fn check_server_key(
            &mut self,
            _: &russh::keys::PublicKeyOrCertificate,
        ) -> Result<bool, Self::Error> {
            Ok(true)
        }
    }
    struct Server(usize);
    impl russh::server::Handler for Server {
        type Error = russh::Error;
        async fn auth_none(&mut self, _: &str) -> Result<russh::server::Auth, Self::Error> {
            Ok(russh::server::Auth::Accept)
        }
        async fn channel_open_session(
            &mut self,
            _channel: russh::Channel<russh::server::Msg>,
            reply: russh::server::ChannelOpenHandle,
            _: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }
        async fn shell_request(
            &mut self,
            channel: russh::ChannelId,
            session: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            match self.0 {
                0 => session.channel_success(channel)?,
                1 => session.channel_failure(channel)?,
                _ => {}
            }
            self.0 += 1;
            Ok(())
        }
    }

    #[tokio::test]
    async fn accepted_host_key_is_bound_to_this_connection() {
        use crate::access::{AccessContext, AccessMode, LocalPrincipal};
        use russh::client::Handler as _;
        let now = std::time::SystemTime::now();
        let owner = AccessContext {
            session_id: 1,
            principal: LocalPrincipal {
                principal_id: "test".into(),
                scopes: vec![],
            },
            mode: AccessMode::Cookie,
            csrf: String::new(),
            expires_at: now,
            absolute_expires_at: now,
            created_at: now,
        };
        let target: ConnectionTarget = serde_json::from_value(json!({"id":"test","label":"Test",
            "protocol":"ssh","host":"127.0.0.1","port":22}))
        .unwrap();
        let session = crate::business::remote::RemoteHub::default()
            .create(
                owner,
                target.clone(),
                vec![],
                "test".into(),
                "test".into(),
                1,
            )
            .unwrap();
        let mut handler = HostHandler {
            session: session.clone(),
            target,
            hop: 0,
            accepted_host_key: None,
        };
        let key = |seed| {
            let private: russh::keys::PrivateKey =
                russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[seed; 32]).into();
            russh::keys::PublicKeyOrCertificate::from(private.public_key().clone())
        };
        let accepted = key(1);
        {
            let waiting = handler.check_server_key(&accepted);
            tokio::pin!(waiting);
            assert!(futures_util::poll!(&mut waiting).is_pending());
            session
                .challenge
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .response
                .take()
                .unwrap()
                .send(json!({"decision":"accept_once"}))
                .unwrap();
            assert!(waiting.await.unwrap());
        }
        assert!(handler.check_server_key(&accepted).await.unwrap());
        assert!(!handler.check_server_key(&key(2)).await.unwrap());
        assert!(session.challenge.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn ssh_channel_requests_distinguish_acknowledgement_rejection_and_silence() {
        let (client, server) = tokio::io::duplex(65536);
        let mut config = russh::server::Config::default();
        let key = russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]);
        config.keys.push(key.into());
        let server = tokio::spawn(async move {
            let running = russh::server::run_stream(Arc::new(config), server, Server(0))
                .await
                .unwrap();
            let _ = running.await;
        });
        let mut handle = russh::client::connect_stream(
            Arc::new(russh::client::Config::default()),
            client,
            Client,
        )
        .await
        .unwrap();
        assert!(handle.authenticate_none("test").await.unwrap().success());
        let mut channel = handle.channel_open_session().await.unwrap();
        channel.request_shell(true).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), wait_channel_request(&mut channel))
                .await
                .unwrap()
                .is_ok()
        );
        channel.request_shell(true).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), wait_channel_request(&mut channel))
                .await
                .unwrap()
                .is_err()
        );
        channel.request_shell(true).await.unwrap();
        assert!(tokio::time::timeout(
            Duration::from_millis(100),
            wait_channel_request(&mut channel)
        )
        .await
        .is_err());
        server.abort();
    }
}
