//! 运行配置：严格 JSON、显式产品前缀与 listener 身份。
//!
//! 配置不通过遍历其他组件目录推断；未知字段、重复键、冲突监听一律拒绝（见 01-build-layout §2）。
//! 访问密钥可以来自受限文件，避免出现在命令行/日志中。

use std::net::SocketAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// WebService 运行配置。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 连接配置存储文件（不含密码/私钥）；空值仅供内存测试。
    #[serde(default)]
    pub connection_profiles_file: String,
    /// 服务端允许的 VNC/SSH 目标；默认关闭所有外部协议。
    #[serde(default)]
    pub targets: Vec<crate::connections::ConnectionTarget>,
    /// Linux DRM 默认开启；实际平台探测通过后才使用 root 采集辅助，false 明确禁用。
    #[serde(default = "default_enable_drm")]
    pub enable_drm: bool,
    /// 产品 id，决定 URL 前缀与 Cookie 名称。
    pub product_id: String,
    /// 浏览器允许的精确 Origin，例如 `https://127.0.0.1:30443`。
    pub public_origin: String,
    /// LAN HTTPS 监听地址。
    pub bind: String,
    /// Connector 专用回环 HTTP，同时承载本机免登录管理页。
    #[serde(default = "default_loopback_bind")]
    pub loopback_bind: String,
    /// HTTPS 证书链（PEM）。
    pub certificate: String,
    /// HTTPS 私钥（PEM）。
    pub private_key: String,
    /// 访问密钥文件；内容为单行明文密钥，读取后只保留摘要。
    pub access_key_file: String,
    /// desk 显示器默认 id。
    #[serde(default)]
    pub default_display: Option<String>,
    /// 单会话最大帧率。
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// 单会话目标码率（bit/s）。
    #[serde(default = "default_bitrate")]
    pub bitrate_bps: u32,
    /// 同时进行的 desk 会话上限。
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
}

fn default_fps() -> u32 {
    20
}

fn default_enable_drm() -> bool {
    true
}

/// 固定本机管理入口；测试可显式使用回环随机端口。
fn default_loopback_bind() -> String {
    "127.0.0.1:30445".into()
}

fn default_bitrate() -> u32 {
    4_000_000
}

fn default_max_sessions() -> usize {
    4
}

/// 配置错误：启动阶段直接失败，不做默认值兜底。
#[derive(Debug)]
pub enum ConfigError {
    /// 文件读取失败。
    Io(std::io::Error),
    /// JSON 解析失败（含重复键/未知字段）。
    Parse(String),
    /// 字段语义非法。
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "读取配置失败: {err}"),
            Self::Parse(err) => write!(f, "解析配置失败: {err}"),
            Self::Invalid(err) => write!(f, "配置非法: {err}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// 从文件加载并校验配置。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read(path).map_err(ConfigError::Io)?;
        let value =
            crate::json::parse_json_strict(&raw).map_err(|err| ConfigError::Parse(err.message))?;
        let mut config: Config =
            serde_json::from_value(value).map_err(|err| ConfigError::Parse(err.to_string()))?;
        config.resolve_paths(path);
        if !config.connection_profiles_file.is_empty()
            && Path::new(&config.connection_profiles_file).exists()
        {
            let raw = std::fs::read(&config.connection_profiles_file).map_err(ConfigError::Io)?;
            let value =
                crate::json::parse_json_strict(&raw).map_err(|e| ConfigError::Parse(e.message))?;
            let stored: Vec<crate::connections::ConnectionTarget> =
                serde_json::from_value(value).map_err(|e| ConfigError::Parse(e.to_string()))?;
            config.targets = stored;
        }
        config.validate()?;
        Ok(config)
    }

    /// 把相对路径解析为相对**配置文件所在目录**。
    ///
    /// 与 gateway 的 `runtimeconfig.ResolvePaths` 一致：启动时的工作目录不可作为
    /// 隐式配置来源，否则同一份配置在不同目录启动会指向不同证书。
    pub fn resolve_paths(&mut self, config_path: &Path) {
        // `Path::parent()` 对裸文件名返回 `Some("")`，这里统一当作当前目录，
        // 避免拼出 `.\cert.pem` 这类依赖平台的字符串。
        let base = config_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new(""));
        if self.connection_profiles_file.is_empty() {
            self.connection_profiles_file = format!("{}.connections.json", self.product_id);
        }
        for path in [
            &mut self.certificate,
            &mut self.private_key,
            &mut self.access_key_file,
            &mut self.connection_profiles_file,
        ] {
            let candidate = Path::new(path.as_str());
            if !candidate.is_absolute() {
                *path = base.join(candidate).to_string_lossy().into_owned();
            }
        }
    }

    /// 语义校验：产品前缀、Origin、监听地址、端口冲突。
    pub fn validate(&self) -> Result<(), ConfigError> {
        crate::connections::validate(&self.targets).map_err(ConfigError::Invalid)?;
        let valid_product = !self.product_id.is_empty()
            && self.product_id.len() <= 32
            && self.product_id.bytes().enumerate().all(|(i, b)| match b {
                b'a'..=b'z' | b'0'..=b'9' => true,
                b'-' => i > 0,
                _ => false,
            })
            && self.product_id.as_bytes()[0].is_ascii_lowercase();
        if !valid_product {
            return Err(ConfigError::Invalid(
                "product_id must match ^[a-z][a-z0-9-]{0,31}$".into(),
            ));
        }
        let origin = self.public_origin.trim_end_matches('/');
        if !origin.starts_with("https://") {
            return Err(ConfigError::Invalid(
                "public_origin must be an https origin".into(),
            ));
        }
        if origin.contains('/') && origin[8..].contains('/') {
            return Err(ConfigError::Invalid(
                "public_origin must not contain a path".into(),
            ));
        }
        self.bind
            .parse::<SocketAddr>()
            .map_err(|_| ConfigError::Invalid("bind must be host:port".into()))?;
        let admin = self.loopback_bind.parse::<SocketAddr>().map_err(|_| {
            ConfigError::Invalid("loopback_bind must be a loopback socket address".into())
        })?;
        if !admin.ip().is_loopback() {
            return Err(ConfigError::Invalid(
                "loopback_bind must be loopback".into(),
            ));
        }
        let public = self.bind.parse::<SocketAddr>().unwrap();
        if admin.port() != 0 && admin.port() == public.port() {
            return Err(ConfigError::Invalid(
                "admin and public listener ports must differ".into(),
            ));
        }
        if self.max_sessions == 0 {
            return Err(ConfigError::Invalid("max_sessions must be > 0".into()));
        }
        if self.fps == 0 || self.fps > 60 {
            return Err(ConfigError::Invalid("fps must be 1..=60".into()));
        }
        if self.bitrate_bps < 200_000 || self.bitrate_bps > 100_000_000 {
            return Err(ConfigError::Invalid(
                "bitrate_bps must be 200000..=100000000".into(),
            ));
        }
        if self.certificate == self.private_key {
            return Err(ConfigError::Invalid(
                "certificate and private_key must differ".into(),
            ));
        }
        Ok(())
    }

    /// 读取访问密钥；去除首尾空白，拒绝过短密钥。
    pub fn read_access_key(&self) -> Result<String, ConfigError> {
        let raw = std::fs::read_to_string(&self.access_key_file).map_err(ConfigError::Io)?;
        let key = raw.trim();
        if key.len() < crate::access::MIN_ACCESS_KEY_LEN {
            return Err(ConfigError::Invalid(
                "access key must be at least 6 characters".into(),
            ));
        }
        if key.chars().any(char::is_whitespace) {
            return Err(ConfigError::Invalid(
                "access key must not contain whitespace".into(),
            ));
        }
        Ok(key.to_string())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn access_key_file_accepts_six_characters_and_rejects_shorter_or_whitespace() {
        let path = std::env::temp_dir().join(format!("yoodesk-key-{}", uuid::Uuid::new_v4()));
        let mut config = test_config();
        config.access_key_file = path.to_string_lossy().into_owned();
        std::fs::write(&path, "abc123\n").unwrap();
        assert_eq!(config.read_access_key().unwrap(), "abc123");
        for invalid in ["abc12", "abc 123"] {
            std::fs::write(&path, invalid).unwrap();
            assert!(config.read_access_key().is_err());
        }
        std::fs::remove_file(path).unwrap();
    }

    pub(crate) fn test_config() -> Config {
        Config {
            connection_profiles_file: String::new(),
            targets: vec![],
            enable_drm: true,
            loopback_bind: "127.0.0.1:0".into(),
            product_id: "yoodesk".into(),
            public_origin: "https://127.0.0.1:30443".into(),
            bind: "127.0.0.1:30443".into(),
            certificate: "cert.pem".into(),
            private_key: "key.pem".into(),
            access_key_file: "access.key".into(),
            default_display: None,
            fps: 20,
            bitrate_bps: 4_000_000,
            max_sessions: 4,
        }
    }

    #[test]
    fn valid_config_passes() {
        assert!(test_config().validate().is_ok());
    }

    #[test]
    fn drm_defaults_enabled_and_accepts_explicit_disable() {
        let mut value = serde_json::to_value(test_config()).unwrap();
        value.as_object_mut().unwrap().remove("enable_drm");
        assert!(
            serde_json::from_value::<Config>(value.clone())
                .unwrap()
                .enable_drm
        );
        value["enable_drm"] = serde_json::json!(false);
        assert!(!serde_json::from_value::<Config>(value).unwrap().enable_drm);
    }

    #[test]
    fn invalid_product_id_rejected() {
        let mut config = test_config();
        for value in ["", "YooDesk", "1abc", "-abc", "a_b", &"a".repeat(33)] {
            config.product_id = value.to_string();
            assert!(config.validate().is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn http_origin_rejected() {
        let mut config = test_config();
        config.public_origin = "http://127.0.0.1:30443".into();
        assert!(config.validate().is_err());
        config.public_origin = "https://127.0.0.1:30443/some/path".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn unknown_fields_rejected_by_strict_parse() {
        let raw = br#"{"product_id":"yoodesk"}"#;
        let value = crate::json::parse_json_strict(raw).unwrap();
        assert!(serde_json::from_value::<Config>(value).is_err());
    }

    #[test]
    fn duplicate_keys_rejected() {
        let raw = br#"{"product_id":"a","product_id":"b"}"#;
        assert!(crate::json::parse_json_strict(raw).is_err());
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn relative_paths_resolve_against_config_directory() {
        let mut config = tests::test_config();
        config.certificate = "certs/local.crt".into();
        config.private_key = "/abs/local.key".into();
        config.resolve_paths(Path::new("E:/YooDesk/web-service/configs/local.json"));
        // 用 `Path` 比较而不是字符串，避免 Windows 反斜杠与正斜杠混用导致误报。
        assert_eq!(
            Path::new(&config.certificate),
            Path::new("E:/YooDesk/web-service/configs/certs/local.crt")
        );
        // 绝对路径保持不变（Windows 上 `/abs/local.key` 会带盘符，按绝对性判断）。
        assert!(Path::new(&config.private_key).is_absolute());
        assert!(config.private_key.ends_with("local.key"));
        assert_eq!(
            Path::new(&config.access_key_file),
            Path::new("E:/YooDesk/web-service/configs/access.key")
        );
    }

    #[test]
    fn bare_filename_keeps_working_directory_base() {
        let mut config = tests::test_config();
        config.resolve_paths(Path::new("local.json"));
        assert_eq!(config.certificate, "cert.pem");
    }
}
