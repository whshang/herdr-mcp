//! Trusted browser-extension IPC socket lifecycle.
//!
//! The Chrome Native Messaging broker is origin-restricted by Chromium and
//! forwards local requests over this mode-0600 Unix socket. Binding is
//! intentionally opt-in during the Rust migration; production ownership is
//! enabled only after `/mcp` and every `/push/*` route have parity.

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::pin::Pin;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::task::{Context, Poll};
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

#[cfg(unix)]
pub const EXTENSION_IPC_FRAME_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(unix)]
pub const MAX_EXTENSION_IPC_HEADER_BYTES: usize = 32 * 1024;
#[cfg(unix)]
pub const MAX_EXTENSION_IPC_BODY_PREFETCH_BYTES: usize = 1024 * 1024 + 1;
#[cfg(unix)]
const MAX_CONCURRENT_IPC_PREFLIGHTS: usize = 64;
#[cfg(unix)]
const ALLOWED_HTTP_METHOD_PREFIXES: &[&[u8]] = &[
    b"GET /",
    b"POST /",
    b"DELETE /",
    b"HEAD /",
    b"OPTIONS /",
    b"PUT /",
    b"PATCH /",
];

#[cfg(unix)]
#[derive(Debug)]
pub struct ExtensionIpcSocket {
    path: PathBuf,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
pub struct PrefixedUnixStream {
    prefix: std::io::Cursor<Vec<u8>>,
    inner: tokio::net::UnixStream,
    delegate_read_after_prefix: bool,
}

#[cfg(unix)]
impl AsyncRead for PrefixedUnixStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let pos = usize::try_from(self.prefix.position()).unwrap_or(usize::MAX);
        let slice = self.prefix.get_ref().as_slice();
        if pos < slice.len() {
            let remaining = &slice[pos..];
            let to_copy = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..to_copy]);
            self.prefix.set_position((pos + to_copy) as u64);
            return Poll::Ready(Ok(()));
        }
        if self.delegate_read_after_prefix {
            Pin::new(&mut self.inner).poll_read(cx, buf)
        } else {
            // The complete single HTTP frame is already buffered in `self.prefix`
            // with `Connection: close`, so Hyper will close the connection as
            // soon as the response is flushed. Returning `Poll::Pending` here
            // prevents Hyper from treating read-EOF as an early client abort.
            Poll::Pending
        }
    }
}

#[cfg(unix)]
impl AsyncWrite for PrefixedUnixStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(unix)]
pub struct BoundedExtensionIpcListener {
    ready_rx: tokio::sync::mpsc::Receiver<(PrefixedUnixStream, tokio::net::unix::SocketAddr)>,
    local_addr: tokio::net::unix::SocketAddr,
    acceptor_task: tokio::task::JoinHandle<()>,
}

#[cfg(unix)]
impl BoundedExtensionIpcListener {
    pub fn new(
        listener: tokio::net::UnixListener,
        frame_timeout: Duration,
    ) -> Result<Self, String> {
        let local_addr = listener
            .local_addr()
            .map_err(|error| format!("cannot inspect extension IPC listener address: {error}"))?;
        let (ready_tx, ready_rx) = tokio::sync::mpsc::channel(MAX_CONCURRENT_IPC_PREFLIGHTS);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_IPC_PREFLIGHTS));
        let acceptor_task = tokio::spawn(async move {
            loop {
                let (stream, addr) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                };
                let Ok(permit) = semaphore.clone().try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let tx = ready_tx.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Ok(prefixed) = read_validated_http_frame(stream, frame_timeout).await {
                        let _ = tx.send((prefixed, addr)).await;
                    }
                });
            }
        });
        Ok(Self {
            ready_rx,
            local_addr,
            acceptor_task,
        })
    }
}

#[cfg(unix)]
impl Drop for BoundedExtensionIpcListener {
    fn drop(&mut self) {
        self.acceptor_task.abort();
    }
}

#[cfg(unix)]
impl axum::serve::Listener for BoundedExtensionIpcListener {
    type Io = PrefixedUnixStream;
    type Addr = tokio::net::unix::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready_rx.recv().await {
            Some(item) => item,
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> tokio::io::Result<Self::Addr> {
        Ok(self.local_addr.clone())
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameHeaderValidation {
    NeedMoreBytes,
    Valid {
        header_end: usize,
        content_length: usize,
        expect_100_continue: bool,
        is_sse_push_stream: bool,
    },
    Invalid,
}

#[cfg(unix)]
fn validate_request_line_prefix(buf: &[u8]) -> Result<Option<(usize, bool)>, ()> {
    let line_end = buf.iter().position(|&b| b == b'\n');
    let first_line = match line_end {
        Some(pos) => &buf[..pos],
        None => buf,
    };
    if first_line.len() > 4096 {
        return Err(());
    }
    if first_line
        .iter()
        .any(|&b| (b < 0x20 && b != b'\r') || b == 0x7f)
    {
        return Err(());
    }
    let trimmed = first_line.strip_suffix(b"\r").unwrap_or(first_line);
    let matches_prefix = ALLOWED_HTTP_METHOD_PREFIXES
        .iter()
        .any(|allowed| allowed.starts_with(trimmed) || trimmed.starts_with(allowed));
    if !matches_prefix {
        return Err(());
    }
    let Some(pos) = line_end else {
        return Ok(None);
    };
    if !ALLOWED_HTTP_METHOD_PREFIXES
        .iter()
        .any(|allowed| trimmed.starts_with(allowed))
    {
        return Err(());
    }
    if !(trimmed.ends_with(b" HTTP/1.1") || trimmed.ends_with(b" HTTP/1.0")) {
        return Err(());
    }
    let is_sse =
        trimmed.starts_with(b"GET /push/events ") || trimmed.starts_with(b"GET /push/events?");
    Ok(Some((pos + 1, is_sse)))
}

#[cfg(unix)]
fn find_header_delimiter(buf: &[u8]) -> Option<(usize, usize)> {
    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some((pos, 4));
    }
    buf.windows(2)
        .position(|w| w == b"\n\n")
        .map(|pos| (pos, 2))
}

#[cfg(unix)]
fn inspect_http_frame_headers(buf: &[u8]) -> FrameHeaderValidation {
    let first_line_info = match validate_request_line_prefix(buf) {
        Ok(info) => info,
        Err(()) => return FrameHeaderValidation::Invalid,
    };
    let Some((headers_offset, is_sse_push_stream)) = first_line_info else {
        if buf.len() >= MAX_EXTENSION_IPC_HEADER_BYTES {
            return FrameHeaderValidation::Invalid;
        }
        return FrameHeaderValidation::NeedMoreBytes;
    };
    let Some((delim_pos, delim_len)) = find_header_delimiter(buf) else {
        if buf.len() >= MAX_EXTENSION_IPC_HEADER_BYTES {
            return FrameHeaderValidation::Invalid;
        }
        return FrameHeaderValidation::NeedMoreBytes;
    };
    let header_end = delim_pos + delim_len;
    if header_end > MAX_EXTENSION_IPC_HEADER_BYTES {
        return FrameHeaderValidation::Invalid;
    }
    let header_block = &buf[headers_offset..delim_pos];
    let mut content_length: Option<usize> = None;
    let mut expect_100_continue = false;
    for raw_line in header_block.split(|&b| b == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if line.is_empty() {
            continue;
        }
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            return FrameHeaderValidation::Invalid;
        };
        let name = &line[..colon];
        let value = &line[colon + 1..];
        if name.is_empty()
            || name.iter().any(|&b| b <= 0x20 || b == 0x7f || b == b':')
            || value.iter().any(|&b| (b < 0x20 && b != b'\t') || b == 0x7f)
        {
            return FrameHeaderValidation::Invalid;
        }
        let Ok(name_str) = std::str::from_utf8(name) else {
            return FrameHeaderValidation::Invalid;
        };
        let Ok(value_str) = std::str::from_utf8(value) else {
            return FrameHeaderValidation::Invalid;
        };
        let value_trimmed = value_str.trim();
        if name_str.eq_ignore_ascii_case("transfer-encoding") {
            return FrameHeaderValidation::Invalid;
        }
        if name_str.eq_ignore_ascii_case("content-length") {
            let Ok(parsed) = value_trimmed.parse::<usize>() else {
                return FrameHeaderValidation::Invalid;
            };
            if let Some(existing) = content_length
                && existing != parsed
            {
                return FrameHeaderValidation::Invalid;
            }
            content_length = Some(parsed);
        }
        if name_str.eq_ignore_ascii_case("expect")
            && value_trimmed.eq_ignore_ascii_case("100-continue")
        {
            expect_100_continue = true;
        }
    }
    FrameHeaderValidation::Valid {
        header_end,
        content_length: content_length.unwrap_or(0),
        expect_100_continue,
        is_sse_push_stream,
    }
}

#[cfg(unix)]
async fn write_ipc_error_and_close(
    mut stream: tokio::net::UnixStream,
    status_line: &str,
    code: &str,
) {
    let body = format!("{{\"ok\":false,\"code\":\"{code}\"}}");
    let response = format!(
        "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = tokio::time::timeout(Duration::from_millis(250), async {
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    })
    .await;
}

#[cfg(unix)]
fn neutralize_header_in_place(buf: &mut [u8], name_with_colon: &[u8], replacement_prefix: &[u8]) {
    let mut i = 0;
    while i + name_with_colon.len() <= buf.len() {
        let at_line_start = i == 0 || buf[i - 1] == b'\n';
        if at_line_start && buf[i..i + name_with_colon.len()].eq_ignore_ascii_case(name_with_colon)
        {
            buf[i..i + replacement_prefix.len()].copy_from_slice(replacement_prefix);
            i += name_with_colon.len();
        } else {
            i += 1;
        }
    }
}

#[cfg(unix)]
fn inject_connection_close(buf: &[u8], header_end: usize) -> Vec<u8> {
    let delim_pos = find_header_delimiter(&buf[..header_end])
        .map(|(pos, _)| pos)
        .unwrap_or(header_end);
    let mut head = buf[..delim_pos].to_vec();
    neutralize_header_in_place(&mut head, b"connection:", b"x-ipc-conn");
    if !head.ends_with(b"\r\n") && !head.ends_with(b"\n") {
        head.extend_from_slice(b"\r\n");
    }
    head.extend_from_slice(b"connection: close\r\n\r\n");
    head.extend_from_slice(&buf[header_end..]);
    head
}

#[cfg(unix)]
async fn read_validated_http_frame(
    mut stream: tokio::net::UnixStream,
    frame_timeout: Duration,
) -> Result<PrefixedUnixStream, ()> {
    let deadline = tokio::time::Instant::now() + frame_timeout;
    let mut buf = Vec::with_capacity(1024);

    let (header_end, content_length, expect_100_continue, is_sse_push_stream) = loop {
        match tokio::time::timeout_at(deadline, stream.read_buf(&mut buf)).await {
            Err(_) => {
                write_ipc_error_and_close(
                    stream,
                    "408 Request Timeout",
                    "extension_ipc_frame_timeout",
                )
                .await;
                return Err(());
            }
            Ok(Err(_)) => return Err(()),
            Ok(Ok(0)) => {
                if !buf.is_empty() {
                    write_ipc_error_and_close(
                        stream,
                        "400 Bad Request",
                        "extension_ipc_invalid_frame_header",
                    )
                    .await;
                }
                return Err(());
            }
            Ok(Ok(_)) => match inspect_http_frame_headers(&buf) {
                FrameHeaderValidation::NeedMoreBytes => continue,
                FrameHeaderValidation::Invalid => {
                    write_ipc_error_and_close(
                        stream,
                        "400 Bad Request",
                        "extension_ipc_invalid_frame_header",
                    )
                    .await;
                    return Err(());
                }
                FrameHeaderValidation::Valid {
                    header_end,
                    content_length,
                    expect_100_continue,
                    is_sse_push_stream,
                } => {
                    break (
                        header_end,
                        content_length,
                        expect_100_continue,
                        is_sse_push_stream,
                    );
                }
            },
        }
    };

    let target_body_bytes = content_length.min(MAX_EXTENSION_IPC_BODY_PREFETCH_BYTES);
    let target_total_bytes = header_end.saturating_add(target_body_bytes);
    if expect_100_continue && buf.len() < target_total_bytes {
        let _ =
            tokio::time::timeout_at(deadline, stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n"))
                .await;
    }

    while buf.len() < target_total_bytes {
        match tokio::time::timeout_at(deadline, stream.read_buf(&mut buf)).await {
            Err(_) | Ok(Ok(0)) => {
                write_ipc_error_and_close(
                    stream,
                    "408 Request Timeout",
                    "extension_ipc_frame_timeout",
                )
                .await;
                return Err(());
            }
            Ok(Err(_)) => return Err(()),
            Ok(Ok(_)) => {}
        }
    }

    if expect_100_continue {
        neutralize_header_in_place(&mut buf[..header_end], b"expect:", b"X-Expc");
    }

    if buf.len() > target_total_bytes {
        buf.truncate(target_total_bytes);
    }

    let final_buf = if is_sse_push_stream {
        buf
    } else {
        inject_connection_close(&buf, header_end)
    };

    Ok(PrefixedUnixStream {
        prefix: std::io::Cursor::new(final_buf),
        inner: stream,
        delegate_read_after_prefix: is_sse_push_stream
            || content_length > MAX_EXTENSION_IPC_BODY_PREFETCH_BYTES,
    })
}

#[cfg(unix)]
impl ExtensionIpcSocket {
    pub async fn bind(path: impl AsRef<Path>) -> Result<(tokio::net::UnixListener, Self), String> {
        let path = path.as_ref();
        prepare_socket_path(path)?;
        let listener = tokio::net::UnixListener::bind(path)
            .map_err(|error| format!("cannot bind extension IPC {}: {error}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("cannot secure extension IPC {}: {error}", path.display()))?;
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| format!("cannot stat extension IPC {}: {error}", path.display()))?;
        if !metadata.file_type().is_socket() {
            return Err(format!(
                "extension IPC {} is not a Unix socket after bind",
                path.display()
            ));
        }
        Ok((
            listener,
            Self {
                path: path.to_path_buf(),
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        ))
    }

    fn remove_if_owned(&self) {
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
impl Drop for ExtensionIpcSocket {
    fn drop(&mut self) {
        self.remove_if_owned();
    }
}

#[cfg(unix)]
fn prepare_socket_path(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "extension IPC path must have a parent directory".to_owned())?;
    if let Ok(metadata) = std::fs::symlink_metadata(parent)
        && metadata.file_type().is_symlink()
    {
        return Err(format!(
            "extension IPC parent {} must not be a symlink",
            parent.display()
        ));
    }
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "cannot create extension IPC parent {}: {error}",
            parent.display()
        )
    })?;

    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "extension IPC {} must not be a symlink",
            path.display()
        ));
    }
    if !metadata.file_type().is_socket() {
        return Err(format!(
            "extension IPC {} exists and is not a Unix socket",
            path.display()
        ));
    }
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return Err(format!(
            "extension IPC {} is already accepting connections",
            path.display()
        ));
    }
    std::fs::remove_file(path).map_err(|error| {
        format!(
            "cannot remove stale extension IPC {}: {error}",
            path.display()
        )
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "hmi-{:x}-{:x}-{nonce:x}.sock",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    async fn wait_until_socket_stale(path: &Path) {
        for _ in 0..200 {
            if std::os::unix::net::UnixStream::connect(path).is_err() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!(
            "socket remained connectable after listener drop: {}",
            path.display()
        );
    }

    #[tokio::test]
    async fn bind_secures_socket_and_drop_removes_owned_inode() {
        let path = temp_path();
        let (listener, guard) = ExtensionIpcSocket::bind(&path).await.unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        drop(listener);
        drop(guard);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn live_socket_is_never_unlinked_by_second_bind() {
        let path = temp_path();
        let (listener, guard) = ExtensionIpcSocket::bind(&path).await.unwrap();
        let client = std::os::unix::net::UnixStream::connect(&path).unwrap();
        drop(client);

        let error = ExtensionIpcSocket::bind(&path).await.unwrap_err();
        assert!(error.contains("already accepting connections"));
        assert!(path.exists());
        drop(listener);
        drop(guard);
    }

    #[tokio::test]
    async fn stale_socket_is_replaced_but_regular_file_is_refused() {
        let path = temp_path();
        let stale = tokio::net::UnixListener::bind(&path).unwrap();
        drop(stale);
        wait_until_socket_stale(&path).await;
        let (replacement, guard) = ExtensionIpcSocket::bind(&path).await.unwrap();
        drop(replacement);
        drop(guard);

        std::fs::write(&path, b"do-not-delete").unwrap();
        let error = ExtensionIpcSocket::bind(&path).await.unwrap_err();
        assert!(error.contains("not a Unix socket"));
        assert_eq!(std::fs::read(&path).unwrap(), b"do-not-delete");
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn malformed_or_non_http_frame_headers_rejected_immediately_without_stalling_subsequent_ipc()
     {
        use axum::{Router, routing::get};

        let path = temp_path();
        let (listener, guard) = ExtensionIpcSocket::bind(&path).await.unwrap();
        let bounded =
            BoundedExtensionIpcListener::new(listener, Duration::from_millis(300)).unwrap();
        let app = Router::new().route("/health", get(|| async { "{\"ok\":true}" }));
        let server = tokio::spawn(async move {
            let _ = axum::serve(bounded, app).await;
        });

        let malformed_payloads: &[&[u8]] = &[
            b"\x04\x00\x00\x00{}",
            b"{\"jsonrpc\":\"2.0\",\"id\":1}\n",
            b"CONNECT /mcp HTTP/1.1\r\nHost: localhost\r\n\r\n",
            b"POST /mcp HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        ];

        for payload in malformed_payloads {
            let started = tokio::time::Instant::now();
            let mut client = tokio::net::UnixStream::connect(&path).await.unwrap();
            client.write_all(payload).await.unwrap();
            let mut resp = Vec::new();
            client.read_to_end(&mut resp).await.unwrap();
            let text = String::from_utf8_lossy(&resp);
            assert!(
                text.starts_with("HTTP/1.1 400 Bad Request"),
                "expected 400 Bad Request for malformed frame, got: {text}"
            );
            assert!(text.contains("extension_ipc_invalid_frame_header"));
            assert!(
                started.elapsed() < Duration::from_millis(150),
                "malformed frame rejection took too long: {:?}",
                started.elapsed()
            );
        }

        let mut valid = tokio::net::UnixStream::connect(&path).await.unwrap();
        valid
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut valid_resp = Vec::new();
        valid.read_to_end(&mut valid_resp).await.unwrap();
        let valid_text = String::from_utf8_lossy(&valid_resp);
        assert!(valid_text.starts_with("HTTP/1.1 200 OK"));
        assert!(valid_text.contains("{\"ok\":true}"));

        server.abort();
        drop(guard);
    }

    #[tokio::test]
    async fn half_open_or_incomplete_body_client_times_out_boundedly_while_valid_ipc_requests_succeed_immediately()
     {
        use axum::{Router, routing::post};

        let path = temp_path();
        let (listener, guard) = ExtensionIpcSocket::bind(&path).await.unwrap();
        let bounded =
            BoundedExtensionIpcListener::new(listener, Duration::from_millis(200)).unwrap();
        let app = Router::new().route("/mcp", post(|body: String| async move { body }));
        let server = tokio::spawn(async move {
            let _ = axum::serve(bounded, app).await;
        });

        let silent_client = tokio::net::UnixStream::connect(&path).await.unwrap();
        let mut partial_header_client = tokio::net::UnixStream::connect(&path).await.unwrap();
        partial_header_client
            .write_all(b"POST /mcp HTTP/1.1\r\nHost: localhost\r\n")
            .await
            .unwrap();
        let mut truncated_body_client = tokio::net::UnixStream::connect(&path).await.unwrap();
        truncated_body_client
            .write_all(
                b"POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Length: 200\r\n\r\n{\"a\":",
            )
            .await
            .unwrap();

        // While all three stalled clients are still open, a valid IPC client must
        // be served immediately without waiting for the stalled clients to time out.
        let valid_started = tokio::time::Instant::now();
        let mut valid_client = tokio::net::UnixStream::connect(&path).await.unwrap();
        valid_client
            .write_all(
                b"POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
            )
            .await
            .unwrap();
        let mut valid_resp = Vec::new();
        valid_client.read_to_end(&mut valid_resp).await.unwrap();
        let valid_text = String::from_utf8_lossy(&valid_resp);
        assert!(valid_text.starts_with("HTTP/1.1 200 OK"));
        assert!(valid_text.ends_with("{\"ok\":true}"));
        assert!(
            valid_started.elapsed() < Duration::from_millis(100),
            "valid IPC request was delayed by half-open peers: {:?}",
            valid_started.elapsed()
        );

        for mut stalled in [silent_client, partial_header_client, truncated_body_client] {
            let mut out = Vec::new();
            tokio::time::timeout(Duration::from_millis(500), stalled.read_to_end(&mut out))
                .await
                .expect("stalled client should be closed within bounded timeout")
                .unwrap();
            let text = String::from_utf8_lossy(&out);
            assert!(
                text.starts_with("HTTP/1.1 408 Request Timeout"),
                "expected 408 Request Timeout, got: {text}"
            );
            assert!(text.contains("extension_ipc_frame_timeout"));
        }

        server.abort();
        drop(guard);
    }
}
