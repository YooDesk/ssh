#![allow(unsafe_code)]
//! A plugin never duplicates the host's authentication state.
use crate::{
    json::ApiError,
    plugin_abi::{Buffer, HostApi},
};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Clone, Copy)]
pub struct RemoteAccess(pub HostApi);

impl RemoteAccess {
    pub fn call<T: DeserializeOwned>(&self, request: Value) -> Result<T, ApiError> {
        let unavailable = || {
            ApiError::new(
                503,
                "service_unavailable",
                "host access authority unavailable",
            )
        };
        let input = serde_json::to_vec(&request).map_err(|_| unavailable())?;
        let mut output = Buffer::empty();
        let status =
            unsafe { (self.0.access)(self.0.context, input.as_ptr(), input.len(), &mut output) };
        let parsed = unsafe {
            output
                .bytes()
                .map(|bytes| serde_json::from_slice::<Value>(bytes))
        };
        unsafe {
            (self.0.free)(output);
        }
        if status != 0 {
            return Err(unavailable());
        }
        let mut value = parsed
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
        if let Some(error) = value.get_mut("error") {
            return Err(serde_json::from_value(error.take()).map_err(|_| unavailable())?);
        }
        let data = value.get_mut("data").ok_or_else(unavailable)?.take();
        serde_json::from_value(data).map_err(|_| unavailable())
    }
}
