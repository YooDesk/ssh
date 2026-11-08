//! 管理用户可新增连接；业务会话仍只引用服务器保存的目标id。
use serde::{Deserialize, Serialize};

/// 服务端连接配置，SSH 与文件面板共用同一个目标。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionTarget {
    /// 可选Windows域/机器名。
    #[serde(default)]
    pub domain: String,
    /// 可选RDP叶证书SHA256十六进制摘要。
    #[serde(default)]
    pub certificate_sha256: String,
    /// 稳定目标编号。
    pub id: String,
    /// 显示名称。
    pub label: String,
    /// vnc 或 ssh。
    pub protocol: String,
    /// 目标地址。
    pub host: String,
    /// TCP 端口。
    pub port: u16,
    /// SSH 用户名。
    #[serde(default)]
    pub username: String,
    /// SSH SHA256 主机指纹（区分大小写）。
    #[serde(default)]
    pub host_key: String,
    /// SFTP 允许的远程根。
    #[serde(default = "default_root")]
    pub root: String,
    /// 跳板目标编号，按连接顺序排列。
    #[serde(default)]
    pub jump_chain: Vec<String>,
}

fn default_root() -> String {
    "/".into()
}

/// 不含凭据的公开目标摘要。
#[derive(Serialize)]
pub struct TargetSummary<'a> {
    /// RDP域。
    pub domain: &'a str,
    /// RDP证书摘要，不含私钥。
    pub certificate_sha256: &'a str,
    /// 乐观并发版本。
    pub revision: String,
    /// 内建本机连接，固定在首位且不可删除。
    pub is_default: bool,
    /// 根路径和指纹属于公开配置，不含口令/私钥。
    pub root: &'a str,
    /// 期望的SSH服务器公钥指纹。
    pub host_key: &'a str,
    /// 目标编号。
    pub target_id: &'a str,
    /// 显示名称。
    pub label: &'a str,
    /// 主机地址。
    pub host: &'a str,
    /// 端口。
    pub port: u16,
    /// 用户名。
    pub username: &'a str,
    /// 跳板编号。
    pub jump_chain: &'a [String],
}

impl ConnectionTarget {
    /// 公开安全摘要。
    pub fn summary(&self) -> TargetSummary<'_> {
        use sha2::{Digest, Sha256};
        TargetSummary {
            domain: &self.domain,
            certificate_sha256: &self.certificate_sha256,
            revision: format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(self).expect("serializable profile"))
            ),
            is_default: matches!(self.id.as_str(), "local-vnc" | "local-ssh" | "local-rdp"),
            root: &self.root,
            host_key: &self.host_key,
            target_id: &self.id,
            label: &self.label,
            host: &self.host,
            port: self.port,
            username: &self.username,
            jump_chain: &self.jump_chain,
        }
    }
}

/// 启动时拒绝重复编号、非法地址和循环跳板。
pub fn validate(targets: &[ConnectionTarget]) -> Result<(), String> {
    if targets.len() > 67
        || targets
            .iter()
            .filter(|t| !matches!(t.id.as_str(), "local-vnc" | "local-ssh" | "local-rdp"))
            .count()
            > 64
    {
        return Err("at most 64 custom targets and three local connections".into());
    }
    let mut ids = std::collections::HashSet::new();
    for target in targets {
        if target.id.is_empty()
            || target.id.len() > 64
            || !target
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || !ids.insert(&target.id)
        {
            return Err("invalid or duplicate target id".into());
        }
        if !matches!(target.protocol.as_str(), "vnc" | "ssh" | "rdp")
            || !valid_host(&target.host)
            || target.port == 0
            || target.label.is_empty()
            || target.label.len() > 128
            || target.label.chars().any(char::is_control)
        {
            return Err("invalid connection target".into());
        }
        if (target.id == "local-vnc" && (target.protocol != "vnc" || target.host != "127.0.0.1"))
            || (target.id == "local-ssh"
                && (target.protocol != "ssh" || target.host != "127.0.0.1"))
            || (target.id == "local-rdp"
                && (target.protocol != "rdp" || target.host != "127.0.0.1"))
        {
            return Err("local connections must use 127.0.0.1 and their original protocol".into());
        }
        if target.username.len() > 128
            || target.username.chars().any(char::is_control)
            || target.domain.len() > 253
            || target.domain.chars().any(char::is_control)
            || (!target.certificate_sha256.is_empty()
                && (target.certificate_sha256.len() != 64
                    || !target
                        .certificate_sha256
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit())))
        {
            return Err("invalid RDP domain or certificate fingerprint".into());
        }
        if target.protocol == "ssh"
            && ((target.username.is_empty() && target.id != "local-ssh")
                || target.username.len() > 128
                || target.username.chars().any(char::is_control)
                || !target.root.starts_with('/')
                || target.root.len() > 4096
                || target.host_key.len() > 128
                || target.root.contains('\0')
                || (!target.host_key.is_empty() && !target.host_key.starts_with("SHA256:")))
        {
            return Err("invalid SSH target".into());
        }
        if target.jump_chain.len() > 8 {
            return Err("at most eight SSH jumps".into());
        }
        let mut chain = std::collections::HashSet::from([target.id.as_str()]);
        for jump in &target.jump_chain {
            if !chain.insert(jump.as_str())
                || !targets
                    .iter()
                    .any(|t| t.id == *jump && t.protocol == "ssh" && t.jump_chain.is_empty())
            {
                return Err("invalid or cyclic jump chain".into());
            }
        }
    }
    Ok(())
}

fn valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    host.parse::<std::net::IpAddr>().is_ok()
        || host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// 无密码/私钥的目标目录，写入成功后才发布内存新版本。
pub struct Catalog {
    items: Vec<ConnectionTarget>,
    file: String,
}
impl Catalog {
    /// 两个内建本机连接始终位于对应协议首位。
    pub fn new(config: &crate::config::Config) -> Self {
        let reference = config.targets.iter().find(|t| {
            t.protocol == "ssh"
                && matches!(t.host.as_str(), "127.0.0.1" | "localhost")
                && t.port == 22
        });
        let username = reference.map(|t| t.username.clone()).unwrap_or_else(|| {
            std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_default()
        });
        let mut items = vec![];
        for (protocol, port) in [("vnc", 5900), ("ssh", 22), ("rdp", 3389)] {
            items.push(ConnectionTarget {
                domain: String::new(),
                certificate_sha256: String::new(),
                id: format!("local-{protocol}"),
                label: "本机（127.0.0.1）".into(),
                protocol: protocol.into(),
                host: "127.0.0.1".into(),
                port,
                username: if protocol == "ssh" {
                    username.clone()
                } else {
                    String::new()
                },
                host_key: if protocol == "ssh" {
                    reference.map(|t| t.host_key.clone()).unwrap_or_default()
                } else {
                    String::new()
                },
                root: "/".into(),
                jump_chain: vec![],
            });
        }
        for target in &config.targets {
            if let Some(old) = items.iter_mut().find(|t| t.id == target.id) {
                *old = target.clone();
            } else {
                items.push(target.clone());
            }
        }
        Self {
            items,
            file: config.connection_profiles_file.clone(),
        }
    }
    /// 完整配置快照用于原生连接；公开响应自行投影。
    pub fn all(&self) -> Vec<ConnectionTarget> {
        self.items.clone()
    }
    /// 新增或更新，旧版本冲突时不覆盖其他页面的修改。
    pub fn put(
        &mut self,
        target: ConnectionTarget,
        expected: Option<&str>,
    ) -> Result<ConnectionTarget, crate::json::ApiError> {
        use crate::json::ApiError;
        let mut next = self.items.clone();
        if let Some(old) = next.iter_mut().find(|t| t.id == target.id) {
            if expected != Some(old.summary().revision.as_str()) {
                return Err(ApiError::new(
                    409,
                    "state_conflict",
                    "connection changed; refresh and try again",
                ));
            }
            *old = target.clone();
        } else {
            if expected.is_some() {
                return Err(ApiError::new(404, "not_found", "connection not found"));
            }
            next.push(target.clone());
        }
        validate(&next).map_err(|e| ApiError::new(400, "invalid_request", e))?;
        self.persist(&next)?;
        self.items = next;
        Ok(target)
    }
    /// 删除被跳板引用的目标时拒绝，防止使已有连接配置失效。
    pub fn remove(&mut self, id: &str, revision: &str) -> Result<(), crate::json::ApiError> {
        use crate::json::ApiError;
        if matches!(id, "local-vnc" | "local-ssh" | "local-rdp") {
            return Err(ApiError::new(
                403,
                "forbidden",
                "the local connection cannot be deleted",
            ));
        }
        let old = self
            .items
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| ApiError::new(404, "not_found", "connection not found"))?;
        if old.summary().revision != revision {
            return Err(ApiError::new(
                409,
                "state_conflict",
                "connection changed; refresh and try again",
            ));
        }
        if self
            .items
            .iter()
            .any(|t| t.jump_chain.iter().any(|j| j == id))
        {
            return Err(ApiError::new(
                409,
                "state_conflict",
                "connection is used as an SSH jump host",
            ));
        }
        let next: Vec<_> = self.items.iter().filter(|t| t.id != id).cloned().collect();
        self.persist(&next)?;
        self.items = next;
        Ok(())
    }
    fn persist(&self, next: &[ConnectionTarget]) -> Result<(), crate::json::ApiError> {
        if self.file.is_empty() {
            return Ok(());
        }
        use std::io::Write;
        let path = std::path::Path::new(&self.file);
        let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> std::io::Result<()> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(next)?)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, path)
        })();
        if let Err(error) = result {
            if temporary.exists() {
                let _ = std::fs::remove_file(&temporary);
            }
            tracing::error!(%error,"保存连接配置失败");
            return Err(crate::json::ApiError::new(
                500,
                "internal",
                "cannot save connection profiles",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    #[test]
    fn profiles_survive_restart_without_resurrecting_deleted_targets() {
        let dir = std::env::temp_dir().join(format!("connection-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let mut config = crate::config::tests::test_config();
        config.connection_profiles_file = dir.join("profiles.json").to_string_lossy().into_owned();
        let mut catalog = Catalog::new(&config);
        let mut target = catalog.all()[0].clone();
        target.id = "external-vnc".into();
        target.host = "192.0.2.10".into();
        target.label = "External VNC".into();
        let saved = catalog.put(target, None).unwrap();
        assert!(std::path::Path::new(&config.connection_profiles_file).exists());
        let config_path = dir.join("config.json");
        let raw = serde_json::json!({"product_id":"yoodesk","public_origin":"https://127.0.0.1:30443","bind":"127.0.0.1:30443","certificate":"cert.pem","private_key":"key.pem","access_key_file":"access.key","connection_profiles_file":"profiles.json","targets":[saved.clone()]});
        std::fs::write(&config_path, raw.to_string()).unwrap();
        let restored = crate::config::Config::load(&config_path).unwrap();
        let mut restored = Catalog::new(&restored);
        assert_eq!(restored.all().len(), 4);
        restored
            .remove(&saved.id, &saved.summary().revision)
            .unwrap();
        let restored = crate::config::Config::load(&config_path).unwrap();
        assert!(!restored.targets.iter().any(|t| t.id == saved.id));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
