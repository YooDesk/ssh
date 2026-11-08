#![allow(unsafe_code)]
//! Each DLL owns its Tokio runtime, protocol sessions and bounded loopback transport.
use crate::{
    access::{AccessAuthority, AccessPolicy, LoginLimiter},
    business::BusinessHub,
    config::Config,
    http::AppState,
    json::ApiError,
    plugin_abi::{self, Buffer, HostApi, PluginApi, ABI_VERSION, MAX_CONTROL_BYTES},
};
use axum::{http::StatusCode, middleware, response::IntoResponse};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    ffi::c_void,
    future::IntoFuture,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::sync::watch;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    config: Config,
    service: bool,
    token: String,
}
struct Instance {
    business: Arc<BusinessHub>,
    stop: watch::Sender<bool>,
    sessions: watch::Sender<u32>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn error(message: impl Into<String>) -> ApiError {
    ApiError::new(503, "service_unavailable", message)
}
unsafe fn decode<T: serde::de::DeserializeOwned>(
    input: *const u8,
    len: usize,
) -> Result<T, ApiError> {
    if input.is_null() || len > MAX_CONTROL_BYTES {
        return Err(error("invalid plugin input"));
    }
    serde_json::from_slice(std::slice::from_raw_parts(input, len))
        .map_err(|_| error("invalid plugin request"))
}
unsafe fn response(output: *mut Buffer, result: Result<Value, ApiError>) -> i32 {
    if output.is_null() {
        return -1;
    }
    let envelope = match result {
        Ok(value) => json!({"data":value}),
        Err(error) => json!({"error":error}),
    };
    match serde_json::to_vec(&envelope) {
        Ok(bytes) => {
            *output = Buffer::owned(bytes);
            0
        }
        Err(_) => -1,
    }
}

unsafe extern "C" fn start(
    host: *const HostApi,
    input: *const u8,
    len: usize,
    handle: *mut *mut c_void,
    output: *mut Buffer,
) -> i32 {
    if host.is_null() || handle.is_null() || output.is_null() {
        return -1;
    }
    *handle = std::ptr::null_mut();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<Value, ApiError> {
            let host = *host;
            if host.abi_version != ABI_VERSION || host.struct_size != std::mem::size_of::<HostApi>()
            {
                return Err(error("host/plugin ABI mismatch"));
            }
            let mut start: Start = decode(input, len)?;
            // The legacy shared catalog seeds each plugin once; subsequent writes must be isolated.
            if !start.config.connection_profiles_file.is_empty() {
                let legacy = std::path::Path::new(&start.config.connection_profiles_file);
                let own = legacy.with_extension(format!(
                    "{}.json",
                    env!("CARGO_PKG_NAME")
                        .strip_prefix("yoodesk-")
                        .ok_or_else(|| error("invalid plugin identity"))?
                ));
                if own.is_file() {
                    let raw = std::fs::read(&own)
                        .map_err(|_| error("plugin target catalog unavailable"))?;
                    let value = crate::json::parse_json_strict(&raw)?;
                    start.config.targets = serde_json::from_value(value)
                        .map_err(|_| error("invalid plugin target catalog"))?;
                    start
                        .config
                        .validate()
                        .map_err(|_| error("invalid plugin target configuration"))?;
                }
                start.config.connection_profiles_file = own.to_string_lossy().into_owned();
            }
            if start.token.len() != 64 {
                return Err(error("invalid plugin transport token"));
            }
            let policy = AccessPolicy::new(start.config.product_id.clone(), Vec::new());
            let access = Arc::new(AccessAuthority::from_host(policy, host)?);
            let (stop, shutdown) = watch::channel(false);
            let (sessions, events) = watch::channel(0u32);
            let (ready, started) = std::sync::mpsc::sync_channel(1);
            let thread = std::thread::Builder::new().name(format!("yoodesk-{}", env!("CARGO_PKG_NAME"))).spawn(move || {
            let run = || -> Result<(), ApiError> {
                let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|_| error("plugin runtime unavailable"))?;
                runtime.block_on(async move {
                    #[cfg(feature="desk-engine")]
                    if start.service { crate::desk::engine::rpc::set_transport(None); }
                    #[cfg(feature="desk-engine")]
                    let business = Arc::new(BusinessHub::with_config(crate::desk::default_backend(), &start.config));
                    #[cfg(not(feature="desk-engine"))]
                    let business = Arc::new(BusinessHub::new());
                    let state = Arc::new(AppState { config:start.config, access, business:business.clone(), limiter:LoginLimiter::new(10.0,10.0/60.0) });
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|_| error("plugin listener unavailable"))?;
                    let address = listener.local_addr().map_err(|_| error("plugin listener address unavailable"))?;
                    let token = start.token;
                    let app = crate::http::router(state).layer(middleware::from_fn(move |mut request: axum::extract::Request, next: middleware::Next| {
                        let token = token.clone();
                        async move {
                            use subtle::ConstantTimeEq;
                            let allowed = request.headers_mut().remove("x-yoodesk-plugin-token").is_some_and(|provided| bool::from(provided.as_bytes().ct_eq(token.as_bytes())));
                            if !allowed { return StatusCode::FORBIDDEN.into_response(); }
                            // All identity-bearing headers were checked by the public/Connector host boundary.
                            next.run(request).await
                        }
                    }));
                    let _ = ready.send((address, business.clone()));
                    #[cfg(all(windows, feature="desk-engine"))]
                    let worker = if start.service { Some(tokio::spawn(crate::windows_worker_host::supervise(business.clone(), shutdown.clone(), events))) } else { None };
                    #[cfg(all(target_os="linux", feature="desk-engine"))]
                    let worker = if start.service { Some(tokio::spawn(crate::linux_worker_host::supervise(business.clone(), shutdown.clone(), events))) } else { None };
                    #[cfg(not(feature="desk-engine"))]
                    let _ = events;
                    let server = axum::serve(listener, app).with_graceful_shutdown(wait_stop(shutdown.clone())).into_future();
                    tokio::pin!(server);
                    let result = tokio::select! {
                        result=&mut server => result.map_err(|_|error("plugin HTTP server failed")),
                        _=wait_stop(shutdown.clone()) => {
                            business.remote.close_all();
                            #[cfg(feature="desk-engine")] business.desk.close_all("server_shutdown");
                            let _ = tokio::time::timeout(Duration::from_secs(15), &mut server).await;
                            Ok(())
                        }
                    };
                    #[cfg(feature="desk-engine")]
                    if let Some(worker) = worker { let _ = tokio::time::timeout(Duration::from_secs(5),worker).await; }
                    result
                })
            };
            if let Err(error) = run() { tracing::error!(%error.message, "plugin runtime stopped"); }
        }).map_err(|_| error("plugin thread unavailable"))?;
            let (address, business) = match started.recv_timeout(Duration::from_secs(30)) {
                Ok(value) => value,
                _ => {
                    stop.send_replace(true);
                    let _ = thread.join();
                    return Err(error("plugin startup failed"));
                }
            };
            *handle = Box::into_raw(Box::new(Instance {
                business,
                stop,
                sessions,
                thread: Some(thread),
            })) as *mut c_void;
            Ok(json!({"address":address.to_string()}))
        },
    ));
    response(
        output,
        result.unwrap_or_else(|_| Err(error("plugin startup panicked"))),
    )
}

async fn wait_stop(mut stop: watch::Receiver<bool>) {
    while !*stop.borrow() {
        if stop.changed().await.is_err() {
            break;
        }
    }
}

unsafe extern "C" fn command(
    handle: *mut c_void,
    input: *const u8,
    len: usize,
    output: *mut Buffer,
) -> i32 {
    if handle.is_null() {
        return -1;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<Value, ApiError> {
            let instance = &*(handle as *const Instance);
            let request: Value = decode(input, len)?;
            let id = request["id"].as_u64().unwrap_or(0);
            let reason = request["reason"].as_str().unwrap_or("access_revoked");
            match request["op"].as_str() {
                Some("close.owner") => {
                    instance.business.remote.close_owner(id);
                    #[cfg(feature = "desk-engine")]
                    instance.business.desk.close_all_for(id, reason);
                }
                Some("close.all") => {
                    instance.business.remote.close_all();
                    #[cfg(feature = "desk-engine")]
                    instance.business.desk.close_all("server_shutdown");
                }
                Some("access.renew") => {
                    #[cfg(feature = "desk-engine")]
                    {
                        let expires: SystemTime =
                            serde_json::from_value(request["expires"].clone())
                                .map_err(|_| error("invalid access deadline"))?;
                        instance.business.desk.update_access_expiry_for(
                            id,
                            expires,
                            request["revision"].as_u64().unwrap_or(0),
                        );
                    }
                }
                Some("session.event") => {
                    instance.sessions.send_replace(
                        u32::try_from(id).map_err(|_| error("invalid session event"))?,
                    );
                }
                Some("ready") => {
                    #[cfg(feature = "desk-engine")]
                    return Ok(json!(
                        instance.business.desk.backend().capabilities().capture
                    ));
                    #[cfg(not(feature = "desk-engine"))]
                    return Ok(json!(true));
                }
                #[cfg(feature = "desk-engine")]
                Some("chat.rooms") => return Ok(instance.business.desk.chat_rooms()),
                #[cfg(feature = "desk-engine")]
                Some("chat.reply") => {
                    let conversation = request["conversation"]
                        .as_str()
                        .ok_or_else(|| error("invalid conversation"))?;
                    let id: uuid::Uuid = serde_json::from_value(request["id"].clone())
                        .map_err(|_| error("invalid message id"))?;
                    let text = request["text"]
                        .as_str()
                        .ok_or_else(|| error("invalid message"))?;
                    return instance.business.desk.chat_reply(conversation, id, text);
                }
                _ => return Err(error("unsupported plugin operation")),
            }
            let _ = reason;
            Ok(Value::Null)
        },
    ));
    response(
        output,
        result.unwrap_or_else(|_| Err(error("plugin command panicked"))),
    )
}

unsafe extern "C" fn stop(handle: *mut c_void) {
    if handle.is_null() {
        return;
    }
    let mut instance = Box::from_raw(handle as *mut Instance);
    instance.stop.send_replace(true);
    if let Some(thread) = instance.thread.take() {
        if thread.join().is_err() {
            tracing::error!("plugin runtime panicked during shutdown");
        }
    }
}

unsafe extern "C" fn worker(input: *const u8, len: usize, output: *mut Buffer) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<Value, ApiError> {
            let args: Vec<String> = decode(input, len)?;
            #[cfg(feature = "desk-engine")]
            {
                let mode = args.first().map(String::as_str).unwrap_or("");
                #[cfg(target_os = "linux")]
                match mode {
                    "--x11-capture-probe" => return Ok(json!(crate::linux_x11_probe::run())),
                    "--user-input-service" => {
                        return Ok(json!(crate::desk::engine::run_user_input_service()))
                    }
                    "--wayland-display-probe" => {
                        return Ok(json!(crate::desk::engine::run_wayland_display_probe()))
                    }
                    _ => {}
                }
                let flag = |name: &str| {
                    args.windows(2)
                        .find(|pair| pair[0] == name)
                        .map(|pair| pair[1].as_str())
                        .ok_or_else(|| error("missing desk worker argument"))
                };
                let pipe = flag("--pipe")?;
                let parent = flag("--parent-pid")?
                    .parse::<u32>()
                    .map_err(|_| error("invalid worker parent"))?;
                #[cfg(windows)]
                let result = crate::windows_worker::run_worker(pipe, parent);
                #[cfg(target_os = "linux")]
                let result = if mode == "--rustdesk-privileged-worker" {
                    crate::unix_worker::run_privileged_worker(pipe, parent)
                } else {
                    crate::unix_worker::run_worker(pipe, parent)
                };
                result.map_err(|_| error("desk worker failed"))?;
                return Ok(json!(0));
            }
            #[cfg(not(feature = "desk-engine"))]
            {
                let _ = args;
                Err(error("this plugin has no desktop worker"))
            }
        },
    ));
    response(
        output,
        result.unwrap_or_else(|_| Err(error("desk worker panicked"))),
    )
}

static API: PluginApi = PluginApi {
    abi_version: ABI_VERSION,
    struct_size: std::mem::size_of::<PluginApi>(),
    id: PLUGIN_ID.as_ptr() as *const std::ffi::c_char,
    start,
    command,
    stop,
    free: plugin_abi::free,
    worker,
};

#[no_mangle]
pub extern "C" fn yd_plugin_v1() -> *const PluginApi {
    &API
}

// The repository-specific ID is appended by tools/configure_plugins.py.

const PLUGIN_ID: &[u8] = b"ssh\0";
