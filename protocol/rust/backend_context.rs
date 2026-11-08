use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::hmac;
use serde::{Deserialize, Serialize};

pub const HEADER: &str = "x-raccess-backend-context";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub session_id: String,
    pub generation: String,
    pub capability: String,
    pub target_id: String,
    pub scopes: Vec<String>,
    pub expires: u64,
    pub method: String,
    pub path: String,
}

fn key(secret: &[u8]) -> Option<hmac::Key> {
    (secret.len() == 32).then(|| hmac::Key::new(hmac::HMAC_SHA256, secret))
}

pub fn sign(secret: &[u8], context: &Context) -> Option<String> {
    let key = key(secret)?;
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(context).ok()?);
    if payload.len() > 8192 {
        return None;
    }
    let tag = hmac::sign(&key, payload.as_bytes());
    Some(format!(
        "{payload}.{}",
        URL_SAFE_NO_PAD.encode(tag.as_ref())
    ))
}

pub fn verify(secret: &[u8], value: &str, method: &str, path: &str, now: u64) -> Option<Context> {
    if value.len() > 8240 {
        return None;
    }
    let (payload, tag) = value.split_once('.')?;
    hmac::verify(
        &key(secret)?,
        payload.as_bytes(),
        &URL_SAFE_NO_PAD.decode(tag).ok()?,
    )
    .ok()?;
    let context: Context = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    if context.method != method
        || context.path != path
        || context.expires <= now
        || context.expires > now.saturating_add(15 * 60)
        || context.session_id.is_empty()
        || context.session_id.len() > 128
        || context.generation.is_empty()
        || context.generation.len() > 32
        || context.target_id.is_empty()
        || context.target_id.len() > 128
        || context.scopes.is_empty()
        || context.scopes.len() > 32
    {
        return None;
    }
    Some(context)
}
