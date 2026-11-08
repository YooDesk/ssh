//! 本地访问身份：`local-key` 认证、preauth CSRF、Cookie/Bearer 两种访问模式。
//!
//! 契约来源：`docs/design/10-revision-v2.md` §3（R02）与 `docs/design/04-webservice.md` §1。
//!
//! 这是本机/局域网访问凭据，不是云账号，也不是目标 SSH/VNC 密码；WebService 不验证 Gateway JWT。
//! 关键不变量：
//! - 访问密钥只经 HTTPS JSON 或受控 loopback HTTP 提交，不写 URL/日志；
//! - Cookie 写操作必须同时通过精确 Origin 与 `X-CSRF-Token`；
//! - 过期/撤销是终态，不因随后的续期请求复活；
//! - 续期只轮换 Cookie/CSRF，不延长绝对期限。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::json::{codes, ApiError, ErrorDetails};
mod invitations;

/// 本地访问口令的最小长度，配置读取与认证初始化保持一致。
pub(crate) const MIN_ACCESS_KEY_LEN: usize = 6;

/// 访问模式：HTTPS Cookie 或内存 Bearer。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessMode {
    /// `__Host-` 前缀的 HttpOnly Cookie，仅 HTTPS。
    Cookie,
    /// 仅存浏览器内存的短期 token，`Authorization: Bearer`。
    Bearer,
}

impl AccessMode {
    /// 从查询/请求体解析；非法值明确拒绝，不做默认降级。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cookie" => Some(Self::Cookie),
            "bearer" => Some(Self::Bearer),
            _ => None,
        }
    }

    /// 本模式是否必须使用 HTTPS。
    pub fn requires_https(self) -> bool {
        // Bearer 也会经公网 LAN listener 传输，同样是 HTTPS 前提；
        // Cookie 的 `__Host-` 前缀本身要求 Secure。
        let _ = self;
        true
    }
}

/// 本地主体：WebService 自己的身份命名空间，与 Gateway 账号无关。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalPrincipal {
    /// 稳定的主体 id，只用于 WebService 内部资源归属。
    pub principal_id: String,
    /// 允许的业务 scope，写入响应供前端裁剪 UI。
    pub scopes: Vec<String>,
}

/// 访问策略配置。
#[derive(Debug, Clone)]
pub struct AccessPolicy {
    /// 产品 id，用于 Cookie 名称与路径前缀。
    pub product_id: String,
    /// 本机主体与允许 scope。
    pub principals: Vec<LocalPrincipal>,
    /// 空闲期限；续期重置。
    pub idle_ttl: Duration,
    /// 绝对期限；续期不延长。
    pub absolute_ttl: Duration,
    /// preauth 事务期限（设计为 120 秒）。
    pub preauth_ttl: Duration,
    /// 单次 stream/events 票据期限（设计为 30 秒）。
    pub ticket_ttl: Duration,
}

impl AccessPolicy {
    /// 依据产品 id 生成默认期限：空闲 1 小时、绝对 12 小时、preauth 120 秒、票据 30 秒。
    pub fn new(product_id: impl Into<String>, principals: Vec<LocalPrincipal>) -> Self {
        Self {
            product_id: product_id.into(),
            principals,
            idle_ttl: Duration::from_secs(3600),
            absolute_ttl: Duration::from_secs(12 * 3600),
            preauth_ttl: Duration::from_secs(120),
            ticket_ttl: Duration::from_secs(30),
        }
    }

    /// HTTPS Cookie 名称：`__Host-{product_id}_business_session`。
    pub fn session_cookie(&self) -> String {
        format!("__Host-{}_business_session", self.product_id)
    }

    /// loopback HTTP Cookie 名称，路径为产品前缀，不能用于 LAN。
    pub fn loopback_cookie(&self) -> String {
        format!("{}_loopback_session", self.product_id)
    }

    /// preauth Cookie 名称。
    pub fn preauth_cookie(&self) -> String {
        format!("__Host-{}_business_preauth", self.product_id)
    }
}

/// 时钟注入：测试可推进时间而不 sleep。
pub trait Clock: Send + Sync {
    /// 当前时间。
    fn now(&self) -> SystemTime;
}

/// 系统时钟。
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// 活动访问会话。
#[derive(Debug, Clone)]
struct RemoteAccess {
    binding: (String, String),
    capability: String,
    target: String,
    principal: LocalPrincipal,
    token: String,
    unlocked: bool,
}

#[derive(Debug, Clone)]
struct AccessSession {
    invitation: Option<invitations::Grant>,
    /// 会话凭据摘要；逻辑编号不是认证凭据。
    token_digest: [u8; 32],
    principal_index: usize,
    remote: Option<RemoteAccess>,
    mode: AccessMode,
    /// 会话建立时刻，供后续审计与“创建于”展示。
    created: SystemTime,
    absolute_deadline: SystemTime,
    idle_deadline: SystemTime,
    csrf: String,
    revoked: bool,
}

/// 一次性 preauth 事务。
#[derive(Debug, Clone)]
struct Preauth {
    csrf: String,
    mode: AccessMode,
    expires: SystemTime,
    consumed: bool,
}

/// 一次性 WS 票据（stream/events）。
#[derive(Debug, Clone)]
struct Ticket {
    session_id: u64,
    channel: TicketChannel,
    expires: SystemTime,
    consumed: bool,
}

/// 票据适用通道。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TicketChannel {
    /// 二进制业务流。
    Stream,
    /// 类型化事件流。
    Events,
}

impl TicketChannel {
    /// 解析票据通道；未知值拒绝。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "stream" => Some(Self::Stream),
            "events" => Some(Self::Events),
            _ => None,
        }
    }
}

/// 认证后的请求身份，随请求传递到业务层。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessContext {
    /// 逻辑会话 id，用于查找与撤销；不是 Gateway 的 remote session_id。
    pub session_id: u64,
    /// 本地主体。
    pub principal: LocalPrincipal,
    /// 访问模式。
    pub mode: AccessMode,
    /// 当前 CSRF（Cookie 模式写操作需要回传）。
    pub csrf: String,
    /// 空闲截止时间。
    pub expires_at: SystemTime,
    /// 绝对截止时间。
    pub absolute_expires_at: SystemTime,
    /// 会话建立时刻。
    pub created_at: SystemTime,
}

impl AccessContext {
    /// 判断当前身份是否具备某 scope。
    pub fn has_scope(&self, scope: &str) -> bool {
        self.principal.scopes.iter().any(|s| s == scope)
    }

    /// 写操作约束：`input.pointer` 等 scope 必须存在。
    pub fn require_scope(&self, scope: &str) -> Result<(), ApiError> {
        if self.has_scope(scope) {
            Ok(())
        } else {
            Err(ApiError::new(
                403,
                codes::FORBIDDEN,
                format!("missing scope {scope}"),
            ))
        }
    }
}

/// 访问权威：本地密钥校验、会话/票据生命周期。
///
/// 所有内部状态在一个 `Mutex` 之后；临界区只做内存操作，不等待 I/O。
pub struct AccessAuthority {
    remote: Option<crate::remote_access::RemoteAccess>,
    policy: AccessPolicy,
    /// 访问密钥的业务副本，只保存 SHA-256 摘要。
    key_digest: Mutex<[u8; 32]>,
    clock: Arc<dyn Clock>,
    state: Mutex<AuthorityState>,
    bootstrap_limiter: LoginLimiter,
}

#[derive(Default)]
struct AuthorityState {
    invitation_path: Option<std::path::PathBuf>,
    invitations: Vec<invitations::Record>,
    next_session_id: u64,
    sessions: HashMap<u64, AccessSession>,
    preauths: HashMap<String, Preauth>,
    tickets: HashMap<String, Ticket>,
}

/// 登录成功后返回给浏览器的最小集合。
#[derive(Debug, Clone)]
pub struct LoginResult {
    /// 公开的非凭据会话实例标识，仅用于绑定客户端意图。
    pub instance_id: String,
    /// Cookie 模式：需设置的会话 Cookie 值。
    pub cookie: Option<String>,
    /// Bearer 模式：仅存内存的访问 token。
    pub access_token: Option<String>,
    /// 当前访问信息（响应 DTO 的主体部分）。
    pub info: AccessSessionInfo,
}

/// `AccessInfo` DTO 中不随模式变化的部分。
#[derive(Debug, Clone, Serialize)]
pub struct AccessSessionInfo {
    /// 本地主体 id。
    pub principal_id: String,
    /// 该主体可用的业务 scope。
    pub scopes: Vec<String>,
    /// 空闲截止时间。
    #[serde(serialize_with = "crate::timefmt::system_time_as_rfc3339")]
    pub expires_at: SystemTime,
    /// 绝对截止时间。
    #[serde(serialize_with = "crate::timefmt::system_time_as_rfc3339")]
    pub absolute_expires_at: SystemTime,
    /// 当前 CSRF token。
    pub csrf_token: String,
    /// 认证模式，固定 `local-key`。
    pub auth_mode: &'static str,
    /// 访问模式。
    pub access_mode: AccessMode,
    /// Bearer 模式的访问 token；Cookie 模式不返回。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
}

impl AccessAuthority {
    pub fn from_host(
        policy: AccessPolicy,
        api: crate::plugin_abi::HostApi,
    ) -> Result<Self, ApiError> {
        let mut authority = Self::new(policy, "plugin-unused-access-key", Arc::new(SystemClock))?;
        authority.remote = Some(crate::remote_access::RemoteAccess(api));
        Ok(authority)
    }

    /// 用明文访问密钥构造；密钥只保留摘要。
    pub fn new(
        policy: AccessPolicy,
        access_key: &str,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ApiError> {
        if access_key.len() < MIN_ACCESS_KEY_LEN {
            return Err(ApiError::new(
                500,
                codes::INTERNAL,
                "access key must be at least 6 characters",
            ));
        }
        Ok(Self {
            policy,
            key_digest: Mutex::new(digest(access_key)),
            clock,
            state: Mutex::new(AuthorityState::default()),
            remote: None,
            bootstrap_limiter: LoginLimiter::new(120.0, 30.0),
        })
    }

    /// 访问策略只读视图。
    pub fn policy(&self) -> &AccessPolicy {
        &self.policy
    }

    /// 生成 preauth 事务：返回 (ticket, csrf)。
    ///
    /// Cookie 模式由服务端设置 preauth Cookie 保存 ticket；Bearer 模式把 ticket 直接返回给前端
    /// （仅存内存），前端登录时以 `X-Preauth-Token` 回传。
    pub fn allow_bootstrap(&self) -> bool {
        // 全局预算同时限制多来源地址的匿名洪泛。
        self.bootstrap_limiter
            .allow(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
    }

    /// 生成 preauth；容量耗尽时返回可重试的 429，不驱逐其他客户端。
    pub fn begin_preauth(&self, mode: AccessMode) -> Result<(String, String), ApiError> {
        let ticket = random_token(32);
        let csrf = random_token(24);
        let mut state = self.lock_state();
        let now = self.clock.now();
        state
            .preauths
            .retain(|_, entry| now < entry.expires && !entry.consumed);
        if state.preauths.len() >= MAX_PREAUTHS {
            return Err(capacity_error("too many pending logins"));
        }
        state.preauths.insert(
            ticket.clone(),
            Preauth {
                csrf: csrf.clone(),
                mode,
                expires: self.clock.now() + self.policy.preauth_ttl,
                consumed: false,
            },
        );
        Ok((ticket, csrf))
    }

    /// 校验 preauth 的 CSRF，不消费事务（登录失败可重试）。
    pub fn check_preauth_csrf(&self, ticket: &str, csrf: &str) -> Result<(), ApiError> {
        let now = self.clock.now();
        let state = self.lock_state();
        let entry = state
            .preauths
            .get(ticket)
            .ok_or_else(|| ApiError::new(401, codes::UNAUTHENTICATED, "preauth expired"))?;
        if entry.consumed || now >= entry.expires {
            return Err(ApiError::new(
                401,
                codes::UNAUTHENTICATED,
                "preauth expired",
            ));
        }
        if !constant_time_eq(entry.csrf.as_bytes(), csrf.as_bytes()) {
            return Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "invalid csrf token",
            ));
        }
        Ok(())
    }

    /// 校验访问密钥；比较摘要，避免长度/内容时间侧信道。
    pub fn verify_key(&self, credential: &str) -> bool {
        let provided = digest(credential);
        provided.ct_eq(&*self.key_digest.lock().unwrap()).into()
    }

    pub(crate) fn replace_key(&self, password: &str) -> Vec<u64> {
        let mut key = self.key_digest.lock().unwrap();
        *key = digest(password);
        let mut state = self.lock_state();
        state.invitations.clear();
        state.sessions.drain().map(|(id, _)| id).collect()
    }

    pub(crate) fn revoke_connector_protocol(&self, protocol: &str) -> Vec<u64> {
        let mut state = self.lock_state();
        let mut revoked = Vec::new();
        state.sessions.retain(|id, session| {
            let matches = session
                .remote
                .as_ref()
                .is_some_and(|remote| remote.capability == protocol);
            if matches {
                revoked.push(*id);
            }
            !matches
        });
        revoked
    }

    /// 登录：校验 preauth + 密钥，原子消费 preauth 并建立会话。
    pub fn login(
        &self,
        ticket: &str,
        csrf: &str,
        credential: &str,
        mode: AccessMode,
    ) -> Result<LoginResult, ApiError> {
        self.check_preauth_csrf(ticket, csrf)?;
        let key = self.key_digest.lock().unwrap();
        let now = self.clock.now();
        let mut state = self.lock_state();
        let invitation = if bool::from(digest(credential).ct_eq(&*key)) {
            None
        } else {
            invitations::lookup(&state.invitations, credential, now)
        };
        if !bool::from(digest(credential).ct_eq(&*key)) && invitation.is_none() {
            // 密钥错误不消费 preauth，避免一次错误就让用户重开页面；
            // 但也不区分“密钥错”和“主体不存在”。
            return Err(ApiError::new(
                401,
                codes::UNAUTHENTICATED,
                "invalid credential",
            ));
        }
        let principal = self
            .policy
            .principals
            .first()
            .cloned()
            .ok_or_else(|| ApiError::new(403, codes::FORBIDDEN, "no local principal"))?;
        {
            let entry = state
                .preauths
                .get_mut(ticket)
                .ok_or_else(|| ApiError::new(401, codes::UNAUTHENTICATED, "preauth expired"))?;
            if entry.consumed || now >= entry.expires {
                return Err(ApiError::new(
                    401,
                    codes::UNAUTHENTICATED,
                    "preauth expired",
                ));
            }
            if entry.mode != mode {
                return Err(ApiError::new(
                    400,
                    codes::INVALID_REQUEST,
                    "access mode does not match preauth",
                ));
            }
        }
        state.sessions.retain(|_, session| {
            !session.revoked && now < session.idle_deadline && now < session.absolute_deadline
        });
        state
            .tickets
            .retain(|_, ticket| now < ticket.expires && !ticket.consumed);
        if state.sessions.len() >= MAX_ACCESS_SESSIONS {
            return Err(capacity_error("too many access sessions"));
        }
        state.preauths.remove(ticket);
        state.next_session_id += 1;
        let session_id = state.next_session_id;
        let csrf_token = random_token(24);
        let token = session_token(session_id);
        let absolute_deadline = invitation
            .as_ref()
            .map_or(now + self.policy.absolute_ttl, |grant| {
                min_time(now + self.policy.absolute_ttl, grant.expires)
            });
        let idle_deadline = min_time(now + self.policy.idle_ttl, absolute_deadline);
        let token_digest = Sha256::digest(token.as_bytes()).into();
        let instance_id = instance_from_digest(&token_digest);
        state.sessions.insert(
            session_id,
            AccessSession {
                invitation,
                token_digest,
                principal_index: 0,
                remote: None,
                mode,
                created: now,
                absolute_deadline,
                idle_deadline,
                csrf: csrf_token.clone(),
                revoked: false,
            },
        );
        Ok(LoginResult {
            instance_id,
            cookie: Some(token.clone()),
            access_token: Some(token),
            info: AccessSessionInfo {
                principal_id: principal.principal_id.clone(),
                scopes: principal.scopes.clone(),
                expires_at: idle_deadline,
                absolute_expires_at: absolute_deadline,
                csrf_token,
                auth_mode: "local-key",
                access_mode: mode,
                access_token: None,
            },
        })
    }

    /// 非凭据实例标识；不暴露原token或其存储校验摘要。
    pub(crate) fn connector_access(
        &self,
        context: crate::backend_context::Context,
    ) -> Result<(String, String), ApiError> {
        let now = self.clock.now();
        let deadline = SystemTime::UNIX_EPOCH + Duration::from_secs(context.expires);
        if deadline <= now
            || !context
                .scopes
                .iter()
                .any(|scope| scope == "session.connect")
        {
            return Err(unauthorized());
        }
        let mut state = self.lock_state();
        let binding = (context.session_id, context.generation);
        if let Some(session) = state.sessions.values_mut().find(|session| {
            session
                .remote
                .as_ref()
                .is_some_and(|remote| remote.binding == binding)
        }) {
            let remote = session.remote.as_ref().unwrap();
            if session.revoked
                || session
                    .invitation
                    .as_ref()
                    .is_some_and(|grant| now >= grant.expires)
                || remote.capability != context.capability
                || remote.target != context.target_id
                || remote.principal.scopes != context.scopes
                || deadline < session.absolute_deadline
            {
                return Err(unauthorized());
            }
            session.absolute_deadline = deadline;
            session.idle_deadline = deadline;
            return Ok((remote.token.clone(), session.csrf.clone()));
        }
        state.sessions.retain(|_, session| {
            !session.revoked && now < session.absolute_deadline && now < session.idle_deadline
        });
        if state.sessions.len() >= MAX_ACCESS_SESSIONS {
            return Err(capacity_error("too many access sessions"));
        }
        state.next_session_id += 1;
        let id = state.next_session_id;
        let token = session_token(id);
        let csrf = random_token(24);
        let principal = LocalPrincipal {
            principal_id: format!("connector:{}", binding.0),
            scopes: context.scopes,
        };
        state.sessions.insert(
            id,
            AccessSession {
                invitation: None,
                token_digest: Sha256::digest(token.as_bytes()).into(),
                principal_index: 0,
                mode: AccessMode::Bearer,
                created: now,
                absolute_deadline: deadline,
                idle_deadline: deadline,
                csrf: csrf.clone(),
                revoked: false,
                remote: Some(RemoteAccess {
                    binding,
                    capability: context.capability,
                    target: context.target_id,
                    principal,
                    token: token.clone(),
                    unlocked: false,
                }),
            },
        );
        Ok((token, csrf))
    }

    pub(crate) fn permits_target(
        &self,
        context: &AccessContext,
        capability: &str,
        target: &str,
    ) -> bool {
        if let Some(remote) = &self.remote {
            return remote.call(serde_json::json!({"op":"target.permitted", "context":context,"capability":capability,"target":target})).unwrap_or(false);
        }

        self.lock_state()
            .sessions
            .get(&context.session_id)
            .is_some_and(|session| {
                session.remote.as_ref().is_none_or(|remote| {
                    remote.capability == capability
                        && (remote.target == target
                            || matches!(capability, "vnc" | "ssh" | "rdp") && remote.target == "*")
                })
            })
    }

    pub(crate) fn connector_unlocked(&self, token: &str) -> bool {
        Self::parse_session_token(token)
            .and_then(|id| {
                self.lock_state()
                    .sessions
                    .get(&id)
                    .and_then(|session| session.remote.as_ref().map(|remote| remote.unlocked))
            })
            .unwrap_or(false)
    }

    pub(crate) fn is_connector_access(&self, context: &AccessContext) -> bool {
        if let Some(remote) = &self.remote {
            return remote
                .call(serde_json::json!({"op":"connector.session", "context":context}))
                .unwrap_or(false);
        }

        self.lock_state()
            .sessions
            .get(&context.session_id)
            .is_some_and(|session| session.remote.is_some())
    }

    /// Returns the non-secret identifier for an authenticated access instance.
    pub fn instance_id(&self, context: &AccessContext) -> Result<String, ApiError> {
        if let Some(remote) = &self.remote {
            return remote.call(serde_json::json!({"op":"instance", "context":context}));
        }

        let state = self.lock_state();
        let session = state
            .sessions
            .get(&context.session_id)
            .ok_or_else(unauthorized)?;
        Ok(instance_from_digest(&session.token_digest))
    }

    /// 已验证票据或内部编号查找；公开 Cookie/Bearer 必须先走 authenticate_token。
    /// 校验模式、空闲期限与绝对期限；读取不延长期限。
    pub fn resolve(&self, session_id: u64, mode: AccessMode) -> Result<AccessContext, ApiError> {
        if let Some(remote) = &self.remote {
            return remote.call(serde_json::json!({"op":"resolve", "id":session_id, "mode":mode}));
        }

        let now = self.clock.now();
        let state = self.lock_state();
        let session = state.sessions.get(&session_id).ok_or_else(unauthorized)?;
        if session.revoked
            || session
                .invitation
                .as_ref()
                .is_some_and(|grant| now >= grant.expires)
            || session.mode != mode
            || now >= session.idle_deadline
            || now >= session.absolute_deadline
        {
            return Err(unauthorized());
        }
        let principal = match &session.remote {
            Some(remote) => remote.principal.clone(),
            None => self
                .policy
                .principals
                .get(session.principal_index)
                .cloned()
                .ok_or_else(unauthorized)?,
        };
        Ok(AccessContext {
            session_id,
            principal,
            mode: session.mode,
            csrf: session.csrf.clone(),
            expires_at: session
                .invitation
                .as_ref()
                .map_or(session.idle_deadline, |grant| {
                    min_time(session.idle_deadline, grant.expires)
                }),
            absolute_expires_at: session
                .invitation
                .as_ref()
                .map_or(session.absolute_deadline, |grant| {
                    min_time(session.absolute_deadline, grant.expires)
                }),
            created_at: session.created,
        })
    }

    /// 仅解析编号和凭据格式；对外认证还必须验证随机凭据摘要。
    pub fn parse_session_token(value: &str) -> Option<u64> {
        let (raw, proof) = value.strip_prefix(SESSION_PREFIX)?.split_once('.')?;
        if raw.len() != 32
            || !raw.bytes().all(|b| b.is_ascii_hexdigit())
            || proof.len() != 64
            || !proof.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return None;
        }
        u64::from_str_radix(raw, 16).ok().filter(|id| *id != 0)
    }

    /// Cookie/Bearer 必须拥有登录时生成的 256 位随机凭据，不能只凭猜测编号进入会话。
    pub fn authenticate_token(
        &self,
        value: &str,
        mode: AccessMode,
    ) -> Result<AccessContext, ApiError> {
        if let Some(remote) = &self.remote {
            return remote
                .call(serde_json::json!({"op":"authenticate", "token":value, "mode":mode}));
        }

        let id = Self::parse_session_token(value).ok_or_else(unauthorized)?;
        {
            let state = self.lock_state();
            let session = state.sessions.get(&id).ok_or_else(unauthorized)?;
            let digest: [u8; 32] = Sha256::digest(value.as_bytes()).into();
            if !bool::from(session.token_digest.ct_eq(&digest)) {
                return Err(unauthorized());
            }
        }
        self.resolve(id, mode)
    }

    /// 续期空闲期限并轮换 CSRF；不延长绝对期限。
    pub fn renew(&self, context: &AccessContext) -> Result<(SystemTime, String), ApiError> {
        let now = self.clock.now();
        let mut state = self.lock_state();
        let session = state
            .sessions
            .get_mut(&context.session_id)
            .ok_or_else(unauthorized)?;
        if session.revoked
            || session
                .invitation
                .as_ref()
                .is_some_and(|grant| now >= grant.expires)
            || now >= session.idle_deadline
            || now >= session.absolute_deadline
        {
            return Err(unauthorized());
        }
        let csrf = random_token(24);
        let deadline = session
            .invitation
            .as_ref()
            .map_or(session.absolute_deadline, |grant| {
                min_time(session.absolute_deadline, grant.expires)
            });
        session.idle_deadline = min_time(now + self.policy.idle_ttl, deadline);
        session.csrf = csrf.clone();
        Ok((session.idle_deadline, csrf))
    }

    /// 写操作的 CSRF 校验。
    ///
    /// Cookie 模式必须回传当前 CSRF；Bearer 模式本身不是浏览器自动附带凭据，
    /// 仍要求 CSRF 头存在以保持“所有写操作显式”的一致语义（设计 §3）。
    pub fn require_csrf(&self, context: &AccessContext, provided: &str) -> Result<(), ApiError> {
        if context.csrf.is_empty() {
            return Err(unauthorized());
        }
        if constant_time_eq(context.csrf.as_bytes(), provided.as_bytes()) {
            Ok(())
        } else {
            Err(
                ApiError::new(400, codes::INVALID_REQUEST, "invalid csrf token")
                    .retryable(true)
                    .with_details(ErrorDetails {
                        stage: Some("csrf".into()),
                        field: Some("x-csrf-token".into()),
                        ..Default::default()
                    }),
            )
        }
    }

    /// 退出当前浏览器访问会话，并撤销其创建的一次性票据。
    ///
    /// 只撤销这一个会话，不影响其他浏览器或远程主体（设计 §3）。
    pub fn logout(&self, context: &AccessContext) {
        let mut state = self.lock_state();
        state.sessions.remove(&context.session_id);
        state
            .tickets
            .retain(|_, ticket| ticket.session_id != context.session_id);
    }

    /// 签发单次 stream/events 票据。
    pub fn issue_ticket(
        &self,
        context: &AccessContext,
        channel: TicketChannel,
    ) -> Result<(String, SystemTime), ApiError> {
        if let Some(remote) = &self.remote {
            return remote.call(serde_json::json!({"op":"ticket.issue", "context":context, "channel":match channel {TicketChannel::Stream=>"stream",TicketChannel::Events=>"events"}}));
        }

        let now = self.clock.now();
        let ticket = ticket_token();
        let expires = now + self.policy.ticket_ttl;
        let mut state = self.lock_state();
        // 先清理已过期票据，保证容量只受活跃票据影响。
        state.tickets.retain(|_, t| now < t.expires && !t.consumed);
        if state.tickets.len() >= MAX_TICKETS {
            return Err(capacity_error("too many pending tickets"));
        }
        state.tickets.insert(
            ticket.clone(),
            Ticket {
                session_id: context.session_id,
                channel,
                expires,
                consumed: false,
            },
        );
        Ok((ticket, expires))
    }

    /// 原子消费票据并校验会话、通道。
    pub fn consume_ticket(
        &self,
        ticket: &str,
        channel: TicketChannel,
    ) -> Result<AccessContext, ApiError> {
        if let Some(remote) = &self.remote {
            return remote.call(serde_json::json!({"op":"ticket.consume", "token":ticket, "channel":match channel {TicketChannel::Stream=>"stream",TicketChannel::Events=>"events"}}));
        }

        if !ticket.starts_with(TICKET_PREFIX) {
            return Err(unauthorized());
        }
        let now = self.clock.now();
        let mut state = self.lock_state();
        let entry = state.tickets.get_mut(ticket).ok_or_else(unauthorized)?;
        if entry.consumed || now >= entry.expires || entry.channel != channel {
            return Err(unauthorized());
        }
        entry.consumed = true;
        let session_id = entry.session_id;
        state.tickets.remove(ticket);
        drop(state);
        // 票据只证明“这是本条会话发起的一次性授权”，仍按当前会话状态复核。
        let mode = self.session_mode(session_id).ok_or_else(unauthorized)?;
        self.resolve(session_id, mode)
    }

    /// 会话当前访问模式；已撤销或不存在返回 `None`。
    fn session_mode(&self, session_id: u64) -> Option<AccessMode> {
        let state = self.lock_state();
        state
            .sessions
            .get(&session_id)
            .filter(|s| !s.revoked)
            .map(|s| s.mode)
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, AuthorityState> {
        // 临界区只有内存操作，不会因为 panic 之外的路径中毒；中毒视为内部错误。
        self.state.lock().unwrap_or_else(|err| err.into_inner())
    }
}

/// 票据和会话 token 的统一前缀，便于区分两类值。
const MAX_PREAUTHS: usize = 4096;
const MAX_ACCESS_SESSIONS: usize = 4096;
const MAX_TICKETS: usize = 8192;

fn capacity_error(message: &str) -> ApiError {
    ApiError::new(429, codes::CAPACITY_EXCEEDED, message).retryable(true)
}

/// 票据和会话 token 的统一前缀，便于区分两类值。
const SESSION_PREFIX: &str = "raccess.s.";
const TICKET_PREFIX: &str = "raccess.t.";

/// 会话 token 的字符串形式。
fn session_token(session_id: u64) -> String {
    format!("{SESSION_PREFIX}{session_id:032x}.{}", random_token(32))
}

/// 票据 token 的字符串形式：`raccess.t.<64 hex>`。
///
/// 前端把它作为 `raccess.ticket.<opaque>` 候选送回握手，服务端只做一次性消费，
/// 不把票据放进 URL 或日志。
fn ticket_token() -> String {
    format!("{TICKET_PREFIX}{}", random_token(32))
}

/// 设计要求的 WS 票候选前缀 `raccess.ticket.<opaque>`。
pub const WS_TICKET_SUBPROTOCOL_PREFIX: &str = "raccess.ticket.";

/// 从 `Sec-WebSocket-Protocol` 候选中提取票据值。
///
/// 返回 `(ticket, bad)`：`bad=true` 表示客户端确实提交了票据候选但格式非法，
/// 此时禁止回退 Cookie 模式（设计 §3）。
pub fn extract_ticket_candidate<'a, I>(protocols: I) -> (Option<&'a str>, bool)
where
    I: IntoIterator<Item = &'a str>,
{
    let mut found = None;
    let mut bad = false;
    for protocol in protocols {
        if let Some(rest) = protocol.strip_prefix(WS_TICKET_SUBPROTOCOL_PREFIX) {
            if found.is_some() || rest.is_empty() || rest.len() > 4096 {
                // 同时出现多个候选或空值都视为非法：不能猜测客户端意图，
                // 也不能因为一个坏候选回退 Cookie 认证。
                bad = true;
            } else {
                found = Some(rest);
            }
        }
    }
    if bad {
        return (None, true);
    }
    (found, bad)
}

fn unauthorized() -> ApiError {
    ApiError::new(401, codes::UNAUTHENTICATED, "access session required")
}

fn digest(value: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher.finalize().into()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

fn min_time(a: SystemTime, b: SystemTime) -> SystemTime {
    if a < b {
        a
    } else {
        b
    }
}

/// 实例标识经过域分隔的再次摘要，不可用作Cookie/Bearer凭据。
fn instance_from_digest(digest: &[u8; 32]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"yoodesk/access-instance/v1\0");
    hash.update(digest);
    format!("{:x}", hash.finalize())
}

/// 生成高熵随机 token（十六进制字符串）。
fn random_token(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 认证失败限流：按来源地址的简单令牌桶，失败才消耗配额。
///
/// 设计 §3 要求“认证失败限流”。这里只限制本进程内存，不引入外部依赖。
pub struct LoginLimiter {
    inner: Mutex<HashMap<IpAddr, Bucket>>,
    capacity: f64,
    refill_per_second: f64,
}

struct Bucket {
    tokens: f64,
    updated: std::time::Instant,
}

/// 验证前预占失败额度；只有认证成功才归还，取消/错误退出仍计作失败。
pub(crate) struct LoginAttempt<'a> {
    limiter: &'a LoginLimiter,
    peer: IpAddr,
}

impl LoginAttempt<'_> {
    /// 只有完成认证的调用方才能归还本次预占，消费self防止重复归还。
    pub(crate) fn succeed(self) {
        let mut map = self
            .limiter
            .inner
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if let Some(bucket) = map.get_mut(&self.peer) {
            bucket.tokens = (bucket.tokens + 1.0).min(self.limiter.capacity);
        }
    }
}

impl LoginLimiter {
    /// 验证前预占一个令牌；失败、取消或提前返回时不归还。
    pub(crate) fn begin_attempt(&self, peer: IpAddr) -> Option<LoginAttempt<'_>> {
        self.allow(peer).then_some(LoginAttempt {
            limiter: self,
            peer,
        })
    }
    /// 默认每分钟 10 次失败尝试。
    pub fn new(capacity: f64, refill_per_second: f64) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            capacity,
            refill_per_second,
        }
    }

    /// 消耗一次尝试额度；超额返回 `false`。
    pub fn allow(&self, peer: IpAddr) -> bool {
        let now = std::time::Instant::now();
        let mut map = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        // 限制表大小，避免被大量来源地址撑满。
        if map.len() >= 4096 && !map.contains_key(&peer) {
            map.retain(|_, bucket| {
                bucket.tokens
                    + now.duration_since(bucket.updated).as_secs_f64() * self.refill_per_second
                    < self.capacity
            });
            if map.len() >= 4096 {
                return false;
            }
        }
        let bucket = map.entry(peer).or_insert(Bucket {
            tokens: self.capacity,
            updated: now,
        });
        let elapsed = now.duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_second).min(self.capacity);
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// 要求 Cookie 写操作的精确 Origin。
///
/// 返回 `Ok(())` 仅当 `origin` 与服务端配置的公开来源完全一致。
/// 无 Origin（原生程序）只能使用受控 loopback 入口，不能借此绕过浏览器检查。
pub fn require_exact_origin(origin: Option<&str>, expected: &str) -> Result<(), ApiError> {
    match origin {
        Some(value) if value == expected => Ok(()),
        Some(_) => Err(ApiError::new(403, codes::FORBIDDEN, "origin not allowed")),
        None => Err(ApiError::new(
            403,
            codes::FORBIDDEN,
            "Origin header required",
        )),
    }
}

/// 把鉴权失败映射为带 `WWW-Authenticate` 语义的字段细节（便于前端区分）。
pub fn auth_details(hint: &str) -> ErrorDetails {
    ErrorDetails {
        hint: Some(hint.to_string()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 链接和临时密码共享期限，已登录会话及续期也不能越过邀请截止时间。
    #[test]
    fn invitation_credentials_share_deadline_and_bound_sessions() {
        let (auth, clock) = authority();
        let invite = auth
            .create_invitation(10, vec!["desk".into(), "ssh".into()])
            .unwrap();
        assert_eq!(invite.temporary_password.len(), 8);
        assert!(invite
            .temporary_password
            .bytes()
            .any(|b| b.is_ascii_digit()));
        assert!(invite
            .temporary_password
            .bytes()
            .any(|b| b.is_ascii_lowercase()));
        let (pre, csrf) = auth.begin_preauth(AccessMode::Cookie).unwrap();
        let signed = auth
            .login(&pre, &csrf, &invite.token, AccessMode::Cookie)
            .unwrap();
        let context = auth
            .authenticate_token(signed.cookie.as_ref().unwrap(), AccessMode::Cookie)
            .unwrap();
        assert!(auth.invitation_allows(&context, "desk"));
        assert!(!auth.invitation_allows(&context, "rdp"));
        let (pre, csrf) = auth.begin_preauth(AccessMode::Cookie).unwrap();
        assert!(auth
            .login(&pre, &csrf, &invite.temporary_password, AccessMode::Cookie)
            .is_ok());
        clock.advance(10);
        let (pre, csrf) = auth.begin_preauth(AccessMode::Cookie).unwrap();
        assert!(auth
            .login(&pre, &csrf, &invite.token, AccessMode::Cookie)
            .is_err());
        assert!(auth
            .login(&pre, &csrf, &invite.temporary_password, AccessMode::Cookie)
            .is_err());
        assert!(auth
            .authenticate_token(signed.cookie.as_ref().unwrap(), AccessMode::Cookie)
            .is_err());
        assert!(auth.renew(&context).is_err());
        assert!(auth
            .login(&pre, &csrf, "0123456789abcdef0123", AccessMode::Cookie)
            .is_ok());
    }

    /// 重启恢复的是摘要，改密及显式撤销后持久记录不能复活旧邀请。
    #[test]
    fn invitation_store_restarts_revokes_and_never_contains_plaintext_credentials() {
        let directory =
            std::env::temp_dir().join(format!("yoodesk-invite-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("invitations.json");
        let (auth, clock) = authority();
        auth.configure_invitations(path.clone()).unwrap();
        let invite = auth.create_invitation(60, vec!["vnc".into()]).unwrap();
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(!stored.contains(&invite.token) && !stored.contains(&invite.temporary_password));
        let restarted =
            AccessAuthority::new(auth.policy().clone(), "0123456789abcdef0123", clock.clone())
                .unwrap();
        restarted.configure_invitations(path.clone()).unwrap();
        let (pre, csrf) = restarted.begin_preauth(AccessMode::Cookie).unwrap();
        assert!(restarted
            .login(&pre, &csrf, &invite.token, AccessMode::Cookie)
            .is_ok());
        restarted.revoke_invitations().unwrap();
        let again =
            AccessAuthority::new(auth.policy().clone(), "0123456789abcdef0123", clock.clone())
                .unwrap();
        again.configure_invitations(path.clone()).unwrap();
        let (pre, csrf) = again.begin_preauth(AccessMode::Cookie).unwrap();
        assert!(again
            .login(&pre, &csrf, &invite.temporary_password, AccessMode::Cookie)
            .is_err());
        let invite = again.create_invitation(60, vec!["desk".into()]).unwrap();
        again.replace_key("changed-password");
        let changed =
            AccessAuthority::new(auth.policy().clone(), "changed-password", clock).unwrap();
        changed.configure_invitations(path.clone()).unwrap();
        let (pre, csrf) = changed.begin_preauth(AccessMode::Cookie).unwrap();
        assert!(changed
            .login(&pre, &csrf, &invite.token, AccessMode::Cookie)
            .is_err());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    /// 失败写入不能发布新凭据，活动邀请数保持有界。
    #[test]
    fn invitation_capacity_and_storage_failure_do_not_issue_credentials() {
        let (auth, _) = authority();
        for (seconds, protocols) in [
            (0, vec!["desk".into()]),
            (86401, vec!["desk".into()]),
            (60, vec!["desk".into(), "desk".into()]),
        ] {
            assert_eq!(
                auth.create_invitation(seconds, protocols)
                    .err()
                    .unwrap()
                    .status,
                400
            );
        }
        for _ in 0..64 {
            auth.create_invitation(60, vec!["desk".into()]).unwrap();
        }
        assert_eq!(
            auth.create_invitation(60, vec!["desk".into()])
                .err()
                .unwrap()
                .status,
            429
        );
        let (auth, _) = authority();
        auth.configure_invitations(
            std::env::temp_dir()
                .join(uuid::Uuid::new_v4().to_string())
                .join("missing.json"),
        )
        .unwrap();
        assert!(auth.create_invitation(60, vec!["desk".into()]).is_err());
        assert!(auth.lock_state().invitations.is_empty());
    }
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn replacing_key_rejects_previous_key_and_revokes_existing_sessions() {
        let (auth, _) = authority();
        let (ticket, csrf) = auth.begin_preauth(AccessMode::Bearer).unwrap();
        let _ = auth
            .login(&ticket, &csrf, "0123456789abcdef0123", AccessMode::Bearer)
            .unwrap();
        assert_eq!(auth.lock_state().sessions.len(), 1);
        let removed = auth.replace_key("new-password");
        assert_eq!(removed.len(), 1);
        assert!(auth.lock_state().sessions.is_empty());
        assert!(!auth.verify_key("0123456789abcdef0123"));
        assert!(auth.verify_key("new-password"));
    }

    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn new() -> Self {
            Self(AtomicU64::new(1_000))
        }
        fn advance(&self, seconds: u64) {
            self.0.fetch_add(seconds, Ordering::SeqCst);
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH + Duration::from_secs(self.0.load(Ordering::SeqCst))
        }
    }

    fn authority() -> (AccessAuthority, Arc<FakeClock>) {
        let clock = Arc::new(FakeClock::new());
        let policy = AccessPolicy::new(
            "yoodesk",
            vec![LocalPrincipal {
                principal_id: "local".into(),
                scopes: vec!["session.connect".into(), "screen.view".into()],
            }],
        );
        (
            AccessAuthority::new(policy, "0123456789abcdef0123", clock.clone()).unwrap(),
            clock,
        )
    }

    fn login(auth: &AccessAuthority, mode: AccessMode) -> LoginResult {
        let (ticket, csrf) = auth.begin_preauth(mode).unwrap();
        auth.login(&ticket, &csrf, "0123456789abcdef0123", mode)
            .unwrap()
    }

    #[test]
    fn six_character_key_authenticates_and_shorter_key_is_rejected() {
        let (original, clock) = authority();
        let auth = AccessAuthority::new(original.policy.clone(), "abc123", clock.clone()).unwrap();
        let (ticket, csrf) = auth.begin_preauth(AccessMode::Bearer).unwrap();
        assert_eq!(
            auth.login(&ticket, &csrf, "abc124", AccessMode::Bearer)
                .unwrap_err()
                .status,
            401
        );
        assert!(auth
            .login(&ticket, &csrf, "abc123", AccessMode::Bearer)
            .is_ok());
        assert!(AccessAuthority::new(original.policy.clone(), "abc12", clock).is_err());
    }

    #[test]
    fn preauth_capacity_recovers_after_expiry_and_consumption() {
        let (auth, clock) = authority();
        let (ticket, csrf) = auth.begin_preauth(AccessMode::Cookie).unwrap();
        for _ in 1..MAX_PREAUTHS {
            auth.begin_preauth(AccessMode::Cookie).unwrap();
        }
        assert_eq!(
            auth.begin_preauth(AccessMode::Cookie).unwrap_err().status,
            429
        );
        auth.login(&ticket, &csrf, "0123456789abcdef0123", AccessMode::Cookie)
            .unwrap();
        assert!(!auth.lock_state().preauths.contains_key(&ticket));
        auth.begin_preauth(AccessMode::Cookie).unwrap();
        clock.advance(120);
        auth.begin_preauth(AccessMode::Cookie).unwrap();
        assert_eq!(auth.lock_state().preauths.len(), 1);
    }

    #[test]
    fn bootstrap_has_a_separate_global_budget() {
        let (mut auth, _) = authority();
        auth.bootstrap_limiter = LoginLimiter::new(120.0, 0.0);
        for _ in 0..120 {
            assert!(auth.allow_bootstrap());
        }
        assert!(!auth.allow_bootstrap());
        // Anonymous traffic must not consume the credential attempt budget.
        assert!(auth.begin_preauth(AccessMode::Cookie).is_ok());
    }

    #[test]
    fn access_sessions_are_reclaimed_and_capped() {
        let (auth, clock) = authority();
        for _ in 0..MAX_ACCESS_SESSIONS {
            login(&auth, AccessMode::Cookie);
        }
        let (ticket, csrf) = auth.begin_preauth(AccessMode::Cookie).unwrap();
        assert_eq!(
            auth.login(&ticket, &csrf, "0123456789abcdef0123", AccessMode::Cookie)
                .unwrap_err()
                .status,
            429
        );
        assert!(auth.check_preauth_csrf(&ticket, &csrf).is_ok());
        let current = auth.lock_state().sessions.keys().next().copied().unwrap();
        let context = auth.resolve(current, AccessMode::Cookie).unwrap();
        auth.logout(&context);
        // Capacity errors preserve the unconsumed preauth for a direct retry.
        auth.login(&ticket, &csrf, "0123456789abcdef0123", AccessMode::Cookie)
            .unwrap();
        clock.advance(3600);
        login(&auth, AccessMode::Cookie);
        assert_eq!(auth.lock_state().sessions.len(), 1);
    }

    #[test]
    fn limiter_never_grows_past_capacity_or_resets_blocked_peers() {
        let limiter = LoginLimiter::new(1.0, 0.0);
        for n in 0..4096u32 {
            assert!(limiter.allow(IpAddr::V4(n.into())));
        }
        assert!(!limiter.allow(IpAddr::V4(4096u32.into())));
        assert!(!limiter.allow(IpAddr::V4(0u32.into())));
        assert_eq!(limiter.inner.lock().unwrap().len(), 4096);
    }

    #[test]
    fn login_requires_preauth_csrf() {
        let (auth, _) = authority();
        let (ticket, _csrf) = auth.begin_preauth(AccessMode::Cookie).unwrap();
        let err = auth
            .login(&ticket, "wrong", "0123456789abcdef0123", AccessMode::Cookie)
            .unwrap_err();
        assert_eq!(err.code, codes::INVALID_REQUEST);
    }

    #[test]
    fn login_rejects_wrong_key_and_keeps_preauth() {
        let (auth, _) = authority();
        let (ticket, csrf) = auth.begin_preauth(AccessMode::Cookie).unwrap();
        assert!(auth
            .login(&ticket, &csrf, "not-the-key", AccessMode::Cookie)
            .is_err());
        // 正确密钥仍可使用同一 preauth，避免一次输入错误就强制重开页面。
        assert!(auth
            .login(&ticket, &csrf, "0123456789abcdef0123", AccessMode::Cookie)
            .is_ok());
    }

    #[test]
    fn preauth_is_single_use_and_expires() {
        let (auth, clock) = authority();
        let (ticket, csrf) = auth.begin_preauth(AccessMode::Cookie).unwrap();
        auth.login(&ticket, &csrf, "0123456789abcdef0123", AccessMode::Cookie)
            .unwrap();
        assert!(auth
            .login(&ticket, &csrf, "0123456789abcdef0123", AccessMode::Cookie)
            .is_err());

        let (ticket, csrf) = auth.begin_preauth(AccessMode::Cookie).unwrap();
        clock.advance(121);
        assert!(auth
            .login(&ticket, &csrf, "0123456789abcdef0123", AccessMode::Cookie)
            .is_err());
    }

    #[test]
    fn access_mode_must_match_preauth() {
        let (auth, _) = authority();
        let (ticket, csrf) = auth.begin_preauth(AccessMode::Cookie).unwrap();
        let err = auth
            .login(&ticket, &csrf, "0123456789abcdef0123", AccessMode::Bearer)
            .unwrap_err();
        assert_eq!(err.code, codes::INVALID_REQUEST);
    }

    #[test]
    fn session_expires_at_idle_and_absolute_deadlines() {
        let (auth, clock) = authority();
        let login = login(&auth, AccessMode::Cookie);
        let id = AccessAuthority::parse_session_token(login.cookie.as_deref().unwrap()).unwrap();
        assert!(auth.resolve(id, AccessMode::Cookie).is_ok());

        clock.advance(3601);
        assert!(auth.resolve(id, AccessMode::Cookie).is_err());
    }

    #[test]
    fn renew_rotates_csrf_but_never_extends_absolute() {
        let (auth, clock) = authority();
        let login = login(&auth, AccessMode::Cookie);
        let id = AccessAuthority::parse_session_token(login.cookie.as_deref().unwrap()).unwrap();
        let context = auth.resolve(id, AccessMode::Cookie).unwrap();

        clock.advance(1800);
        let (expires, csrf) = auth.renew(&context).unwrap();
        assert_ne!(csrf, context.csrf);
        assert_eq!(
            expires,
            context
                .absolute_expires_at
                .min(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + 1800 + 3600))
        );

        // 持续续期：空闲期限滚动，但绝对期限不延长。
        // 每步 3000 秒小于空闲期限 3600 秒，因此只靠续期即可跨过 12 小时绝对期限。
        let mut context = auth.resolve(id, AccessMode::Cookie).unwrap();
        let mut still_alive = true;
        for _ in 0..14 {
            clock.advance(3000);
            match auth.resolve(id, AccessMode::Cookie) {
                Ok(current) => {
                    context = current;
                    let (expires, _) = auth.renew(&context).unwrap();
                    assert!(expires <= context.absolute_expires_at);
                }
                Err(_) => {
                    still_alive = false;
                    break;
                }
            }
        }
        // 绝对期限到达后是终态，续期不能复活。
        assert!(!still_alive, "绝对期限必须终止会话");
        assert!(auth.resolve(id, AccessMode::Cookie).is_err());
        assert!(auth.renew(&context).is_err());
    }

    #[test]
    fn mode_mismatch_rejected() {
        let (auth, _) = authority();
        let login = login(&auth, AccessMode::Cookie);
        let id = AccessAuthority::parse_session_token(login.cookie.as_deref().unwrap()).unwrap();
        assert!(auth.resolve(id, AccessMode::Bearer).is_err());
    }

    #[test]
    fn logout_revokes_only_this_session() {
        let (auth, _) = authority();
        let first = login(&auth, AccessMode::Cookie);
        let second = login(&auth, AccessMode::Bearer);
        let first_id =
            AccessAuthority::parse_session_token(first.cookie.as_deref().unwrap()).unwrap();
        let second_id =
            AccessAuthority::parse_session_token(second.access_token.as_deref().unwrap()).unwrap();
        let context = auth.resolve(first_id, AccessMode::Cookie).unwrap();
        auth.logout(&context);
        assert!(auth.resolve(first_id, AccessMode::Cookie).is_err());
        assert!(auth.resolve(second_id, AccessMode::Bearer).is_ok());
    }

    #[test]
    fn csrf_is_required_for_writes() {
        let (auth, _) = authority();
        let login = login(&auth, AccessMode::Cookie);
        let id = AccessAuthority::parse_session_token(login.cookie.as_deref().unwrap()).unwrap();
        let context = auth.resolve(id, AccessMode::Cookie).unwrap();
        assert!(auth.require_csrf(&context, "nope").is_err());
        assert!(auth.require_csrf(&context, &context.csrf).is_ok());
    }

    #[test]
    fn tickets_are_single_use_and_channel_bound() {
        let (auth, clock) = authority();
        let login = login(&auth, AccessMode::Cookie);
        let id = AccessAuthority::parse_session_token(login.cookie.as_deref().unwrap()).unwrap();
        let context = auth.resolve(id, AccessMode::Cookie).unwrap();
        let (ticket, _) = auth.issue_ticket(&context, TicketChannel::Stream).unwrap();
        let consumed = auth.consume_ticket(&ticket, TicketChannel::Stream).unwrap();
        assert_eq!(consumed.session_id, id);
        assert!(auth.consume_ticket(&ticket, TicketChannel::Stream).is_err());

        let (ticket, _) = auth.issue_ticket(&context, TicketChannel::Stream).unwrap();
        assert!(auth.consume_ticket(&ticket, TicketChannel::Events).is_err());

        let (ticket, _) = auth.issue_ticket(&context, TicketChannel::Events).unwrap();
        clock.advance(31);
        assert!(auth.consume_ticket(&ticket, TicketChannel::Events).is_err());
    }

    #[test]
    fn logout_invalidates_outstanding_tickets() {
        let (auth, _) = authority();
        let login = login(&auth, AccessMode::Cookie);
        let id = AccessAuthority::parse_session_token(login.cookie.as_deref().unwrap()).unwrap();
        let context = auth.resolve(id, AccessMode::Cookie).unwrap();
        let (ticket, _) = auth.issue_ticket(&context, TicketChannel::Stream).unwrap();
        auth.logout(&context);
        assert!(auth.consume_ticket(&ticket, TicketChannel::Stream).is_err());
    }

    #[test]
    fn ticket_candidate_extraction() {
        let protocols = ["raccess.ticket.abc", "business.v2"];
        assert_eq!(extract_ticket_candidate(protocols), (Some("abc"), false));
        assert_eq!(
            extract_ticket_candidate(["raccess.ticket.", "x"]),
            (None, true)
        );
        assert_eq!(extract_ticket_candidate(["business.v2"]), (None, false));
        assert_eq!(
            extract_ticket_candidate(["raccess.ticket.a", "raccess.ticket.b"]),
            (None, true)
        );
    }

    #[test]
    fn origin_check_is_exact() {
        assert!(
            require_exact_origin(Some("https://127.0.0.1:30443"), "https://127.0.0.1:30443")
                .is_ok()
        );
        assert!(
            require_exact_origin(Some("https://evil.example"), "https://127.0.0.1:30443").is_err()
        );
        assert!(require_exact_origin(None, "https://127.0.0.1:30443").is_err());
    }

    #[test]
    fn login_limiter_backs_off_after_failures() {
        let limiter = LoginLimiter::new(2.0, 0.0);
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(limiter.allow(peer));
        assert!(limiter.allow(peer));
        assert!(!limiter.allow(peer));
        // 其他来源不受影响。
        assert!(limiter.allow("10.0.0.2".parse().unwrap()));
    }

    #[test]
    fn login_attempts_reserve_before_verification_and_refund_success_only() {
        let limiter = LoginLimiter::new(2.0, 0.0);
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        {
            let first = limiter.begin_attempt(peer).unwrap();
            let _failed = limiter.begin_attempt(peer).unwrap();
            assert!(
                limiter.begin_attempt(peer).is_none(),
                "in-flight checks must be bounded"
            );
            first.succeed();
            let _canceled = limiter.begin_attempt(peer).unwrap();
        }
        assert!(
            limiter.begin_attempt(peer).is_none(),
            "failed/canceled checks must retain their debit"
        );
        assert!(limiter.begin_attempt("10.0.0.2".parse().unwrap()).is_some());
    }

    #[test]
    fn session_token_parsing_rejects_garbage() {
        assert_eq!(
            AccessAuthority::parse_session_token(&session_token(1)),
            Some(1)
        );
        assert_eq!(
            AccessAuthority::parse_session_token("raccess.s.00000000000000000000000000000001"),
            None
        );
        assert_eq!(AccessAuthority::parse_session_token("raccess.s.1"), None);
        assert_eq!(
            AccessAuthority::parse_session_token("raccess.s.zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"),
            None
        );
        assert_eq!(AccessAuthority::parse_session_token(""), None);
    }

    /// 知道编号或替换随机后缀不能获得身份；Cookie/Bearer 和撤销规则仍生效。
    #[test]
    fn session_credentials_require_the_issued_random_proof() {
        let (auth, _) = authority();
        for mode in [AccessMode::Cookie, AccessMode::Bearer] {
            let issued = login(&auth, mode);
            let token = issued.access_token.unwrap();
            let id = AccessAuthority::parse_session_token(&token).unwrap();
            assert_eq!(
                auth.authenticate_token(&token, mode).unwrap().session_id,
                id
            );
            let bare = format!("{SESSION_PREFIX}{id:032x}");
            let forged = format!("{bare}.{}", "0".repeat(64));
            assert!(auth.authenticate_token(&bare, mode).is_err());
            assert!(auth.authenticate_token(&forged, mode).is_err());
            let wrong_mode = if mode == AccessMode::Cookie {
                AccessMode::Bearer
            } else {
                AccessMode::Cookie
            };
            assert!(auth.authenticate_token(&token, wrong_mode).is_err());
            let context = auth.authenticate_token(&token, mode).unwrap();
            auth.logout(&context);
            assert!(auth.authenticate_token(&token, mode).is_err());
        }
    }

    /// 进程重建即使复用相同逻辑编号，旧 Cookie 也不能进入新登录的身份。
    #[test]
    fn old_credentials_cannot_authenticate_after_authority_restart() {
        let (first, _) = authority();
        let old = login(&first, AccessMode::Cookie).cookie.unwrap();
        let (second, _) = authority();
        let current = login(&second, AccessMode::Cookie).cookie.unwrap();
        assert_eq!(
            AccessAuthority::parse_session_token(&old),
            AccessAuthority::parse_session_token(&current)
        );
        assert!(second.authenticate_token(&old, AccessMode::Cookie).is_err());
        assert!(second
            .authenticate_token(&current, AccessMode::Cookie)
            .is_ok());
    }
}
