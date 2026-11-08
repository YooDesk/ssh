//! 统一响应构造：JSON 信封、安全头与 204 例外。
//!
//! 设计 §3：JSON 成功 `{v:2,data,request_id}`，失败 `{v:2,error,...}`；只有 101/204/octet-stream
//! 明确例外。所有响应都加 `nosniff`、`no-store`，页面另加 CSP 与 `frame-ancestors`。

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::json::{codes, to_json_bytes, ApiError, SuccessEnvelope};

/// 统一 JSON 响应包装。
pub struct ApiResponse {
    status: StatusCode,
    body: Vec<u8>,
}

impl IntoResponse for ApiResponse {
    fn into_response(self) -> Response {
        let mut response = Response::new(axum::body::Body::from(self.body));
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        response
    }
}

/// 统一构造成功响应。
pub fn success<T: serde::Serialize>(data: T, request_id: &str) -> ApiResponse {
    match to_json_bytes(&SuccessEnvelope::new(data, request_id.to_string())) {
        Ok(body) => ApiResponse {
            status: StatusCode::OK,
            body,
        },
        Err(err) => failure(&err, request_id),
    }
}

/// 统一构造失败响应。
pub fn failure(error: &ApiError, request_id: &str) -> ApiResponse {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_json_bytes(&error.envelope(request_id)).unwrap_or_else(|_| {
        // 极端情况下信封本身序列化失败：给出最小可用 JSON，仍保持契约形状。
        br#"{"v":2,"error":{"code":"internal","message":"internal error","retryable":false},"request_id":"unknown"}"#
            .to_vec()
    });
    ApiResponse { status, body }
}

/// 构造 204 响应（无 JSON envelope）。
pub fn no_content() -> Response {
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// 校验 `X-Request-ID`：缺失时返回明确错误（写操作必须显式）。
pub fn require_request_id(value: Option<&HeaderValue>) -> Result<String, ApiError> {
    let value = value.ok_or_else(|| {
        ApiError::new(400, codes::INVALID_REQUEST, "X-Request-ID header required")
    })?;
    let text = value
        .to_str()
        .map_err(|_| ApiError::new(400, codes::INVALID_REQUEST, "invalid X-Request-ID"))?;
    validate_request_id(text)?;
    Ok(text.to_string())
}

/// 读请求可省略 `X-Request-ID`，此时生成一个用于回显的 id。
pub fn request_id_or_generated(value: Option<&HeaderValue>) -> Result<String, ApiError> {
    match value {
        None => Ok(uuid::Uuid::new_v4().to_string()),
        Some(header) => require_request_id(Some(header)),
    }
}

fn validate_request_id(text: &str) -> Result<(), ApiError> {
    if text.is_empty() || text.len() > 128 || !text.is_ascii() {
        return Err(ApiError::new(
            400,
            codes::INVALID_REQUEST,
            "X-Request-ID must be 1..=128 ASCII characters",
        ));
    }
    Ok(())
}

/// 把页面所需的安全头（CSP、frame-ancestors、referrer）加到响应上。
///
/// CSP 只允许同源脚本与样式；iframe 仅允许同源嵌入。外部父域白名单是部署配置项，
/// 本批次不放宽，避免“默认允许任意嵌入”这类静默风险。
pub fn page_headers(response: &mut Response) {
    const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
         img-src 'self' data: blob:; connect-src 'self' ws: wss:; \
         frame-ancestors 'self'; base-uri 'none'; form-action 'self'";
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(CSP) {
        headers.insert(header::CONTENT_SECURITY_POLICY, value);
    }
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_validation() {
        assert!(validate_request_id("abc-123").is_ok());
        assert!(validate_request_id("").is_err());
        assert!(validate_request_id(&"a".repeat(129)).is_err());
        assert!(validate_request_id("中文").is_err());
    }

    #[test]
    fn failure_envelope_shape() {
        let error = ApiError::new(404, codes::NOT_FOUND, "x");
        let response = failure(&error, "req");
        assert_eq!(response.status, StatusCode::NOT_FOUND);
        let value: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(value["v"], 2);
        assert_eq!(value["error"]["code"], "not_found");
        assert_eq!(value["request_id"], "req");
    }

    #[test]
    fn success_envelope_shape() {
        let response = success(serde_json::json!({"a": 1}), "req");
        assert_eq!(response.status, StatusCode::OK);
        let value: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(value["data"]["a"], 1);
        assert_eq!(value["request_id"], "req");
    }
}
