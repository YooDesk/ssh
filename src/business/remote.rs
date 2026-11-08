//! 非 desk 连接的归属、限额与关闭代次。终端和 SFTP 共享同一会话。
use crate::access::AccessContext;
use crate::connections::ConnectionTarget;
use crate::json::ApiError;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, watch};

/// 有界业务会话注册表。
#[derive(Default)]
pub struct RemoteHub {
    profiles: std::sync::OnceLock<Mutex<crate::connections::Catalog>>,
    sessions: Mutex<HashMap<uuid::Uuid, Arc<RemoteSession>>>,
    credentials: Mutex<CredentialMap>,
}
type CredentialMap = HashMap<String, (u64, std::time::Instant, Arc<Credential>)>;

/// 短期凭据只保存在内存，不接受服务器任意文件路径。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    /// password/private_key。
    pub kind: String,
    /// 密码或 PEM/OpenSSH 文本。
    pub secret: String,
    /// 加密密钥口令。
    #[serde(default)]
    pub passphrase: Option<String>,
    /// 当前不支持持久保存，明确拒绝 true。
    pub persist: bool,
}

/// 单个待回答挑战，绑定随机 id 与当前轮次。
pub struct PendingChallenge {
    /// 公共挑战 DTO。
    pub info: Value,
    /// 一次性回答。
    pub response: Option<tokio::sync::oneshot::Sender<Value>>,
    pub(crate) expires: tokio::time::Instant,
    id: uuid::Uuid,
}

struct ChallengeGuard<'a> {
    slot: &'a Mutex<Option<PendingChallenge>>,
    id: uuid::Uuid,
}
impl Drop for ChallengeGuard<'_> {
    fn drop(&mut self) {
        let mut pending = self.slot.lock().unwrap();
        if pending.as_ref().is_some_and(|item| item.id == self.id) {
            pending.take();
        }
    }
}

/// 一次业务连接，只允许一个原生 stream。
pub struct RemoteSession {
    /// 防止幂等请求重复启动认证。
    pub preparing: AtomicBool,
    /// 同一连接内的文件传输集合。
    pub transfers: crate::sftp::Transfers,
    /// SSH 握手后保活所有跳板。
    pub ssh: tokio::sync::Mutex<Vec<russh::client::Handle<crate::ssh::HostHandler>>>,
    /// 同一 SSH 连接上的 SFTP 子系统。
    pub sftp: tokio::sync::Mutex<Option<Arc<russh_sftp::client::SftpSession>>>,
    /// 当前挑战；新挑战替代旧挑战。
    pub challenge: Mutex<Option<PendingChallenge>>,
    /// 会话错误，不含敏感认证内容。
    pub error: Mutex<Option<String>>,
    /// 随机业务编号。
    pub id: uuid::Uuid,
    /// 服务端目标快照。
    pub target: ConnectionTarget,
    /// 创建时冻结整条跳板链，后续编辑配置不会重定向正在认证的连接。
    pub jumps: Vec<ConnectionTarget>,
    /// 创建它的访问会话。
    pub owner: AccessContext,
    /// 连接不能被第二个 WS 窃取。
    pub attached: AtomicBool,
    /// 关闭信号。
    pub cancel: watch::Sender<bool>,
    state: Mutex<(String, u64)>,
    /// 最新状态通知；慢客户端关闭重新读取快照。
    pub events: broadcast::Sender<Value>,
    request_id: String,
    request_fingerprint: String,
    created: std::time::Instant,
}

impl RemoteHub {
    /// 父进程停止时撤销所有远端业务；释放集合锁后关闭会话，避免锁嵌套。
    pub fn close_all(&self) {
        let sessions: Vec<_> = self.sessions.lock().unwrap().values().cloned().collect();
        for session in sessions {
            session.close();
        }
        self.credentials.lock().unwrap().clear();
    }
    /// 配置加载已完成严格文件校验；首次使用时创建内存目录。
    pub fn profiles(&self, config: &crate::config::Config) -> &Mutex<crate::connections::Catalog> {
        self.profiles
            .get_or_init(|| Mutex::new(crate::connections::Catalog::new(config)))
    }
    /// 保存五分钟短期凭据，容量随显式会话配置，保留默认64及硬上限1024。
    pub fn credential(
        &self,
        owner: u64,
        credential: Credential,
        session_limit: usize,
    ) -> Result<String, ApiError> {
        if !matches!(credential.kind.as_str(), "password" | "private_key")
            || credential.secret.is_empty()
            || credential.secret.len() > 262144
            || credential
                .passphrase
                .as_ref()
                .is_some_and(|s| s.len() > 4096)
        {
            return Err(ApiError::new(400, "invalid_request", "invalid credential"));
        }
        if credential.persist {
            return Err(ApiError::new(
                400,
                "unsupported_feature",
                "persistent credentials are not enabled",
            ));
        }
        let mut items = self.credentials.lock().unwrap();
        items.retain(|_, (_, expires, _)| *expires > std::time::Instant::now());
        if items.len() >= session_limit.clamp(64, 1024) {
            return Err(ApiError::new(429, "capacity_exceeded", "credential limit"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        items.insert(
            id.clone(),
            (
                owner,
                std::time::Instant::now() + std::time::Duration::from_secs(300),
                Arc::new(credential),
            ),
        );
        Ok(id)
    }
    /// 凭据不能被另一个访问会话引用。
    pub fn resolve_credential(&self, owner: u64, id: &str) -> Result<Arc<Credential>, ApiError> {
        self.credentials
            .lock()
            .unwrap()
            .get(id)
            .filter(|(o, e, _)| *o == owner && *e > std::time::Instant::now())
            .map(|(_, _, c)| c.clone())
            .ok_or_else(|| ApiError::new(404, "not_found", "credential expired or not found"))
    }
    /// 创建会话；相同访问主体的相同请求编号幂等。
    pub fn create(
        &self,
        owner: AccessContext,
        target: ConnectionTarget,
        jumps: Vec<ConnectionTarget>,
        request_id: String,
        request_fingerprint: String,
        limit: usize,
    ) -> Result<Arc<RemoteSession>, ApiError> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, s| {
            !*s.cancel.borrow() || s.created.elapsed() < std::time::Duration::from_secs(86400)
        });
        if let Some(s) = sessions
            .values()
            .find(|s| s.owner.session_id == owner.session_id && s.request_id == request_id)
        {
            return if s.target.id == target.id && s.request_fingerprint == request_fingerprint {
                Ok(s.clone())
            } else {
                Err(ApiError::new(
                    409,
                    "conflict",
                    "request id reused for another target",
                ))
            };
        }
        // 活动配额与幂等历史分开；历史满时仅淘汰最旧的已关闭连接。
        // 相同request_id已在上方命中，重试不会因为腾出空间而重复启动。
        if sessions.values().filter(|s| !*s.cancel.borrow()).count() >= limit {
            return Err(ApiError::new(
                429,
                "capacity_exceeded",
                "connection limit reached",
            ));
        }
        if sessions.len() >= 1024 {
            let oldest = sessions
                .values()
                .filter(|s| *s.cancel.borrow())
                .min_by_key(|s| s.created)
                .map(|s| s.id);
            if let Some(id) = oldest {
                sessions.remove(&id);
            } else {
                return Err(ApiError::new(
                    429,
                    "capacity_exceeded",
                    "connection limit reached",
                ));
            }
        }
        let (cancel, _) = watch::channel(false);
        let (events, _) = broadcast::channel(32);
        let initial = if target.protocol == "ssh" {
            "connecting"
        } else {
            "transport_ready"
        };
        let s = Arc::new(RemoteSession {
            preparing: AtomicBool::new(false),
            transfers: Default::default(),
            ssh: Default::default(),
            sftp: Default::default(),
            challenge: Mutex::new(None),
            error: Mutex::new(None),
            id: uuid::Uuid::new_v4(),
            target,
            jumps,
            owner,
            attached: AtomicBool::new(false),
            cancel,
            state: Mutex::new((initial.into(), 1)),
            events,
            request_id,
            request_fingerprint,
            created: std::time::Instant::now(),
        });
        sessions.insert(s.id, s.clone());
        Ok(s)
    }
    /// 按访问会话归属查找，其他会话统一 404。
    pub fn find(
        &self,
        owner: &AccessContext,
        id: uuid::Uuid,
        protocol: &str,
    ) -> Result<Arc<RemoteSession>, ApiError> {
        self.sessions
            .lock()
            .unwrap()
            .get(&id)
            .filter(|s| s.owner.session_id == owner.session_id && s.target.protocol == protocol)
            .cloned()
            .ok_or_else(|| ApiError::new(404, "not_found", "session not found"))
    }

    /// 退出立即关闭连接并移除该访问会话的临时凭据。
    pub fn close_owner(&self, owner: u64) {
        for session in self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.owner.session_id == owner)
        {
            session.close();
        }
        self.credentials
            .lock()
            .unwrap()
            .retain(|_, (id, _, _)| *id != owner);
    }
}

impl RemoteSession {
    /// 状态式取消：订阅前已关闭也立即完成，不能只等待下一次变更。
    pub async fn cancelled(&self) {
        let mut receiver = self.cancel.subscribe();
        let _ = receiver.wait_for(|closed| *closed).await;
    }
    // A queued request must not start remote I/O after session closure.
    pub(crate) async fn lock_live_transfers(
        &self,
    ) -> Option<
        tokio::sync::MutexGuard<'_, std::collections::HashMap<String, crate::sftp::TransferSlot>>,
    > {
        tokio::select! {
            biased;
            _ = self.cancelled() => None,
            guard = self.transfers.0.lock() => {
                if *self.cancel.borrow() { None } else { Some(guard) }
            }
        }
    }

    pub(crate) async fn lock_live_transfer(
        &self,
        id: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<crate::sftp::Transfer>, ApiError> {
        let closed = || ApiError::new(409, "state_conflict", "SSH session closed");
        let slot = self
            .lock_live_transfers()
            .await
            .ok_or_else(closed)?
            .get(id)
            .cloned()
            .ok_or_else(|| ApiError::new(404, "not_found", "transfer not found"))?;
        tokio::select! {
            biased;
            _ = self.cancelled() => Err(closed()),
            guard = slot.lock_owned() => {
                if *self.cancel.borrow() { Err(closed()) } else { Ok(guard) }
            }
        }
    }

    /// 等待当前挑战的回答，取消和超时都释放等待者。
    pub async fn ask(&self, kind: &str, payload: Value) -> Result<Value, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = uuid::Uuid::new_v4();
        let ttl = std::time::Duration::from_secs(120);
        let expires = tokio::time::Instant::now() + ttl;
        {
            let mut pending = self.challenge.lock().unwrap();
            if *self.cancel.borrow() {
                return Err("connection cancelled".into());
            }
            if pending.is_some() {
                return Err("another authentication challenge is pending".into());
            }
            *pending = Some(PendingChallenge {
                id,
                info: json!({"id":id,"kind":kind,"expires_at":crate::timefmt::format_rfc3339(std::time::SystemTime::now()+ttl),"payload":payload}),
                response: Some(tx),
                expires,
            });
        }
        let guard = ChallengeGuard {
            slot: &self.challenge,
            id,
        };
        self.transition("waiting_challenge");
        let answer = tokio::select! {
            biased;
            _ = self.cancelled() => Err("connection cancelled".into()),
            result = tokio::time::timeout_at(expires, rx) => match result {
                Ok(Ok(value)) => Ok(value),
                _ => Err("authentication challenge expired".into()),
            },
        };
        drop(guard);
        if answer.is_ok() {
            self.transition("authenticating");
        }
        answer
    }
    /// 获取当前权威快照。VNC 身份由浏览器原生握手确认，服务端保持 unknown。
    pub fn info(&self, product: &str) -> Value {
        let state = self.state.lock().unwrap();
        let base = format!(
            "/{product}/webclient/{}/api/v2/sessions/{}/",
            self.target.protocol, self.id
        );
        let available = !*self.cancel.borrow()
            && (self.target.protocol != "ssh"
                || state.0 == "active"
                || state.0 == "transport_ready");
        let mut features = match self.target.protocol.as_str() {
            "vnc" => vec!["vnc.rfb"],
            "rdp" => vec![
                "rdp.desktop",
                "rdp.input",
                "rdp.clipboard.text",
                "rdp.resize",
                "rdp.clipboard.files",
            ],
            _ => vec!["ssh.terminal"],
        };
        if self.sftp.try_lock().is_ok_and(|s| s.is_some()) {
            features.push("ssh.sftp");
        }
        json!({ "business_session_id":self.id, "capability":self.target.protocol, "target_id":self.target.id,
            "state":state.0,"state_revision":state.1.to_string(),"stream_available":available,
            "target_authentication":if self.target.protocol!="ssh"{"unknown"}else if available{"verified"}else if state.0=="failed"{"failed"}else{"pending"},"stream_profile":match self.target.protocol.as_str(){"vnc"=>"rfb.v1","rdp"=>"rdcleanpath.v1",_=>"terminal.v1"},
            "features":features,
            "event_url":format!("{base}events/"),"stream_url":if available {Some(format!("{base}stream/"))} else {None},
            "access_mode":"local","access_expires_at":crate::timefmt::format_rfc3339(self.owner.absolute_expires_at) })
    }
    /// 状态版本单调递增，关闭后晚到连接不能复活。
    pub fn transition(&self, name: &str) {
        let mut state = self.state.lock().unwrap();
        if matches!(state.0.as_str(), "closed" | "failed") || *self.cancel.borrow() {
            return;
        }
        state.0 = name.into();
        state.1 += 1;
        if matches!(name, "closed" | "failed") {
            self.cancel.send_replace(true);
        }
        let _ = self
            .events
            .send(json!({"state":name,"state_revision":state.1.to_string(),"stream_available":!*self.cancel.borrow()&&(self.target.protocol!="ssh"||name=="active"||name=="transport_ready"),"target_authentication":if self.target.protocol!="ssh"{"unknown"}else if name=="active"||name=="transport_ready"{"verified"}else if name=="failed"{"failed"}else{"pending"}}));
    }
    /// 幂等关闭所有关联通道。
    pub fn close(&self) {
        self.transition("closed");
    }
    /// 原生流只能连接一次；恢复需要显式创建新会话。
    pub fn attach(&self) -> Result<(), ApiError> {
        if self.target.protocol == "ssh"
            && !matches!(
                self.state.lock().unwrap().0.as_str(),
                "active" | "transport_ready"
            )
        {
            return Err(ApiError::new(
                409,
                "state_conflict",
                "authentication is not ready",
            ));
        }
        if *self.cancel.borrow() || self.attached.swap(true, Ordering::SeqCst) {
            return Err(ApiError::new(
                409,
                "conflict",
                "stream already used or closed",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{AccessMode, LocalPrincipal};

    #[test]
    fn credential_capacity_tracks_configured_sessions_and_preserves_owner_isolation() {
        for (configured, capacity) in [(16, 64), (96, 96), (2048, 1024)] {
            let hub = RemoteHub::default();
            let credential = || Credential {
                kind: "password".into(),
                secret: "temporary-test-secret".into(),
                passphrase: None,
                persist: false,
            };
            for _ in 0..capacity {
                let id = hub.credential(1, credential(), configured).unwrap();
                assert!(hub.resolve_credential(1, &id).is_ok());
                assert_eq!(
                    hub.resolve_credential(2, &id).err().unwrap().code,
                    "not_found"
                );
            }
            assert_eq!(
                hub.credential(1, credential(), configured)
                    .err()
                    .unwrap()
                    .code,
                "capacity_exceeded"
            );
        }
    }

    fn owner() -> AccessContext {
        let now = std::time::SystemTime::now();
        AccessContext {
            session_id: 1,
            principal: LocalPrincipal {
                principal_id: "test".into(),
                scopes: vec![],
            },
            mode: AccessMode::Cookie,
            csrf: "test".into(),
            expires_at: now + std::time::Duration::from_secs(3600),
            absolute_expires_at: now + std::time::Duration::from_secs(43200),
            created_at: now,
        }
    }

    fn create(
        hub: &RemoteHub,
        request: &str,
        limit: usize,
    ) -> Result<Arc<RemoteSession>, ApiError> {
        let target = serde_json::from_value(json!({
            "id":"test-vnc", "label":"Test", "protocol":"vnc", "host":"127.0.0.1", "port":5900
        }))
        .unwrap();
        hub.create(
            owner(),
            target,
            vec![],
            request.into(),
            "same-body".into(),
            limit,
        )
    }

    #[test]
    fn closed_history_does_not_block_new_connections_or_evict_active_sessions() {
        let hub = RemoteHub::default();
        let active = create(&hub, "active", 2).unwrap();
        for i in 0..1023 {
            create(&hub, &format!("closed-{i}"), 2).unwrap().close();
        }
        assert_eq!(hub.sessions.lock().unwrap().len(), 1024);
        let recent = create(&hub, "closed-1022", 2).unwrap();
        assert!(*recent.cancel.borrow());
        let next = create(&hub, "new", 2).unwrap();
        assert_eq!(hub.sessions.lock().unwrap().len(), 1024);
        assert!(Arc::ptr_eq(&active, &create(&hub, "active", 2).unwrap()));
        assert!(Arc::ptr_eq(&next, &create(&hub, "new", 2).unwrap()));
        assert!(Arc::ptr_eq(
            &recent,
            &create(&hub, "closed-1022", 2).unwrap()
        ));
        assert_eq!(
            create(&hub, "over-limit", 2).err().unwrap().code,
            "capacity_exceeded"
        );
        next.close();
        assert!(create(&hub, "after-close", 2).is_ok());
        assert!(hub.find(&owner(), active.id, "vnc").is_ok());
    }

    #[tokio::test]
    async fn busy_transfer_does_not_block_other_transfers_or_session_cancellation() {
        use sha2::Digest as _;
        let session = create(&RemoteHub::default(), "independent-transfers", 1).unwrap();
        {
            let mut table = session.transfers.0.lock().await;
            for id in ["busy", "other"] {
                table.insert(
                    id.into(),
                    Arc::new(tokio::sync::Mutex::new(crate::sftp::Transfer {
                        id: id.into(),
                        direction: "upload".into(),
                        path: id.into(),
                        temporary: None,
                        file: None,
                        offset: 0,
                        total: 0,
                        state: "active".into(),
                        version: String::new(),
                        hash: sha2::Sha256::new(),
                        last: None,
                    })),
                );
            }
        }
        let _busy = session.lock_live_transfer("busy").await.unwrap();
        let waiting = session.lock_live_transfer("busy");
        tokio::pin!(waiting);
        assert!(futures_util::poll!(&mut waiting).is_pending());
        let other = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            session.lock_live_transfer("other"),
        )
        .await
        .expect("busy transfer blocked unrelated transfer")
        .unwrap();
        assert_eq!(other.id, "other");
        session.close();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
                .await
                .expect("queued transfer ignored close")
                .err()
                .unwrap()
                .status,
            409
        );
    }

    #[tokio::test]
    async fn queued_transfer_returns_on_close_while_lock_remains_held() {
        let session = create(&RemoteHub::default(), "queued-transfer", 1).unwrap();
        let held = session.transfers.0.lock().await;
        let waiting = session.lock_live_transfers();
        tokio::pin!(waiting);
        assert!(futures_util::poll!(&mut waiting).is_pending());
        session.close();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("closed session still waits for transfer lock");
        assert!(result.is_none());
        drop(held);
        assert!(session.lock_live_transfers().await.is_none());
    }

    #[tokio::test]
    async fn native_failure_is_terminal_before_the_stream_finishes() {
        let session = create(&RemoteHub::default(), "terminal-failure", 1).unwrap();
        let mut events = session.events.subscribe();
        session.transition("failed");
        assert!(*session.cancel.borrow());
        assert!(!session.info("test")["stream_available"].as_bool().unwrap());
        let event = events.recv().await.unwrap();
        assert_eq!(event["state"], "failed");
        assert_eq!(event["stream_available"], false);
        session.close();
        session.transition("active");
        assert_eq!(session.info("test")["state"], "failed");
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn dropping_challenge_wait_clears_only_its_own_slot() {
        let session = create(&RemoteHub::default(), "abandoned-challenge", 1).unwrap();
        let first_id = {
            let wait = session.ask("certificate", json!({}));
            tokio::pin!(wait);
            assert!(futures_util::poll!(&mut wait).is_pending());
            assert!(session.ask("host_key", json!({})).await.is_err());
            let pending = session.challenge.lock().unwrap();
            pending.as_ref().unwrap().id
        };
        assert!(session.challenge.lock().unwrap().is_none());
        let wait = session.ask("host_key", json!({}));
        tokio::pin!(wait);
        assert!(futures_util::poll!(&mut wait).is_pending());
        assert_ne!(
            session.challenge.lock().unwrap().as_ref().unwrap().id,
            first_id
        );
        session.close();
        assert!(wait.await.is_err());
        assert!(session.challenge.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn cancelled_wait_and_challenge_observe_an_already_closed_session() {
        let hub = RemoteHub::default();
        let session = create(&hub, "cancel-before-subscribe", 1).unwrap();
        session.close();
        tokio::time::timeout(std::time::Duration::from_millis(50), session.cancelled())
            .await
            .unwrap();
        let answer = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            session.ask("credentials", json!({})),
        )
        .await
        .unwrap();
        assert!(answer.is_err());
        assert!(session.challenge.lock().unwrap().is_none());
    }
}
