//! 严格 JSON 与 WebService v2 统一信封。
//!
//! 设计与契约来源：`docs/design/04-webservice.md` §3 与 `protocol/business-v2.schema.json`。
//! 成功响应为 `{v:2,data,request_id}`，失败为 `{v:2,error:{code,message,retryable,details?},request_id}`。
//! 本模块不把任何前端可控字段直接当作身份或资源选择依据。

use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

/// 统一失败信封中的 `details`，字段与 `business-v2.schema.json#/$defs/Error` 完全一致。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorDetails {
    /// 出错字段名。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// 处理阶段标识。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// 给用户的提示。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// 跳板链中的 hop 序号。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hop_index: Option<u8>,
    /// 服务端确认的当前偏移（十进制字符串）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_offset: Option<String>,
    /// 建议的重试等待秒数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
}

/// 统一失败信封的 `error` 对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorBody {
    /// 稳定错误码，见 [`codes`]。
    pub code: String,
    /// 人类可读的错误说明；不得包含凭据或内部路径。
    pub message: String,
    /// 调用方是否可以原样重试。
    pub retryable: bool,
    /// 可选结构化细节。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<ErrorDetails>,
}

/// 失败信封本体。
#[derive(Debug, Clone, Serialize)]
pub struct ErrorEnvelope {
    /// 协议版本，固定为 2。
    pub v: u8,
    /// 错误主体。
    pub error: ErrorBody,
    /// 与请求对应的 request_id。
    pub request_id: String,
}

/// 成功信封，`data` 由各端点给出。
#[derive(Debug, Clone, Serialize)]
pub struct SuccessEnvelope<T> {
    /// 协议版本，固定为 2。
    pub v: u8,
    /// 端点数据。
    pub data: T,
    /// 与请求对应的 request_id。
    pub request_id: String,
}

impl<T> SuccessEnvelope<T> {
    /// 构造 v2 成功信封；`request_id` 必须已由调用方校验（非空、≤128 字节）。
    pub fn new(data: T, request_id: String) -> Self {
        Self {
            v: 2,
            data,
            request_id,
        }
    }
}

/// 业务错误：HTTP 状态 + 稳定错误码 + 是否可重试。
///
/// 错误码是本产品的稳定契约，浏览器与测试都按码断言；不要用 HTTP 500 掩盖可分类失败。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    /// 建议的 HTTP 状态码。
    pub status: u16,
    /// 稳定错误码。
    pub code: String,
    /// 面向调用方的说明。
    pub message: String,
    /// 是否可重试。
    pub retryable: bool,
    /// 结构化细节。
    ///
    /// 用 `Box` 承载：`ErrorDetails` 有六个字段，直接内联会让整个 `Result` 变宽
    /// （clippy::result_large_err），而错误路径并不追求零分配。
    pub details: Option<Box<ErrorDetails>>,
}

impl ApiError {
    /// 构造一个不可重试的业务错误。
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.to_owned(),
            message: message.into(),
            retryable: false,
            details: None,
        }
    }

    /// 标记为可重试（例如暂时性的资源不足）。
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// 附带结构化细节。
    pub fn with_details(mut self, details: ErrorDetails) -> Self {
        self.details = Some(Box::new(details));
        self
    }

    /// 内部错误：细节仅进日志，响应只给出稳定码。
    pub fn internal(context: &str, err: impl std::fmt::Display) -> Self {
        tracing::error!(%context, error = %err, "内部错误");
        Self::new(500, codes::INTERNAL, "internal error")
    }

    /// 转成 v2 失败信封。
    pub fn envelope(&self, request_id: &str) -> ErrorEnvelope {
        ErrorEnvelope {
            v: 2,
            error: ErrorBody {
                code: self.code.to_string(),
                message: self.message.clone(),
                retryable: self.retryable,
                details: self.details.as_deref().cloned(),
            },
            request_id: request_id.to_string(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.code, self.status, self.message)
    }
}

impl std::error::Error for ApiError {}

/// 常见错误码：各模块统一引用，避免拼写漂移。
pub mod codes {
    /// 请求语法或字段非法。
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// 缺少或错误的认证凭据。
    pub const UNAUTHENTICATED: &str = "unauthenticated";
    /// 已认证但无权访问该资源。
    pub const FORBIDDEN: &str = "forbidden";
    /// 资源不存在（对无权主体同样返回此码，避免枚举）。
    pub const NOT_FOUND: &str = "not_found";
    /// 版本或期望值冲突（乐观锁、幂等冲突）。
    pub const CONFLICT: &str = "conflict";
    /// 当前状态不允许该操作。
    pub const STATE_CONFLICT: &str = "state_conflict";
    /// 能力未实现或未在此平台启用。
    pub const UNSUPPORTED_FEATURE: &str = "unsupported_feature";
    /// 达到并发/容量上限。
    pub const CAPACITY_EXCEEDED: &str = "capacity_exceeded";
    /// 被控端采集/输入/编码失败。
    pub const DESK_ENGINE: &str = "desk_engine";
    /// 请求体超过端点上限。
    pub const PAYLOAD_TOO_LARGE: &str = "payload_too_large";
    /// 内部错误（不泄露细节给浏览器，细节只进日志）。
    pub const INTERNAL: &str = "internal";
}

/// 严格解析 JSON 请求体：拒绝重复键、拒绝尾部多余内容、限制嵌套深度。
///
/// 与 gateway 的 `runtimeconfig.checkJSON` 同语义：后出现的重复键不能掩盖前者。
/// 调用方仍需对得到的 `Value` 做字段级结构校验。
pub fn parse_json_strict(raw: &[u8]) -> Result<Value, ApiError> {
    let invalid = || ApiError::new(400, codes::INVALID_REQUEST, "invalid JSON body");
    if std::str::from_utf8(raw).is_err() {
        return Err(invalid());
    }
    let mut de = serde_json::Deserializer::from_slice(raw);
    let value = StrictValue::deserialize(&mut de).map_err(|_| invalid())?;
    de.end().map_err(|_| invalid())?;
    Ok(value.0)
}

/// 递归解析 JSON，同时在每个对象内拒绝重复键。
struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictVisitor { depth: 0 })
    }
}

/// 作为 `next_value_seed`/`next_element_seed` 使用的种子，递归沿用同一严格解析。
struct StrictSeed;

impl<'de> DeserializeSeed<'de> for StrictSeed {
    type Value = StrictValue;

    fn deserialize<D>(self, deserializer: D) -> Result<StrictValue, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        StrictValue::deserialize(deserializer)
    }
}

struct StrictVisitor {
    depth: usize,
}

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("任意 JSON 值")
    }

    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::Bool(v)))
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::Number(Number::from(v))))
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::Number(Number::from(v))))
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<StrictValue, E> {
        let number = Number::from_f64(v).ok_or_else(|| E::custom("non-finite number"))?;
        Ok(StrictValue(Value::Number(number)))
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::String(v.to_owned())))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<StrictValue, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<StrictValue, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        StrictValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<StrictValue, A::Error>
    where
        A: SeqAccess<'de>,
    {
        if self.depth >= 32 {
            return Err(serde::de::Error::custom("nesting too deep"));
        }
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(StrictSeed)? {
            items.push(item.0);
        }
        Ok(StrictValue(Value::Array(items)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<StrictValue, A::Error>
    where
        A: MapAccess<'de>,
    {
        if self.depth >= 32 {
            return Err(serde::de::Error::custom("nesting too deep"));
        }
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(serde::de::Error::custom("duplicate JSON key"));
            }
            let value = map.next_value_seed(StrictSeed)?;
            object.insert(key, value.0);
        }
        Ok(StrictValue(Value::Object(object)))
    }
}

/// 序列化 v2 信封为 JSON 字节。
pub fn to_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, ApiError> {
    serde_json::to_vec(value).map_err(|err| ApiError::internal("serialize envelope", err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_json_rejects_duplicate_keys() {
        assert!(parse_json_strict(br#"{"a":1,"a":2}"#).is_err());
        assert!(parse_json_strict(br#"{"a":{"b":1,"b":2}}"#).is_err());
        assert!(parse_json_strict(br#"[{"a":1},{"a":1}]"#).is_ok());
    }

    #[test]
    fn strict_json_rejects_trailing_content() {
        assert!(parse_json_strict(br#"{"a":1} {"b":2}"#).is_err());
        assert!(parse_json_strict(br#"{"a":1}"#).is_ok());
    }

    #[test]
    fn strict_json_accepts_nested_values() {
        let value = parse_json_strict(br#"{"a":[1,2,{"b":"c"}],"d":null,"e":1.5}"#).unwrap();
        assert_eq!(value["a"][2]["b"], "c");
        assert!(value["d"].is_null());
    }

    #[test]
    fn error_envelope_matches_contract() {
        let error = ApiError::new(404, codes::NOT_FOUND, "no such session");
        let value = serde_json::to_value(error.envelope("req-1")).unwrap();
        assert_eq!(value["v"], 2);
        assert_eq!(value["error"]["code"], "not_found");
        assert_eq!(value["error"]["retryable"], false);
        assert_eq!(value["request_id"], "req-1");
        assert!(value["error"].get("details").is_none());
    }

    #[test]
    fn success_envelope_has_v2_and_request_id() {
        let envelope = SuccessEnvelope::new(serde_json::json!({"ok": true}), "r".into());
        let value = serde_json::to_value(envelope).unwrap();
        assert_eq!(value["v"], 2);
        assert_eq!(value["data"]["ok"], true);
    }
}
