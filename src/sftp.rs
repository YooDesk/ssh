//! SSH 会话内文件操作与分块传输。路径按远端 POSIX 语义处理，不使用宿主机路径规则。
use crate::json::ApiError;
use russh_sftp::{
    client::{error::Error as SftpError, fs::File, RawSftpSession, SftpSession},
    protocol::{FileAttributes, OpenFlags, StatusCode},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// 每个会话独立、有界的传输资源。
pub type TransferSlot = std::sync::Arc<tokio::sync::Mutex<Transfer>>;

/// The session registry and its separate creation gate.
#[derive(Default)]
pub struct Transfers(
    pub tokio::sync::Mutex<HashMap<String, TransferSlot>>,
    pub(crate) std::sync::Arc<tokio::sync::Mutex<()>>,
);
/// 一个有状态的上传或下载。
pub struct Transfer {
    /// 传输编号。
    pub id: String,
    /// upload/download。
    pub direction: String,
    /// 真实目标路径。
    pub path: String,
    /// 上传临时路径。
    pub temporary: Option<String>,
    /// 打开的远端文件句柄。
    pub file: Option<File>,
    /// 确认偏移。
    pub offset: u64,
    /// 文件总字节。
    pub total: u64,
    /// 当前状态。
    pub state: String,
    /// 下载源版本。
    pub version: String,
    /// 上传增量哈希。
    pub hash: Sha256,
    /// 已确认的分块，用于重复 offset 的内容校验。
    pub last: Option<(u64, Vec<u8>)>,
}
impl Transfer {
    /// 不公开远端文件句柄和临时名。
    pub fn info(&self) -> Value {
        json!({"transfer_id":self.id,"offset":self.offset.to_string(),"total":self.total.to_string(),"state":if self.state=="active"{"running"}else if self.state=="publication_unknown"{"failed"}else{&self.state},"source_version":if self.version.is_empty(){None}else{Some(&self.version)},"result_path":if self.state=="completed"{Some(&self.path)}else{None},"verified_sha256":if self.state=="completed"&&self.direction=="upload"{Some(format!("{:x}",self.hash.clone().finalize()))}else{None}})
    }
}
pub(crate) fn error(_: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        502,
        "upstream_error",
        "SFTP operation failed; check permissions, path and remote connection",
    )
}
fn invalid() -> ApiError {
    ApiError::new(400, "invalid_request", "invalid remote path")
}
const UPLOAD_STAGING_PREFIX: &str = ".remote-upload-";

fn is_upload_staging(name: &str) -> bool {
    name.as_bytes()
        .get(..UPLOAD_STAGING_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(UPLOAD_STAGING_PREFIX.as_bytes()))
}

fn contains_upload_staging(path: &str) -> bool {
    path.split('/').any(is_upload_staging)
}

/// 浏览器传相对根路径；拒绝 .、..、NUL 与反斜杠。
pub fn relative(path: &str) -> Result<String, ApiError> {
    if contains_upload_staging(path)
        || path.len() > 4096
        || path.contains(['\0', '\\'])
        || path.starts_with('/')
        || path.split('/').any(|c| c == ".." || c == ".")
    {
        return Err(invalid());
    }
    Ok(path.trim_end_matches('/').to_string())
}
fn within(root: &str, path: &str) -> bool {
    root == "/" || path == root || path.starts_with(&format!("{root}/"))
}
/// canonicalize 对既有对象或新对象父目录执行，防止符号链接越过配置根。
pub async fn path(
    sftp: &SftpSession,
    root: &str,
    path: &str,
    new: bool,
) -> Result<String, ApiError> {
    let relative = relative(path)?;
    let root = sftp.canonicalize(root).await.map_err(error)?;
    let root = root.trim_end_matches('/');
    let root = if root.is_empty() { "/" } else { root };
    let full = if relative.is_empty() {
        root.to_string()
    } else {
        format!("{}/{relative}", root.trim_end_matches('/'))
    };
    let check = if new {
        full.rsplit_once('/')
            .map(|(p, _)| if p.is_empty() { "/" } else { p })
            .unwrap_or("/")
    } else {
        &full
    };
    let real = sftp.canonicalize(check).await.map_err(error)?;
    if !within(root, &real) || contains_upload_staging(&real) {
        return Err(ApiError::new(
            403,
            "forbidden",
            "path escapes configured root",
        ));
    }
    Ok(if new {
        format!(
            "{}/{}",
            real.trim_end_matches('/'),
            full.rsplit('/').next().unwrap()
        )
    } else {
        real
    })
}
fn version(m: &russh_sftp::client::fs::Metadata) -> String {
    format!("{}:{}", m.size.unwrap_or(0), m.mtime.unwrap_or(0))
}

const MAX_LIST_ENTRIES: usize = 4096;
const MAX_LIST_BATCHES: usize = 256;
static DIRECTORY_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

// Serialize tree moves against staging creation across WebService SSH sessions.
static TREE_MUTATION: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

fn tree_busy() -> ApiError {
    ApiError::new(
        409,
        "state_conflict",
        "another remote tree operation is in progress",
    )
}

fn listing_limit() -> ApiError {
    ApiError::new(
        422,
        "capacity_exceeded",
        "remote directory exceeds listing limit",
    )
}

// Read one bounded SFTP packet at a time; never buffer the library's eager full listing.
async fn bounded_directory(
    raw: &RawSftpSession,
    path: String,
) -> Result<Vec<(String, FileAttributes)>, ApiError> {
    let handle = raw.opendir(path).await.map_err(error)?.handle;
    let result = async {
        let mut entries = Vec::new();
        for _ in 0..MAX_LIST_BATCHES {
            match raw.readdir(handle.as_str()).await {
                Ok(batch) => {
                    for item in batch.files {
                        let name = item.filename;
                        if name == "." || name == ".." {
                            continue;
                        }
                        if entries.len() == MAX_LIST_ENTRIES || name.len() > 4096 {
                            return Err(listing_limit());
                        }
                        if name.is_empty() || name.contains(['/', '\\', '\0']) {
                            return Err(error("invalid remote directory entry"));
                        }
                        entries.push((name, item.attrs));
                    }
                }
                Err(SftpError::Status(status)) if status.status_code == StatusCode::Eof => {
                    return Ok(entries)
                }
                Err(err) => return Err(error(err)),
            }
        }
        Err(listing_limit())
    }
    .await;
    let closed = raw.close(handle).await.map_err(error);
    let entries = result?;
    closed?;
    Ok(entries)
}

async fn open_directory_channel(
    session: &crate::business::remote::RemoteSession,
) -> Result<RawSftpSession, ApiError> {
    let mut channel = {
        let handles = session.ssh.lock().await;
        handles
            .last()
            .ok_or_else(|| error("SSH connection closed"))?
            .channel_open_session()
            .await
            .map_err(error)?
    };
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(error)?;
    crate::ssh::wait_channel_request(&mut channel)
        .await
        .map_err(error)?;
    let raw = RawSftpSession::new(channel.into_stream());
    raw.init().await.map_err(error)?;
    Ok(raw)
}

/// List one page after a bounded remote directory scan.
pub async fn list(
    session: &crate::business::remote::RemoteSession,
    sftp: &SftpSession,
    root: &str,
    p: &str,
    cursor: usize,
) -> Result<Value, ApiError> {
    if cursor > MAX_LIST_ENTRIES {
        return Err(invalid());
    }
    let _slot = DIRECTORY_SLOTS
        .try_acquire()
        .map_err(|_| ApiError::new(429, "capacity_exceeded", "too many remote directory scans"))?;
    let work = async {
        let full = path(sftp, root, p, false).await?;
        let raw = open_directory_channel(session).await?;
        bounded_directory(&raw, full).await
    };
    let mut entries = tokio::select! {
        biased;
        _ = session.cancelled() => return Err(ApiError::new(409, "state_conflict", "SSH session closed")),
        result = tokio::time::timeout(std::time::Duration::from_secs(15), work) =>
            result.map_err(|_| ApiError::new(504, "upstream_error", "remote directory scan timed out"))??,
    };
    entries.retain(|(name, _)| !is_upload_staging(name));
    entries.sort_by(|(left_name, left), (right_name, right)| {
        (!left.file_type().is_dir(), left_name).cmp(&(!right.file_type().is_dir(), right_name))
    });
    let total = entries.len();
    if cursor > total {
        return Err(invalid());
    }
    let items: Vec<_> = entries.into_iter().skip(cursor).take(128).map(|(name, m)| {
        let file_type = m.file_type();
        json!({"name":name,"path":if p.is_empty(){name.clone()}else{format!("{p}/{name}")},
            "type":if file_type.is_dir(){"directory"}else if file_type.is_symlink(){"symlink"}else{"file"},
            "size":m.size.unwrap_or(0).to_string(),
            "mtime":crate::timefmt::format_rfc3339(std::time::SystemTime::UNIX_EPOCH+std::time::Duration::from_secs(m.mtime.unwrap_or(0)as u64)),
            "version":version(&m)})
    }).collect();
    Ok(
        json!({"items":items,"next_cursor":if cursor+128<total{Some((cursor+128).to_string())}else{None}}),
    )
}
/// 新建目录，禁止配置根操作。
pub async fn mkdir(sftp: &SftpSession, root: &str, p: &str) -> Result<(), ApiError> {
    if p.is_empty() {
        return Err(invalid());
    }
    let full = path(sftp, root, p, true).await?;
    sftp.create_dir(full).await.map_err(error)
}
/// 标准 SFTP rename 不覆盖既有文件，显式避免数据丢失。
pub async fn rename(
    session: &crate::business::remote::RemoteSession,
    sftp: &SftpSession,
    root: &str,
    from: &str,
    to: &str,
) -> Result<(), ApiError> {
    if relative(from)?.is_empty() || relative(to)?.is_empty() {
        return Err(invalid());
    }
    let _tree = TREE_MUTATION.try_write().map_err(|_| tree_busy())?;
    let work = async {
        let from = path(sftp, root, from, true).await?;
        let to = path(sftp, root, to, true).await?;
        if sftp.try_exists(&to).await.map_err(error)? {
            return Err(ApiError::new(
                409,
                "state_conflict",
                "destination already exists",
            ));
        }
        if sftp.symlink_metadata(&from).await.map_err(error)?.is_dir() {
            let raw = open_directory_channel(session).await?;
            return rename_tree(&raw, from, to).await;
        }
        sftp.rename(from, to).await.map_err(error)
    };
    tokio::select! {
        biased;
        _ = session.cancelled() => Err(ApiError::new(409, "state_conflict", "SSH session closed")),
        result = tokio::time::timeout(std::time::Duration::from_secs(30), work) =>
            result.map_err(|_| ApiError::new(504, "upstream_error", "remote rename timed out; inspect the destination"))?,
    }
}
async fn rename_tree(raw: &RawSftpSession, from: String, to: String) -> Result<(), ApiError> {
    delete_plan(raw, from.clone()).await?;
    raw.rename(from, to).await.map(|_| ()).map_err(error)
}

const MAX_DELETE_ENTRIES: usize = 4096;
const MAX_DELETE_DEPTH: usize = 64;

async fn delete_plan(raw: &RawSftpSession, root: String) -> Result<Vec<(String, bool)>, ApiError> {
    let mut pending = vec![(root, 0)];
    let mut plan = Vec::new();
    while let Some((path, depth)) = pending.pop() {
        let directory = raw
            .lstat(path.as_str())
            .await
            .map_err(error)?
            .attrs
            .is_dir();
        if directory {
            let entries = bounded_directory(raw, path.clone()).await?;
            if plan.len() + pending.len() + entries.len() >= MAX_DELETE_ENTRIES
                || (!entries.is_empty() && depth == MAX_DELETE_DEPTH)
            {
                return Err(listing_limit());
            }
            for (name, _) in entries {
                if is_upload_staging(&name) {
                    return Err(ApiError::new(
                        409,
                        "state_conflict",
                        "directory contains an upload staging file",
                    ));
                }
                let child = format!("{path}/{name}");
                if child.len() > 4096 {
                    return Err(listing_limit());
                }
                pending.push((child, depth + 1));
            }
        }
        plan.push((path, directory));
    }
    Ok(plan)
}

async fn remove_tree(raw: &RawSftpSession, full: String) -> Result<(), ApiError> {
    // Finish enumeration before mutating the remote tree.
    let plan = delete_plan(raw, full).await?;
    for (path, directory) in plan.into_iter().rev() {
        let parent = path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .ok_or_else(invalid)?;
        let parent = if parent.is_empty() { "/" } else { parent };
        let canonical = raw.realpath(parent).await.map_err(error)?;
        if canonical.files.first().map(|entry| entry.filename.as_str()) != Some(parent) {
            return Err(ApiError::new(
                409,
                "state_conflict",
                "remote directory changed during deletion",
            ));
        }
        if directory {
            raw.rmdir(path).await.map_err(error)?;
        } else {
            raw.remove(path).await.map_err(error)?;
        }
    }
    Ok(())
}

/// Delete an entry, preflighting the full tree when recursive deletion is requested.
pub async fn remove(
    session: &crate::business::remote::RemoteSession,
    sftp: &SftpSession,
    root: &str,
    p: &str,
    recursive: bool,
) -> Result<(), ApiError> {
    if relative(p)?.is_empty() {
        return Err(invalid());
    }
    let work = async {
        let full = path(sftp, root, p, true).await?;
        if !recursive {
            let metadata = sftp.symlink_metadata(&full).await.map_err(error)?;
            return if metadata.is_dir() {
                sftp.remove_dir(full).await.map_err(error)
            } else {
                sftp.remove_file(full).await.map_err(error)
            };
        }
        let _tree = TREE_MUTATION.try_write().map_err(|_| tree_busy())?;
        let _slot = DIRECTORY_SLOTS.try_acquire().map_err(|_| {
            ApiError::new(429, "capacity_exceeded", "too many remote directory scans")
        })?;
        let raw = open_directory_channel(session).await?;
        remove_tree(&raw, full).await
    };
    tokio::select! {
        biased;
        _ = session.cancelled() => Err(ApiError::new(409, "state_conflict", "SSH session closed")),
        result = tokio::time::timeout(std::time::Duration::from_secs(30), work) =>
            result.map_err(|_| ApiError::new(504, "upstream_error", "remote deletion timed out"))?,
    }
}
/// 创建传输：上传写独占临时文件，完成前不触碰目标文件。
#[cfg(test)]
pub(crate) async fn create(
    sftp: &SftpSession,
    root: &str,
    value: &Value,
) -> Result<Transfer, ApiError> {
    create_tracked(sftp, root, value, &mut None).await
}

pub(crate) async fn create_tracked(
    sftp: &SftpSession,
    root: &str,
    value: &Value,
    uncertain: &mut Option<Transfer>,
) -> Result<Transfer, ApiError> {
    let direction = value["direction"].as_str().ok_or_else(invalid)?;
    if !matches!(direction, "upload" | "download") {
        return Err(invalid());
    }
    let p = value["path"]
        .as_str()
        .filter(|p| !p.is_empty())
        .ok_or_else(invalid)?;
    let full = path(sftp, root, p, direction == "upload").await?;
    let id = uuid::Uuid::new_v4().to_string();
    let mut version_text = String::new();
    let (file, total, temporary) = if direction == "upload" {
        if value["conflict"] != "fail" {
            return Err(ApiError::new(
                400,
                "unsupported_feature",
                "this server requires non-overwriting upload",
            ));
        }
        if sftp.try_exists(&full).await.map_err(error)? {
            return Err(ApiError::new(
                409,
                "state_conflict",
                "destination already exists",
            ));
        }
        let total = value["size"]
            .as_str()
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(invalid)?;
        let _tree = TREE_MUTATION.try_read().map_err(|_| tree_busy())?;
        let temp = format!(
            "{}/{UPLOAD_STAGING_PREFIX}{id}",
            full.rsplit_once('/').unwrap().0
        );
        let opened = sftp
            .open_with_flags_and_attributes(
                &temp,
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                FileAttributes {
                    permissions: Some(0o600),
                    ..Default::default()
                },
            )
            .await;
        let file = match opened {
            Ok(file) => file,
            Err(err) => {
                // An explicit refusal means this exclusive OPEN did not create the file.
                if !matches!(&err, SftpError::Status(status)
                    if !matches!(status.status_code, StatusCode::NoConnection | StatusCode::ConnectionLost))
                {
                    *uncertain = Some(Transfer {
                        id,
                        direction: direction.into(),
                        path: full,
                        temporary: Some(temp),
                        file: None,
                        offset: 0,
                        total,
                        state: "failed".into(),
                        version: String::new(),
                        hash: Sha256::new(),
                        last: None,
                    });
                }
                return Err(error(err));
            }
        };
        (file, total, Some(temp))
    } else {
        let metadata = sftp.metadata(&full).await.map_err(error)?;
        if !metadata.file_type().is_file() {
            return Err(invalid());
        }
        version_text = version(&metadata);
        if value
            .get("source_version")
            .is_some_and(|v| v != &version_text)
        {
            return Err(ApiError::new(409, "state_conflict", "source changed"));
        }
        (
            sftp.open(&full).await.map_err(error)?,
            metadata.size.unwrap_or(0),
            None,
        )
    };
    Ok(Transfer {
        id,
        direction: direction.into(),
        path: full,
        temporary,
        file: Some(file),
        offset: 0,
        total,
        state: "active".into(),
        version: version_text,
        hash: Sha256::new(),
        last: None,
    })
}
/// 上传块按确认偏移顺序执行，同一 offset 相同内容可重试。
pub async fn upload(t: &mut Transfer, offset: u64, bytes: &[u8]) -> Result<(), ApiError> {
    if t.direction != "upload" || t.state != "active" || bytes.len() > 1 << 20 {
        return Err(invalid());
    }
    if let Some((old, content)) = &t.last {
        if *old == offset && content == bytes {
            return Ok(());
        }
    }
    if offset != t.offset || offset + bytes.len() as u64 > t.total {
        return Err(ApiError::new(
            409,
            "state_conflict",
            "upload offset mismatch",
        ));
    }
    let file = t.file.as_mut().ok_or_else(invalid)?;
    let written = async {
        file.write_all(bytes).await?;
        file.flush().await
    }
    .await;
    if let Err(err) = written {
        // A partial remote write has unknown progress; it cannot be safely retried.
        t.state = "failed".into();
        return Err(error(err));
    }
    t.hash.update(bytes);
    t.offset += bytes.len() as u64;
    t.last = Some((offset, bytes.to_vec()));
    Ok(())
}
/// 有界下载块，Range 与源版本校验由 HTTP 适配调用。
pub async fn download(
    sftp: &SftpSession,
    t: &mut Transfer,
    offset: u64,
    length: usize,
) -> Result<Vec<u8>, ApiError> {
    if t.direction != "download" || t.state != "active" || length > 1 << 20 || offset > t.total {
        return Err(invalid());
    }
    if version(&sftp.metadata(&t.path).await.map_err(error)?) != t.version {
        return Err(ApiError::new(
            409,
            "state_conflict",
            "source changed during transfer",
        ));
    }
    let file = t.file.as_mut().ok_or_else(invalid)?;
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(error)?;
    let mut bytes = vec![0; length.min((t.total - offset) as usize)];
    file.read_exact(&mut bytes).await.map_err(error)?;
    t.offset = offset + bytes.len() as u64;
    Ok(bytes)
}
async fn verify_staging_parent(sftp: &SftpSession, temp: &str) -> Result<(), ApiError> {
    let (parent, name) = temp.rsplit_once('/').ok_or_else(invalid)?;
    if !is_upload_staging(name) {
        return Err(invalid());
    }
    let parent = if parent.is_empty() { "/" } else { parent };
    if sftp.canonicalize(parent).await.map_err(error)? != parent {
        return Err(ApiError::new(
            409,
            "state_conflict",
            "remote upload directory changed",
        ));
    }
    Ok(())
}

fn publication_unknown() -> ApiError {
    ApiError::new(
        502,
        "upstream_error",
        "remote upload publication outcome is unknown; inspect the destination before retrying",
    )
}

async fn publish_upload(
    transfer: &mut Transfer,
    rename: impl std::future::Future<Output = Result<(), SftpError>>,
) -> Result<(), ApiError> {
    // Record uncertainty before awaiting: the HTTP request can be cancelled after sending rename.
    transfer.state = "publication_unknown".into();
    match rename.await {
        Ok(()) => Ok(()),
        Err(SftpError::Status(status))
            if !matches!(
                status.status_code,
                StatusCode::NoConnection | StatusCode::ConnectionLost
            ) =>
        {
            transfer.state = "active".into();
            Err(error(status.status_code))
        }
        Err(_) => Err(publication_unknown()),
    }
}

async fn remote_upload_digest(
    sftp: &SftpSession,
    path: &str,
    size: u64,
) -> Result<sha2::digest::Output<Sha256>, ApiError> {
    let work = async {
        let mut file = sftp.open(path).await.map_err(error)?;
        let mut hash = Sha256::new();
        let mut remaining = size;
        let mut buffer = vec![0u8; 64 << 10];
        loop {
            let limit = remaining.min(buffer.len() as u64 - 1) as usize + 1;
            let count = file.read(&mut buffer[..limit]).await.map_err(error)?;
            if count == 0 {
                if remaining == 0 {
                    break;
                }
                return Err(ApiError::new(
                    409,
                    "state_conflict",
                    "remote upload changed before publication",
                ));
            }
            if count as u64 > remaining {
                return Err(ApiError::new(
                    409,
                    "state_conflict",
                    "remote upload changed before publication",
                ));
            }
            remaining -= count as u64;
            hash.update(&buffer[..count]);
        }
        file.shutdown().await.map_err(error)?;
        Ok(hash.finalize())
    };
    tokio::time::timeout(std::time::Duration::from_secs(180), work)
        .await
        .map_err(|_| {
            ApiError::new(
                504,
                "upstream_error",
                "remote upload verification timed out",
            )
        })?
}

/// 完成上传时验证真实 SHA256；标准 rename 拒绝覆盖。
pub async fn complete(sftp: &SftpSession, t: &mut Transfer, value: &Value) -> Result<(), ApiError> {
    if t.state == "publication_unknown" {
        return Err(publication_unknown());
    }
    let size = value["size"]
        .as_str()
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(invalid)?;
    if !matches!(t.state.as_str(), "active" | "completed") || size != t.total || t.offset != t.total
    {
        return Err(ApiError::new(
            409,
            "state_conflict",
            "transfer is incomplete",
        ));
    }
    if let Some(hash) = value.get("sha256") {
        if t.direction != "upload" {
            return Err(ApiError::new(
                400,
                "unsupported_feature",
                "download checksum is not available",
            ));
        }
        if hash != &format!("{:x}", t.hash.clone().finalize()) {
            return Err(ApiError::new(409, "state_conflict", "SHA256 mismatch"));
        }
    }
    if t.state == "completed" {
        return Ok(());
    }
    if let Some(mut file) = t.file.take() {
        if let Err(err) = file.shutdown().await {
            t.state = "failed".into();
            return Err(error(err));
        }
    }
    if let Some(temp) = &t.temporary {
        verify_staging_parent(sftp, temp).await?;
        let metadata = sftp.symlink_metadata(temp).await.map_err(error)?;
        if !metadata.is_regular() || metadata.size != Some(t.total) {
            t.state = "failed".into();
            return Err(ApiError::new(
                409,
                "state_conflict",
                "remote upload changed before publication",
            ));
        }
        let digest = remote_upload_digest(sftp, temp, t.total).await?;
        if digest != t.hash.clone().finalize() {
            t.state = "failed".into();
            return Err(ApiError::new(
                409,
                "state_conflict",
                "remote upload checksum mismatch",
            ));
        }
        verify_staging_parent(sftp, temp).await?;
        let (temporary, destination) = (temp.clone(), t.path.clone());
        publish_upload(t, sftp.rename(temporary, destination)).await?;
    }
    t.temporary = None;
    t.state = "completed".into();
    t.last = None;
    Ok(())
}
/// 取消只删除本次创建的临时文件，不删传输前已有的目标。
pub async fn cancel(sftp: &SftpSession, t: &mut Transfer) -> Result<(), ApiError> {
    if !matches!(
        t.state.as_str(),
        "active" | "failed" | "publication_unknown"
    ) {
        return Ok(());
    }
    t.file.take();
    if let Some(temp) = &t.temporary {
        verify_staging_parent(sftp, temp).await?;
        match sftp.remove_file(temp).await {
            Ok(()) => {}
            Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => {}
            Err(err) => return Err(error(err)),
        }
    }
    t.temporary = None;
    if t.state != "publication_unknown" {
        t.state = "cancelled".into();
    }
    t.last = None;
    Ok(())
}

pub(crate) async fn cleanup_transfers(
    sftp: &SftpSession,
    slots: impl Iterator<Item = TransferSlot>,
) {
    // A busy slot must not prevent cleanup of the other transfers in this session.
    futures_util::future::join_all(slots.map(|slot| async move {
        let mut transfer = slot.lock().await;
        if let Err(error) = cancel(sftp, &mut transfer).await {
            tracing::warn!(code=%error.code, "SFTP temporary file cleanup failed");
        }
    }))
    .await;
}

#[cfg(test)]
mod directory_tests {
    use super::*;
    use russh_sftp::{
        protocol::{Attrs, File as Entry, Handle, Name, Status},
        server::Handler,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct EndlessDirectory {
        reads: Arc<AtomicUsize>,
        closes: Arc<AtomicUsize>,
        batch_size: usize,
        removals: Arc<AtomicUsize>,
    }

    impl Handler for EndlessDirectory {
        type Error = StatusCode;
        fn unimplemented(&self) -> StatusCode {
            StatusCode::OpUnsupported
        }
        async fn lstat(&mut self, id: u32, _: String) -> Result<Attrs, StatusCode> {
            let mut attrs = FileAttributes::default();
            attrs.set_dir(true);
            Ok(Attrs { id, attrs })
        }
        async fn remove(&mut self, _: u32, _: String) -> Result<Status, StatusCode> {
            self.removals.fetch_add(1, Ordering::Relaxed);
            Err(StatusCode::Failure)
        }
        async fn rmdir(&mut self, _: u32, _: String) -> Result<Status, StatusCode> {
            self.removals.fetch_add(1, Ordering::Relaxed);
            Err(StatusCode::Failure)
        }
        async fn opendir(&mut self, id: u32, _: String) -> Result<Handle, StatusCode> {
            Ok(Handle {
                id,
                handle: "dir".into(),
            })
        }
        async fn readdir(&mut self, id: u32, _: String) -> Result<Name, StatusCode> {
            let batch = self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(Name {
                id,
                files: (0..self.batch_size)
                    .map(|i| Entry::dummy(format!("{batch}-{i}")))
                    .collect(),
            })
        }
        async fn close(&mut self, id: u32, _: String) -> Result<Status, StatusCode> {
            self.closes.fetch_add(1, Ordering::Relaxed);
            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: String::new(),
                language_tag: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn endless_directory_is_bounded_and_its_handle_is_closed() {
        for (batch_size, expected_reads) in [(0, MAX_LIST_BATCHES), (64, MAX_LIST_ENTRIES / 64 + 1)]
        {
            let (client, server) = tokio::io::duplex(65536);
            let reads = Arc::new(AtomicUsize::new(0));
            let closes = Arc::new(AtomicUsize::new(0));
            let server = tokio::spawn(russh_sftp::server::run(
                server,
                EndlessDirectory {
                    reads: reads.clone(),
                    closes: closes.clone(),
                    batch_size,
                    removals: Arc::new(AtomicUsize::new(0)),
                },
            ));
            let raw = RawSftpSession::new(client);
            raw.init().await.unwrap();
            let error = bounded_directory(&raw, "/".into()).await.unwrap_err();
            assert_eq!(error.code, "capacity_exceeded");
            assert_eq!(reads.load(Ordering::Relaxed), expected_reads);
            assert_eq!(closes.load(Ordering::Relaxed), 1);
            drop(raw);
            server.abort();
        }
    }
    #[tokio::test]
    async fn oversized_recursive_delete_does_not_mutate_remote_tree() {
        let (client, server) = tokio::io::duplex(65536);
        let removals = Arc::new(AtomicUsize::new(0));
        let closes = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(russh_sftp::server::run(
            server,
            EndlessDirectory {
                reads: Arc::new(AtomicUsize::new(0)),
                closes: closes.clone(),
                batch_size: 64,
                removals: removals.clone(),
            },
        ));
        let raw = RawSftpSession::new(client);
        raw.init().await.unwrap();
        assert_eq!(
            remove_tree(&raw, "/large".into()).await.unwrap_err().code,
            "capacity_exceeded"
        );
        assert_eq!(removals.load(Ordering::Relaxed), 0);
        assert_eq!(closes.load(Ordering::Relaxed), 1);
        drop(raw);
        server.abort();
    }
    #[tokio::test]
    #[ignore = "requires YOODESK_TEST_SFTP_SERVER pointing to OpenSSH sftp-server"]
    async fn openssh_recursive_delete_removes_only_selected_tree() {
        let exe = std::env::var_os("YOODESK_TEST_SFTP_SERVER").expect("OpenSSH sftp-server path");
        let root =
            std::env::temp_dir().join(format!("yoodesk-sftp-delete-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("selected/child")).unwrap();
        std::fs::write(root.join("selected/child/data"), b"delete").unwrap();
        std::fs::write(root.join("keep"), b"keep").unwrap();
        let mut child = tokio::process::Command::new(exe)
            .current_dir(&root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stream = tokio::io::join(child.stdout.take().unwrap(), child.stdin.take().unwrap());
        let raw = RawSftpSession::new(stream);
        raw.init().await.unwrap();
        let canonical = raw.realpath(".").await.unwrap().files.remove(0).filename;
        let full = format!("{}/selected", canonical.trim_end_matches('/'));
        let staging = root.join("selected/child/.remote-upload-active");
        std::fs::write(&staging, b"pending").unwrap();
        let destination = format!("{}/renamed", canonical.trim_end_matches('/'));
        let blocked_rename = rename_tree(&raw, full.clone(), destination.clone())
            .await
            .unwrap_err();
        assert_eq!(blocked_rename.code, "state_conflict");
        assert!(!root.join("renamed").exists());
        let blocked = remove_tree(&raw, full.clone()).await.unwrap_err();
        assert_eq!(blocked.code, "state_conflict");
        assert_eq!(
            std::fs::read(root.join("selected/child/data")).unwrap(),
            b"delete"
        );
        std::fs::remove_file(staging).unwrap();
        rename_tree(&raw, full, destination.clone())
            .await
            .unwrap_or_else(|err| panic!("recursive rename: {}", err.message));
        assert!(!root.join("selected").exists());
        assert!(root.join("renamed/child/data").exists());
        let result = remove_tree(&raw, destination).await;
        let deleted = !root.join("renamed").exists();
        let kept = std::fs::read(root.join("keep")).unwrap();
        drop(raw);
        let _ = child.kill().await;
        std::fs::remove_dir_all(root).unwrap();
        if let Err(err) = result {
            panic!("recursive delete: {}", err.message);
        }
        assert!(deleted);
        assert_eq!(kept, b"keep");
    }
    #[tokio::test]
    #[ignore = "requires YOODESK_TEST_SFTP_SERVER pointing to OpenSSH sftp-server"]
    async fn openssh_upload_completion_checks_retries_and_remote_contents() {
        let exe = std::env::var_os("YOODESK_TEST_SFTP_SERVER").expect("OpenSSH sftp-server path");
        let root =
            std::env::temp_dir().join(format!("yoodesk-sftp-upload-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let mut child = tokio::process::Command::new(exe)
            .current_dir(&root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stream = tokio::io::join(child.stdout.take().unwrap(), child.stdin.take().unwrap());
        let sftp = SftpSession::new(stream).await.unwrap();
        let mut good = create(
            &sftp,
            ".",
            &json!({"direction":"upload","path":"good","size":"3","conflict":"fail"}),
        )
        .await
        .unwrap();
        upload(&mut good, 0, b"abc").await.unwrap();
        let digest = format!("{:x}", Sha256::digest(b"abc"));
        let request = json!({"size":"3","sha256":digest});
        complete(&sftp, &mut good, &request).await.unwrap();
        complete(&sftp, &mut good, &request).await.unwrap();
        assert_eq!(
            complete(&sftp, &mut good, &json!({"size":"3","sha256":"wrong"}))
                .await
                .unwrap_err()
                .code,
            "state_conflict"
        );
        let mut changed = create(
            &sftp,
            ".",
            &json!({"direction":"upload","path":"changed","size":"3","conflict":"fail"}),
        )
        .await
        .unwrap();
        upload(&mut changed, 0, b"abc").await.unwrap();
        let temp = changed.temporary.clone().unwrap();
        sftp.write(&temp, b"xyz").await.unwrap();
        assert_eq!(
            complete(&sftp, &mut changed, &request)
                .await
                .unwrap_err()
                .code,
            "state_conflict"
        );
        assert!(!root.join("changed").exists());
        cancel(&sftp, &mut changed).await.unwrap();
        assert!(!sftp.try_exists(&temp).await.unwrap());
        assert_eq!(std::fs::read(root.join("good")).unwrap(), b"abc");
        drop(sftp);
        let _ = child.kill().await;
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn interrupted_rename_does_not_become_a_retryable_upload() {
        let mut transfer = Transfer {
            id: "test".into(),
            direction: "upload".into(),
            path: "/destination".into(),
            temporary: Some("/temporary".into()),
            file: None,
            offset: 0,
            total: 0,
            state: "active".into(),
            version: String::new(),
            hash: Sha256::new(),
            last: None,
        };
        {
            let publication = publish_upload(&mut transfer, std::future::pending());
            tokio::pin!(publication);
            assert!(futures_util::poll!(&mut publication).is_pending());
        }
        assert_eq!(transfer.state, "publication_unknown");
        assert_eq!(transfer.info()["state"], "failed");
        assert!(transfer.info()["verified_sha256"].is_null());
        assert_eq!(transfer.temporary.as_deref(), Some("/temporary"));
    }
    #[test]
    fn public_paths_reject_staging_components() {
        for path in [
            ".remote-upload-id",
            "folder/.REMOTE-UPLOAD-id",
            ".Remote-Upload-id/child",
        ] {
            assert!(relative(path).is_err(), "{path}");
        }
        assert_eq!(
            relative("folder/remote-upload-id").unwrap(),
            "folder/remote-upload-id"
        );
    }
    #[tokio::test]
    async fn uncertain_open_keeps_cleanup_record_but_explicit_refusal_does_not() {
        struct OpenFailure(StatusCode);
        impl Handler for OpenFailure {
            type Error = StatusCode;
            fn unimplemented(&self) -> StatusCode {
                StatusCode::OpUnsupported
            }
            async fn realpath(&mut self, id: u32, _: String) -> Result<Name, StatusCode> {
                Ok(Name {
                    id,
                    files: vec![Entry::dummy("/root")],
                })
            }
            async fn stat(&mut self, _: u32, _: String) -> Result<Attrs, StatusCode> {
                Err(StatusCode::NoSuchFile)
            }
            async fn open(
                &mut self,
                _: u32,
                path: String,
                _: OpenFlags,
                _: FileAttributes,
            ) -> Result<Handle, StatusCode> {
                assert!(path.starts_with("/root/.remote-upload-"));
                Err(self.0)
            }
        }
        for (status, tracked) in [
            (StatusCode::ConnectionLost, true),
            (StatusCode::PermissionDenied, false),
        ] {
            let (client, peer) = tokio::io::duplex(65536);
            let peer = tokio::spawn(russh_sftp::server::run(peer, OpenFailure(status)));
            let sftp = SftpSession::new(client).await.unwrap();
            let mut uncertain = None;
            let result = create_tracked(
                &sftp,
                "/root",
                &json!({"direction":"upload","path":"file","size":"1","conflict":"fail"}),
                &mut uncertain,
            )
            .await;
            assert!(result.is_err());
            assert_eq!(uncertain.is_some(), tracked);
            if let Some(transfer) = uncertain {
                assert_eq!(transfer.state, "failed");
                assert!(transfer
                    .temporary
                    .unwrap()
                    .starts_with("/root/.remote-upload-"));
            }
            peer.abort();
        }
    }
}
