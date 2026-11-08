//! 原生图形/终端桥：只转发已配置目标，独立于 Gateway，认证沿用本地访问权威。
use super::{
    auth::authenticate,
    response::{failure, require_request_id, success},
    AppState,
};
use crate::{
    access::{extract_ticket_candidate, AccessContext, TicketChannel},
    business::remote::RemoteSession,
    json::ApiError,
};
use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn protocol(value: &str) -> Result<(), ApiError> {
    if value == "ssh" {
        Ok(())
    } else {
        Err(ApiError::new(404, "not_found", "capability not found"))
    }
}
pub(super) fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    write: bool,
) -> Result<AccessContext, ApiError> {
    let context = authenticate(state, headers)?;
    context.require_scope("session.connect")?;
    if write {
        crate::access::require_exact_origin(
            headers.get("origin").and_then(|v| v.to_str().ok()),
            state.public_origin(),
        )?;
        require_request_id(headers.get("x-request-id"))?;
        let csrf = headers
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        state.access.require_csrf(&context, csrf)?;
    }
    Ok(context)
}
pub(super) fn reply(result: Result<Value, ApiError>, headers: &HeaderMap) -> Response {
    let id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("remote");
    match result {
        Ok(v) => success(v, id).into_response(),
        Err(e) => failure(&e, id).into_response(),
    }
}
pub(super) fn find(
    state: &AppState,
    headers: &HeaderMap,
    cap: &str,
    id: &str,
    write: bool,
) -> Result<Arc<RemoteSession>, ApiError> {
    protocol(cap)?;
    let context = authorize(state, headers, write)?;
    let id = uuid::Uuid::parse_str(id)
        .map_err(|_| ApiError::new(400, "invalid_request", "invalid session id"))?;
    state.business.remote.find(&context, id, cap)
}

pub async fn targets(
    State(state): State<Arc<AppState>>,
    Path(cap): Path<String>,
    headers: HeaderMap,
) -> Response {
    reply(
        (|| {
            protocol(&cap)?;
            let owner = authorize(&state, &headers, false)?;
            let items = state
                .business
                .remote
                .profiles(&state.config)
                .lock()
                .unwrap()
                .all();
            Ok(
                json!({"items":items.iter().filter(|t|t.protocol==cap && state.access.permits_target(&owner,&cap,&t.id)).map(|t|t.summary()).collect::<Vec<_>>(),"can_manage":authorize(&state,&headers,false)?.has_scope("targets.manage")}),
            )
        })(),
        &headers,
    )
}
pub async fn capabilities(
    State(state): State<Arc<AppState>>,
    Path(cap): Path<String>,
    headers: HeaderMap,
) -> Response {
    reply(
        (|| {
            protocol(&cap)?;
            authorize(&state, &headers, false)?;
            Ok(
                json!({"protocol_version":2,"stream_profile":match cap.as_str(){"vnc"=>"rfb.v1","rdp"=>"rdcleanpath.v1",_=>"terminal.v1"},"supported_features":match cap.as_str(){"vnc"=>vec!["vnc.rfb"],"rdp"=>vec!["rdp.desktop","rdp.input","rdp.clipboard.text","rdp.resize","rdp.clipboard.files"],_=>vec!["ssh.terminal","ssh.sftp"]},"limits":{"max_sessions":state.config.max_sessions,"frame_bytes":1048576},"media_status":"not_applicable"}),
            )
        })(),
        &headers,
    )
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    protocol_version: u8,
    target_id: String,
    #[serde(default)]
    credential_ref: Option<String>,
    options: Value,
    client_features: Vec<String>,
}
pub async fn create(
    State(state): State<Arc<AppState>>,
    Path(cap): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result = (|| {
        protocol(&cap)?;
        let owner = authorize(&state, &headers, true)?;
        // Opaque RFB 不由桥解析输入；权限不完整时拒绝建立，不能将前端只看当安全边界。
        owner.require_scope("input.pointer")?;
        owner.require_scope("input.keyboard")?;
        if cap != "ssh" {
            owner.require_scope("screen.view")?;
        }
        let value = crate::json::parse_json_strict(&body)?;
        let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()));
        let body: Create = serde_json::from_value(value)
            .map_err(|_| ApiError::new(400, "invalid_request", "invalid session body"))?;
        if !state.access.permits_target(&owner, &cap, &body.target_id) {
            return Err(ApiError::new(
                403,
                "forbidden",
                "target outside Connector authorization",
            ));
        }
        if body.protocol_version != 2
            || (cap != "ssh" && body.credential_ref.is_some())
            || !body.client_features.is_empty()
            || (!body.options.is_null() && body.options != json!({}))
        {
            return Err(ApiError::new(
                400,
                "unsupported_feature",
                "unsupported session options",
            ));
        }
        let configured = state
            .business
            .remote
            .profiles(&state.config)
            .lock()
            .unwrap()
            .all();
        let target = configured
            .iter()
            .find(|t| t.id == body.target_id && t.protocol == cap)
            .cloned()
            .ok_or_else(|| ApiError::new(404, "not_found", "target not configured"))?;
        let id = require_request_id(headers.get("x-request-id"))?;
        if let Some(ref id) = body.credential_ref {
            state
                .business
                .remote
                .resolve_credential(owner.session_id, id)?;
        }
        let jumps = target
            .jump_chain
            .iter()
            .map(|id| {
                configured
                    .iter()
                    .find(|t| t.id == *id)
                    .cloned()
                    .ok_or_else(|| {
                        ApiError::new(400, "invalid_request", "jump host not configured")
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let session = state.business.remote.create(
            owner,
            target,
            jumps,
            id,
            fingerprint,
            state.config.max_sessions,
        )?;
        if !session
            .preparing
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let monitored = session.clone();
            let access = state.access.clone();
            tokio::spawn(async move {
                let created = tokio::time::Instant::now();
                let mut tick = tokio::time::interval(Duration::from_secs(1));
                loop {
                    tick.tick().await;
                    if *monitored.cancel.borrow() {
                        break;
                    }
                    if access
                        .resolve(monitored.owner.session_id, monitored.owner.mode)
                        .is_err()
                        || (monitored.target.protocol != "ssh"
                            && !monitored.attached.load(std::sync::atomic::Ordering::SeqCst)
                            && created.elapsed() > Duration::from_secs(30))
                    {
                        monitored.close();
                        break;
                    }
                }
            });
            if cap == "ssh" {
                tokio::spawn(crate::ssh::prepare(
                    session.clone(),
                    state.clone(),
                    body.credential_ref,
                ));
            }
        }
        Ok(session.info(&state.config.product_id))
    })();
    let mut response = reply(result, &headers);
    if response.status() == StatusCode::OK {
        *response.status_mut() = StatusCode::CREATED;
    }
    response
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileWrite {
    #[serde(default)]
    domain: String,
    #[serde(default)]
    certificate_sha256: String,
    label: String,
    host: String,
    port: u16,
    #[serde(default)]
    username: String,
    #[serde(default)]
    host_key: String,
    #[serde(default = "profile_root")]
    root: String,
    #[serde(default)]
    jump_chain: Vec<String>,
    #[serde(default)]
    expected_revision: Option<String>,
}
fn profile_root() -> String {
    "/".into()
}
pub async fn create_target(
    State(state): State<Arc<AppState>>,
    Path(cap): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    mutate_target(state, cap, None, headers, body, false).await
}
pub async fn change_target(
    State(state): State<Arc<AppState>>,
    Path((cap, id)): Path<(String, String)>,
    headers: HeaderMap,
    method: axum::http::Method,
    body: Bytes,
) -> Response {
    mutate_target(
        state,
        cap,
        Some(id),
        headers,
        body,
        method == axum::http::Method::DELETE,
    )
    .await
}
async fn mutate_target(
    state: Arc<AppState>,
    cap: String,
    id: Option<String>,
    headers: HeaderMap,
    body: Bytes,
    delete: bool,
) -> Response {
    let result = async {
        protocol(&cap)?;
        authorize(&state, &headers, true)?.require_scope("targets.manage")?;
        let value = crate::json::parse_json_strict(&body)?;
        let created = id.is_none();
        let result = tokio::task::spawn_blocking(move || {
            let mut catalog = state
                .business
                .remote
                .profiles(&state.config)
                .lock()
                .unwrap();
            if let Some(ref id) = id {
                if !catalog
                    .all()
                    .iter()
                    .any(|t| t.id == *id && t.protocol == cap)
                {
                    return Err(ApiError::new(404, "not_found", "connection not found"));
                }
            }
            if delete {
                let id = id.as_deref().unwrap();
                let revision = value["expected_revision"].as_str().ok_or_else(|| {
                    ApiError::new(400, "invalid_request", "expected_revision required")
                })?;
                catalog.remove(id, revision)?;
                return Ok(None);
            }
            let body: ProfileWrite = serde_json::from_value(value)
                .map_err(|_| ApiError::new(400, "invalid_request", "invalid connection profile"))?;
            let target = crate::connections::ConnectionTarget {
                domain: body.domain.trim().into(),
                certificate_sha256: body
                    .certificate_sha256
                    .trim()
                    .replace(':', "")
                    .to_lowercase(),
                id: id.unwrap_or_else(|| format!("user-{}", uuid::Uuid::new_v4())),
                protocol: cap,
                label: body.label.trim().into(),
                host: body
                    .host
                    .trim()
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .into(),
                port: body.port,
                username: body.username.trim().into(),
                root: body.root,
                host_key: body.host_key.trim().into(),
                jump_chain: body.jump_chain,
            };
            catalog
                .put(target, body.expected_revision.as_deref())
                .map(Some)
        })
        .await
        .map_err(|_| ApiError::new(500, "internal", "connection update failed"))??;
        Ok((result, created))
    }
    .await;
    match result {
        Ok((Some(target), created)) => {
            let mut response = reply(
                Ok(serde_json::to_value(target.summary()).unwrap()),
                &headers,
            );
            if created {
                *response.status_mut() = StatusCode::CREATED;
            }
            response
        }
        Ok((None, _)) => super::response::no_content(),
        Err(error) => reply(Err(error), &headers),
    }
}
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path((cap, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    reply(
        find(&state, &headers, &cap, &id, false).map(|s| s.info(&state.config.product_id)),
        &headers,
    )
}
pub async fn close(
    State(state): State<Arc<AppState>>,
    Path((cap, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    match find(&state, &headers, &cap, &id, true) {
        Ok(s) => {
            s.close();
            super::response::no_content()
        }
        Err(e) => reply(Err(e), &headers),
    }
}
pub async fn ticket(
    State(state): State<Arc<AppState>>,
    Path((cap, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    reply(
        (|| {
            let s = find(&state, &headers, &cap, &id, true)?;
            let context = authorize(&state, &headers, true)?;
            let v = crate::json::parse_json_strict(&body)?;
            let channel = match v.get("channel").and_then(Value::as_str) {
                Some("stream") => TicketChannel::Stream,
                Some("events") => TicketChannel::Events,
                _ => {
                    return Err(ApiError::new(
                        400,
                        "invalid_request",
                        "invalid ticket channel",
                    ))
                }
            };
            let (ticket, expires) = state.access.issue_ticket(&context, channel)?;
            Ok(
                json!({"business_session_id":s.id,"channel":if channel==TicketChannel::Stream{"stream"}else{"events"},"ticket":ticket,"expires_at":crate::timefmt::format_rfc3339(expires)}),
            )
        })(),
        &headers,
    )
}
fn ws_auth(
    state: &AppState,
    headers: &HeaderMap,
    cap: &str,
    id: &str,
    channel: TicketChannel,
) -> Result<Arc<RemoteSession>, ApiError> {
    protocol(cap)?;
    crate::access::require_exact_origin(
        headers.get("origin").and_then(|v| v.to_str().ok()),
        state.public_origin(),
    )?;
    let protocols = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let (ticket, bad) = extract_ticket_candidate(protocols.split(',').map(str::trim));
    if bad {
        return Err(ApiError::new(401, "unauthenticated", "invalid ticket"));
    }
    let context = match ticket {
        Some(t) => state.access.consume_ticket(t, channel)?,
        None => authenticate(state, headers)?,
    };
    if headers.contains_key(axum::http::header::AUTHORIZATION)
        && authenticate(state, headers)?.session_id != context.session_id
    {
        return Err(ApiError::new(
            403,
            "forbidden",
            "stream ticket belongs to another access session",
        ));
    }
    let id = uuid::Uuid::parse_str(id)
        .map_err(|_| ApiError::new(400, "invalid_request", "invalid id"))?;
    state.business.remote.find(&context, id, cap)
}
pub async fn stream(
    State(state): State<Arc<AppState>>,
    Path((cap, id)): Path<(String, String)>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let session = match ws_auth(&state, &headers, &cap, &id, TicketChannel::Stream).and_then(|s| {
        s.attach()?;
        Ok(s)
    }) {
        Ok(s) => s,
        Err(e) => return reply(Err(e), &headers),
    };
    let failed_upgrade = session.clone();
    ws.on_failed_upgrade(move |_| failed_upgrade.close())
        .protocols([match cap.as_str() {
            "vnc" => "binary",
            "rdp" => "rdcleanpath.v1",
            _ => "terminal.v1",
        }])
        .max_message_size(1 << 20)
        .max_frame_size(1 << 20)
        .on_upgrade(move |socket| async move {
            crate::ssh::stream(socket, session.clone()).await;
            session.close();
        })
}

pub async fn credential(
    State(state): State<Arc<AppState>>,
    Path(cap): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    reply(
        (|| {
            if cap != "ssh" {
                return Err(ApiError::new(404, "not_found", "not found"));
            }
            let owner = authorize(&state, &headers, true)?;
            let value = crate::json::parse_json_strict(&body)?;
            let credential: crate::business::remote::Credential = serde_json::from_value(value)
                .map_err(|_| ApiError::new(400, "invalid_request", "invalid credential body"))?;
            let kind = credential.kind.clone();
            let id = state.business.remote.credential(
                owner.session_id,
                credential,
                state.config.max_sessions,
            )?;
            Ok(
                json!({"credential_ref":id,"kind":kind,"persist":false,"expires_at":crate::timefmt::format_rfc3339(std::time::SystemTime::now()+Duration::from_secs(300))}),
            )
        })(),
        &headers,
    )
}
/// 只读挑战摘要用于页面恢复；敏感回答永远不进入快照。
pub async fn challenges(
    State(state): State<Arc<AppState>>,
    Path((cap, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    reply(find(&state,&headers,&cap,&id,false).map(|s|json!({"challenge":s.challenge.lock().unwrap().as_ref().filter(|c| tokio::time::Instant::now() < c.expires && !*s.cancel.borrow()).map(|c|c.info.clone()),"error":s.error.lock().unwrap().clone(),"sftp_available":s.sftp.try_lock().is_ok_and(|s|s.is_some())})),&headers)
}
pub async fn answer(
    State(state): State<Arc<AppState>>,
    Path((cap, id, cid)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    reply(
        (|| {
            let s = find(&state, &headers, &cap, &id, true)?;
            let value = crate::json::parse_json_strict(&body)?;
            let mut pending = s.challenge.lock().unwrap();
            let c = pending
                .as_mut()
                .filter(|c| {
                    c.info["id"] == cid
                        && tokio::time::Instant::now() < c.expires
                        && !*s.cancel.borrow()
                })
                .ok_or_else(|| {
                    ApiError::new(409, "state_conflict", "challenge is no longer current")
                })?;
            if value["kind"] != c.info["kind"] || !value["answer"].is_object() {
                return Err(ApiError::new(
                    400,
                    "invalid_request",
                    "challenge answer mismatch",
                ));
            }
            if matches!(value["kind"].as_str(), Some("host_key" | "certificate"))
                && value["answer"]["decision"] == "save"
            {
                return Err(ApiError::new(
                    400,
                    "unsupported_feature",
                    "save the verified host fingerprint in server target configuration",
                ));
            }
            let tx = c.response.take().ok_or_else(|| {
                ApiError::new(409, "state_conflict", "challenge already answered")
            })?;
            tx.send(value["answer"].clone())
                .map_err(|_| ApiError::new(409, "state_conflict", "challenge expired"))?;
            Ok(json!({"accepted":true}))
        })(),
        &headers,
    )
}
pub async fn events(
    State(state): State<Arc<AppState>>,
    Path((cap, id)): Path<(String, String)>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let session = match ws_auth(&state, &headers, &cap, &id, TicketChannel::Events) {
        Ok(s) => s,
        Err(e) => return reply(Err(e), &headers),
    };
    let mut receiver = session.events.subscribe();
    ws.on_upgrade(move |mut socket|async move {
        let info=session.info(&state.config.product_id);
        let snapshot=json!({"v":2,"type":"session.snapshot","event_id":info["state_revision"],"business_session_id":session.id,"payload":info});
        if crate::http::send_ws_message(&mut socket, Message::Text(snapshot.to_string().into())).await.is_err(){return;}
        let mut revision = info["state_revision"].as_str().and_then(|value| value.parse::<u64>().ok()).unwrap_or(0);
        let terminal = |payload: &Value| matches!(payload["state"].as_str(), Some("closed" | "failed"));
        if !terminal(&info) {
            loop { tokio::select! {
                message = socket.recv() => match message {
                    Some(Ok(Message::Ping(p))) => {
                        if crate::http::send_ws_message(&mut socket, Message::Pong(p)).await.is_err() { break; }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                },
                event = receiver.recv() => {
                    let Ok(payload) = event else { break; };
                    let Some(next) = payload["state_revision"].as_str().and_then(|value| value.parse::<u64>().ok()) else { break; };
                    if next <= revision { continue; }
                    revision = next;
                    let done = terminal(&payload);
                    let event = json!({"v":2,"type":"session.state","event_id":payload["state_revision"],"business_session_id":session.id,"payload":payload});
                    if crate::http::send_ws_message(&mut socket, Message::Text(event.to_string().into())).await.is_err() || done { break; }
                }
            }}
        }
        let _=crate::http::send_ws_message(&mut socket, Message::Close(None)).await;
    })
}
