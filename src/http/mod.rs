mod auth;
mod remote;
mod response;
mod sftp;
use crate::{
    access::{AccessAuthority, LoginLimiter},
    business::BusinessHub,
    config::Config,
};
use axum::{
    extract::ConnectInfo,
    routing::{any, get, post},
    Router,
};
pub use response::ApiResponse;
use std::{net::SocketAddr, sync::Arc};
/// 应用共享状态。
pub struct AppState {
    /// 服务配置。
    pub config: Config,
    /// 访问权威。
    pub access: Arc<AccessAuthority>,
    /// 业务会话集合。
    pub business: Arc<BusinessHub>,
    /// 登录失败限流。
    pub limiter: LoginLimiter,
}

impl AppState {
    /// 产品前缀，例如 `/yoodesk`。
    pub fn product_prefix(&self) -> String {
        format!("/{}", self.config.product_id)
    }

    /// 允许的浏览器 Origin（精确匹配）。
    pub fn public_origin(&self) -> &str {
        &self.config.public_origin
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/capabilities/", get(remote::capabilities))
        .route(
            "/connection-targets/",
            get(remote::targets).post(remote::create_target),
        )
        .route(
            "/connection-targets/{target_id}/",
            axum::routing::patch(remote::change_target).delete(remote::change_target),
        )
        .route("/credentials/", post(remote::credential))
        .route("/sessions/{id}/challenges/", get(remote::challenges))
        .route(
            "/sessions/{id}/challenges/{cid}/responses/",
            post(remote::answer),
        )
        .route("/sessions/", post(remote::create))
        .route("/sessions/{id}/", get(remote::get).delete(remote::close))
        .route("/sessions/{id}/stream-tickets/", post(remote::ticket))
        .route("/sessions/{id}/stream/", any(remote::stream))
        .route("/sessions/{id}/events/", any(remote::events))
        .route(
            "/sessions/{id}/entries/",
            get(sftp::entries)
                .post(sftp::entries)
                .patch(sftp::entries)
                .delete(sftp::entries),
        )
        .route("/sessions/{id}/transfers/", post(sftp::create))
        .route(
            "/sessions/{id}/transfers/{tid}/",
            get(sftp::transfer).delete(sftp::transfer),
        )
        .route(
            "/sessions/{id}/transfers/{tid}/content/",
            get(sftp::content).patch(sftp::content),
        )
        .route(
            "/sessions/{id}/transfers/{tid}/complete/",
            post(sftp::complete),
        );
    // Keep the capability path parameter consumed by the migrated handlers.
    Router::new()
        .nest(
            &format!("{}/webclient/{{capability}}/api/v2", state.product_prefix()),
            api,
        )
        .with_state(state)
}
pub fn peer_addr(info: Option<&ConnectInfo<SocketAddr>>) -> Option<SocketAddr> {
    info.map(|ConnectInfo(addr)| *addr)
}
// A peer that stops reading must not pin a stream subscription and its capture session.
pub(crate) async fn send_ws_message(
    socket: &mut axum::extract::ws::WebSocket,
    message: axum::extract::ws::Message,
) -> Result<(), ()> {
    tokio::time::timeout(std::time::Duration::from_secs(10), socket.send(message))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}

#[cfg(test)]
#[tokio::test]
async fn websocket_write_deadline_releases_stalled_connection() {
    use axum::extract::ws::{Message, WebSocketUpgrade};
    use axum::{routing::get, Router};
    use std::time::Duration;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let done_tx = Arc::new(std::sync::Mutex::new(Some(done_tx)));
    let app = Router::new().route(
        "/",
        get(move |upgrade: WebSocketUpgrade| {
            let done_tx = done_tx.lock().unwrap().take().unwrap();
            async move {
                upgrade.on_upgrade(move |mut socket| async move {
                    // Fill the TCP receive window without relying on one frame's size.
                    let payload = axum::body::Bytes::from(vec![0; 1 << 20]);
                    let mut result = Ok(());
                    for _ in 0..256 {
                        result =
                            send_ws_message(&mut socket, Message::Binary(payload.clone())).await;
                        if result.is_err() {
                            break;
                        }
                    }
                    drop(socket);
                    let _ = done_tx.send(result);
                })
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (client, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(15), done_rx)
        .await
        .expect("stalled socket was not released")
        .unwrap();
    assert!(result.is_err());
    assert!(started.elapsed() >= Duration::from_secs(9));
    drop(client);
    server.abort();
}
