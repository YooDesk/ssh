//! 有效期邀请：长期密码不进入链接；持久记录只保存随机凭据摘要和授权范围。
use super::*;
use std::path::{Path, PathBuf};

const MAX_INVITATIONS: usize = 64;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    token: [u8; 32],
    password: [u8; 32],
    expires: SystemTime,
    protocols: Vec<String>,
}

#[derive(Clone, Debug)]
pub(super) struct Grant {
    pub expires: SystemTime,
    pub protocols: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    version: u8,
    binding: [u8; 32],
    entries: Vec<Record>,
}

#[derive(Serialize)]
pub struct Invitation {
    pub token: String,
    pub temporary_password: String,
    pub issued_at: String,
    pub expires_at: String,
    pub protocols: Vec<String>,
}

/// 摘要绑定当前长期密钥，改密后旧持久记录不能重新生效。
fn binding(key: &[u8; 32]) -> [u8; 32] {
    Sha256::digest([b"YooDesk invitations v1".as_slice(), key].concat()).into()
}

/// 原子替换私有摘要文件；写失败不签发邀请或报告撤销成功。
fn persist(path: Option<&Path>, key: &[u8; 32], entries: &[Record]) -> Result<(), ApiError> {
    let Some(path) = path else {
        return Ok(());
    };
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let bytes = serde_json::to_vec(&Registry {
            version: 1,
            binding: binding(key),
            entries: entries.to_vec(),
        })?;
        let temporary = path.with_extension(format!("{}.tmp", random_token(16)));
        let result = (|| -> std::io::Result<()> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    })();
    result.map_err(|_| ApiError::new(500, codes::INTERNAL, "invitation persistence failed"))
}

impl AccessAuthority {
    /// 启动时加载未过期的邀请；摘要与当前密码不匹配时拒绝恢复旧授权。
    pub fn configure_invitations(&self, path: PathBuf) -> Result<(), ApiError> {
        let key = self.key_digest.lock().unwrap();
        let mut state = self.lock_state();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => {
                return Err(ApiError::new(
                    500,
                    codes::INTERNAL,
                    "invitation store unavailable",
                ))
            }
        };
        if let Some(bytes) = bytes {
            if bytes.len() > 65536 {
                return Err(ApiError::new(
                    500,
                    codes::INTERNAL,
                    "invalid invitation store",
                ));
            }
            let registry: Registry = serde_json::from_slice(&bytes)
                .map_err(|_| ApiError::new(500, codes::INTERNAL, "invalid invitation store"))?;
            if registry.version != 1 || registry.entries.len() > MAX_INVITATIONS {
                return Err(ApiError::new(
                    500,
                    codes::INTERNAL,
                    "invalid invitation store",
                ));
            }
            if bool::from(registry.binding.ct_eq(&binding(&key))) {
                let now = self.clock.now();
                state.invitations = registry
                    .entries
                    .into_iter()
                    .filter(|entry| now < entry.expires)
                    .collect();
            }
        }
        state.invitation_path = Some(path);
        Ok(())
    }

    /// 参照 C++ 默认一小时及八位数字/小写字母验证码，签发共用期限的两种凭据。
    pub fn create_invitation(
        &self,
        seconds: u64,
        protocols: Vec<String>,
    ) -> Result<Invitation, ApiError> {
        use rand::Rng;
        if !(1..=86400).contains(&seconds)
            || protocols.is_empty()
            || protocols.len() > 4
            || protocols
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != protocols.len()
            || protocols
                .iter()
                .any(|p| !["desk", "ssh", "rdp", "vnc"].contains(&p.as_str()))
        {
            return Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "invalid invitation options",
            ));
        }
        let now = self.clock.now();
        let expires = now + Duration::from_secs(seconds);
        let key = self.key_digest.lock().unwrap();
        let mut state = self.lock_state();
        let mut entries: Vec<_> = state
            .invitations
            .iter()
            .filter(|entry| now < entry.expires)
            .cloned()
            .collect();
        if entries.len() >= MAX_INVITATIONS {
            return Err(capacity_error("too many active invitations"));
        }
        let token = format!("i1.{}", random_token(32));
        let alphabet = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let password = loop {
            let value: String = (0..8)
                .map(|_| alphabet[rand::rng().random_range(0..36)] as char)
                .collect();
            if value.bytes().any(|b| b.is_ascii_digit())
                && value.bytes().any(|b| b.is_ascii_lowercase())
                && !bool::from(digest(&value).ct_eq(&*key))
                && entries
                    .iter()
                    .all(|entry| !bool::from(entry.password.ct_eq(&digest(&value))))
            {
                break value;
            }
        };
        entries.push(Record {
            token: digest(&token),
            password: digest(&password),
            expires,
            protocols: protocols.clone(),
        });
        persist(state.invitation_path.as_deref(), &key, &entries)?;
        state.invitations = entries;
        Ok(Invitation {
            token,
            temporary_password: password,
            issued_at: crate::timefmt::format_rfc3339(now),
            expires_at: crate::timefmt::format_rfc3339(expires),
            protocols,
        })
    }

    /// 修改对外授权时撤销已有邀请及其访问会话，不影响长期密码的独立 LAN 会话。
    pub(crate) fn revoke_invitations(&self) -> Result<Vec<u64>, ApiError> {
        let key = self.key_digest.lock().unwrap();
        let mut state = self.lock_state();
        persist(state.invitation_path.as_deref(), &key, &[])?;
        state.invitations.clear();
        let mut ids = Vec::new();
        state.sessions.retain(|id, session| {
            if session.invitation.is_some() {
                ids.push(*id);
                false
            } else {
                true
            }
        });
        Ok(ids)
    }

    /// 协议路径在 HTTP/WS 升级前检查邀请范围；普通密码会话保持原权限。
    pub(crate) fn invitation_allows(&self, context: &AccessContext, protocol: &str) -> bool {
        if let Some(remote) = &self.remote {
            return remote.call(serde_json::json!({"op":"invitation.permitted", "context":context,"capability":protocol})).unwrap_or(false);
        }

        self.lock_state()
            .sessions
            .get(&context.session_id)
            .is_some_and(|session| {
                session.invitation.as_ref().is_none_or(|grant| {
                    self.clock.now() < grant.expires
                        && grant.protocols.iter().any(|p| p == protocol)
                })
            })
    }

    /// Connector 的临时凭据同样由 WebService 校验，不能把网关传输授权当成免密授权。
    pub(crate) fn unlock_connector_credential(
        &self,
        context: &AccessContext,
        credential: &str,
    ) -> Result<(), ApiError> {
        let key = self.key_digest.lock().unwrap();
        let mut state = self.lock_state();
        let grant = if bool::from(digest(credential).ct_eq(&*key)) {
            None
        } else {
            Some(
                lookup(&state.invitations, credential, self.clock.now())
                    .ok_or_else(unauthorized)?,
            )
        };
        let session = state
            .sessions
            .get_mut(&context.session_id)
            .ok_or_else(unauthorized)?;
        let remote = session.remote.as_mut().ok_or_else(unauthorized)?;
        if grant
            .as_ref()
            .is_some_and(|grant| !grant.protocols.contains(&remote.capability))
        {
            return Err(unauthorized());
        }
        remote.unlocked = true;
        session.invitation = grant;
        Ok(())
    }
}

/// 只比较摘要，链接令牌与手输密码共享同一记录和过期时间。
pub(super) fn lookup(entries: &[Record], credential: &str, now: SystemTime) -> Option<Grant> {
    let provided = digest(credential);
    entries
        .iter()
        .find(|entry| {
            now < entry.expires
                && (bool::from(entry.token.ct_eq(&provided))
                    || bool::from(entry.password.ct_eq(&provided)))
        })
        .map(|entry| Grant {
            expires: entry.expires,
            protocols: entry.protocols.clone(),
        })
}
