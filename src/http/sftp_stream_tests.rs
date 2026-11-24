use super::*;
use crate::access::{AccessAuthority, AccessMode, AccessPolicy, LocalPrincipal, SystemClock};
use russh_sftp::{
    protocol::{Attrs, Data, FileAttributes, Handle, OpenFlags, Status, StatusCode as SftpStatus},
    server::Handler,
};
use sha2::Digest;
use std::sync::{
    atomic::{AtomicU32, AtomicUsize, Ordering},
    Mutex,
};

struct Storage {
    bytes: Mutex<Vec<u8>>,
    version: AtomicU32,
    writes: AtomicUsize,
}
struct Server(Arc<Storage>);
impl Handler for Server {
    type Error = SftpStatus;
    fn unimplemented(&self) -> SftpStatus {
        SftpStatus::OpUnsupported
    }
    async fn open(
        &mut self,
        id: u32,
        _: String,
        _: OpenFlags,
        _: FileAttributes,
    ) -> Result<Handle, SftpStatus> {
        Ok(Handle {
            id,
            handle: "file".into(),
        })
    }
    async fn stat(&mut self, id: u32, _: String) -> Result<Attrs, SftpStatus> {
        Ok(Attrs {
            id,
            attrs: FileAttributes {
                size: Some(self.0.bytes.lock().unwrap().len() as u64),
                mtime: Some(self.0.version.load(Ordering::SeqCst)),
                permissions: Some(0o100600),
                ..Default::default()
            },
        })
    }
    async fn read(
        &mut self,
        id: u32,
        _: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, SftpStatus> {
        let bytes = self.0.bytes.lock().unwrap();
        let start = offset as usize;
        if start >= bytes.len() {
            return Err(SftpStatus::Eof);
        }
        Ok(Data {
            id,
            data: bytes[start..bytes.len().min(start + len as usize)]
                .to_vec()
                .into(),
        })
    }
    async fn write(
        &mut self,
        id: u32,
        _: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, SftpStatus> {
        self.0.writes.fetch_add(1, Ordering::SeqCst);
        let mut bytes = self.0.bytes.lock().unwrap();
        let end = offset as usize + data.len();
        if bytes.len() < end {
            bytes.resize(end, 0);
        }
        bytes[offset as usize..end].copy_from_slice(&data);
        Ok(ok(id))
    }
    async fn close(&mut self, id: u32, _: String) -> Result<Status, SftpStatus> {
        Ok(ok(id))
    }
}
fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: SftpStatus::Ok,
        error_message: String::new(),
        language_tag: String::new(),
    }
}

struct Fixture {
    state: Arc<AppState>,
    headers: HeaderMap,
    session: Arc<RemoteSession>,
    storage: Arc<Storage>,
    peer: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.peer.abort();
    }
}
impl Fixture {
    async fn new(bytes: Vec<u8>, scopes: &[&str]) -> Self {
        let storage = Arc::new(Storage {
            bytes: Mutex::new(bytes),
            version: AtomicU32::new(7),
            writes: AtomicUsize::new(0),
        });
        let (client, peer) = tokio::io::duplex(65536);
        let server = Server(storage.clone());
        let peer = tokio::spawn(async move {
            russh_sftp::server::run(peer, server).await;
        });
        let ftp = Arc::new(russh_sftp::client::SftpSession::new(client).await.unwrap());
        let config = serde_json::from_value(json!({"product_id":"test","public_origin":"https://localhost","bind":"127.0.0.1:0","certificate":"","private_key":"","access_key_file":""})).unwrap();
        let access = Arc::new(
            AccessAuthority::new(
                AccessPolicy::new(
                    "test",
                    vec![LocalPrincipal {
                        principal_id: "test".into(),
                        scopes: scopes.iter().map(|s| s.to_string()).collect(),
                    }],
                ),
                "0123456789abcdef",
                Arc::new(SystemClock),
            )
            .unwrap(),
        );
        let (ticket, csrf) = access.begin_preauth(AccessMode::Bearer).unwrap();
        let login = access
            .login(&ticket, &csrf, "0123456789abcdef", AccessMode::Bearer)
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", login.access_token.unwrap())
                .parse()
                .unwrap(),
        );
        headers.insert("origin", "https://localhost".parse().unwrap());
        headers.insert("x-csrf-token", login.info.csrf_token.parse().unwrap());
        headers.insert("x-request-id", "stream-test".parse().unwrap());
        let state = Arc::new(AppState {
            config,
            access,
            limiter: crate::access::LoginLimiter::new(100.0, 100.0),
            business: Arc::new(crate::business::BusinessHub::new()),
        });
        let target = serde_json::from_value(json!({"id":"test","label":"Test","protocol":"ssh","host":"127.0.0.1","port":22,"root":"/"})).unwrap();
        let session = state
            .business
            .remote
            .create(
                authorize(&state, &headers, false).unwrap(),
                target,
                vec![],
                "test".into(),
                "test".into(),
                1,
            )
            .unwrap();
        *session.sftp.lock().await = Some(ftp);
        Self {
            state,
            headers,
            session,
            storage,
            peer,
        }
    }
    async fn transfer(&self, direction: &str, total: usize) {
        let ftp = self.session.sftp.lock().await.clone().unwrap();
        let file = ftp
            .open_with_flags("/file", OpenFlags::READ | OpenFlags::WRITE)
            .await
            .unwrap();
        let t = sftp::Transfer {
            id: "transfer".into(),
            direction: direction.into(),
            path: "/file".into(),
            temporary: None,
            file: Some(file),
            offset: 0,
            total: total as u64,
            state: "active".into(),
            version: format!("{}:7", total),
            hash: sha2::Sha256::new(),
            last: None,
            read_position_valid: false,
        };
        self.session
            .transfers
            .0
            .lock()
            .await
            .insert("transfer".into(), Arc::new(tokio::sync::Mutex::new(t)));
    }
    async fn content(&self, method: Method, body: Body) -> Response {
        let mut headers = self.headers.clone();
        if method == Method::PATCH {
            headers.insert("upload-stream", "1".parse().unwrap());
            headers.insert("upload-offset", "0".parse().unwrap());
        }
        content(
            State(self.state.clone()),
            Path(TransferPath {
                id: self.session.id.to_string(),
                tid: "transfer".into(),
            }),
            method,
            headers,
            Request::new(body),
        )
        .await
    }
}

#[tokio::test]
async fn multi_block_upload_and_download_preserve_bytes() {
    let expected: Vec<u8> = (0..3 * 1024 * 1024 + 123)
        .map(|i| (i % 251) as u8)
        .collect();
    let f = Fixture::new(vec![], &["session.connect", "files.write", "files.read"]).await;
    f.transfer("upload", expected.len()).await;
    let chunks: Vec<_> = expected
        .chunks(16384)
        .map(|b| Ok::<_, std::io::Error>(Bytes::copy_from_slice(b)))
        .collect();
    let response = f
        .content(Method::PATCH, Body::from_stream(stream::iter(chunks)))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(*f.storage.bytes.lock().unwrap(), expected);
    assert!(f.storage.writes.load(Ordering::SeqCst) > 3);
    f.transfer("download", expected.len()).await;
    let response = f.content(Method::GET, Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-length"],
        expected.len().to_string()
    );
    let actual = axum::body::to_bytes(response.into_body(), expected.len())
        .await
        .unwrap();
    assert_eq!(actual.as_ref(), expected);
}

#[tokio::test]
async fn streamed_download_rechecks_source_and_session_between_blocks() {
    for cancel in [false, true] {
        let f = Fixture::new(vec![42; 3 << 20], &["session.connect", "files.read"]).await;
        f.transfer("download", 3 << 20).await;
        let response = f.content(Method::GET, Body::empty()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap().len(), 1 << 20);
        if cancel {
            f.session.close();
        } else {
            f.storage.version.store(8, Ordering::SeqCst);
        }
        assert!(body.next().await.unwrap().is_err());
    }
}

#[tokio::test]
async fn streamed_upload_denies_missing_permission_before_remote_write() {
    let f = Fixture::new(vec![], &["session.connect", "files.read"]).await;
    f.transfer("upload", 3 << 20).await;
    assert_eq!(
        f.content(Method::PATCH, Body::from(vec![1; 3 << 20]))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(f.storage.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stalled_streamed_upload_stops_when_session_closes() {
    let f = Fixture::new(vec![], &["session.connect", "files.write"]).await;
    f.transfer("upload", 3 << 20).await;
    let body = Body::from_stream(stream::pending::<Result<Bytes, std::io::Error>>());
    let request = f.content(Method::PATCH, body);
    tokio::pin!(request);
    assert!(futures_util::poll!(&mut request).is_pending());
    f.session.close();
    let response = tokio::time::timeout(std::time::Duration::from_secs(1), request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(f.storage.writes.load(Ordering::SeqCst), 0);
}
