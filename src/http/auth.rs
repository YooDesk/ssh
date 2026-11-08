//! Request credentials are parsed here; the host callback owns the access state.
use super::AppState;
use crate::access::{AccessContext, AccessMode};
use crate::json::{codes, ApiError};
use axum::http::{header, HeaderMap};
/// 从 Cookie 或 Bearer 解析当前访问会话。
pub(crate) fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AccessContext, ApiError> {
    let context = authenticate_credentials(state, headers)?;
    if let Some(expected) = headers.get("x-access-instance") {
        if expected.to_str().ok() != Some(state.access.instance_id(&context)?.as_str()) {
            return Err(ApiError::new(
                403,
                codes::FORBIDDEN,
                "access session instance changed",
            ));
        }
    }
    Ok(context)
}

fn authenticate_credentials(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AccessContext, ApiError> {
    // 同时提供 Bearer 和 Cookie 时，Bearer 优先但仍需模式匹配；
    // 坏 token 不得回退 Cookie（设计 §3）。
    if let Some(authorization) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(token) = authorization.strip_prefix("Bearer ") {
            return state.access.authenticate_token(token, AccessMode::Bearer);
        }
        return Err(ApiError::new(
            401,
            codes::UNAUTHENTICATED,
            "unsupported authorization scheme",
        ));
    }
    let cookies = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(unauthorized)?;
    let value =
        cookie_value(cookies, &state.access.policy().session_cookie()).ok_or_else(unauthorized)?;
    state.access.authenticate_token(&value, AccessMode::Cookie)
}

fn unauthorized() -> ApiError {
    ApiError::new(401, codes::UNAUTHENTICATED, "access session required")
}

/// 从 Cookie 头里取指定名称的值；名称精确匹配，不做前缀或大小写模糊。
pub(crate) fn cookie_value(cookies: &str, name: &str) -> Option<String> {
    cookies.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key.trim() == name {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_value_requires_exact_name() {
        let cookies = "a=1; __Host-yoodesk_business_session=s.2; b=3";
        assert_eq!(
            cookie_value(cookies, "__Host-yoodesk_business_session").as_deref(),
            Some("s.2")
        );
        assert_eq!(
            cookie_value(cookies, "__Host-yoodesk_business_sessio"),
            None
        );
        assert_eq!(cookie_value("", "x"), None);
    }
}
