use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RPC_TIMEOUT: Duration = Duration::from_secs(60);
const PING_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct HerdrClient {
    socket_path: PathBuf,
    timeout: Duration,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HerdrError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for HerdrError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for HerdrError {}

impl HerdrClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout: DEFAULT_RPC_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_timeout(socket_path: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout,
        }
    }

    pub fn ping(&self) -> Result<Value, HerdrError> {
        self.call_with_timeout("ping", json!({}), PING_TIMEOUT)
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, HerdrError> {
        call_socket(&self.socket_path, self.timeout, method, params)
    }

    pub fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, HerdrError> {
        call_socket(
            &self.socket_path,
            timeout.min(MAX_RPC_TIMEOUT),
            method,
            params,
        )
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

#[cfg(unix)]
fn call_socket(
    socket_path: &Path,
    timeout: Duration,
    method: &str,
    params: Value,
) -> Result<Value, HerdrError> {
    use std::os::unix::net::UnixStream;

    let mut stream =
        UnixStream::connect(socket_path).map_err(|error| io_error(error, socket_path))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| io_error(error, socket_path))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| io_error(error, socket_path))?;

    let id = format!("rust-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let request = json!({
        "id": id,
        "method": method,
        "params": params,
    });
    let mut encoded = serde_json::to_vec(&request).map_err(|error| HerdrError {
        code: "encode_error".to_owned(),
        message: error.to_string(),
    })?;
    encoded.push(b'\n');
    stream
        .write_all(&encoded)
        .map_err(|error| io_error(error, socket_path))?;

    let line = read_bounded_line(&mut stream)?;
    let envelope: Value = serde_json::from_slice(&line).map_err(|error| HerdrError {
        code: "parse_error".to_owned(),
        message: error.to_string(),
    })?;

    if let Some(error) = envelope.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_owned();
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        return Err(HerdrError { code, message });
    }

    Ok(envelope.get("result").cloned().unwrap_or_else(|| json!({})))
}

#[cfg(windows)]
fn call_socket(
    _socket_path: &Path,
    _timeout: Duration,
    _method: &str,
    _params: Value,
) -> Result<Value, HerdrError> {
    Err(HerdrError {
        code: "unsupported_transport".to_owned(),
        message: "Windows named-pipe transport has not landed yet".to_owned(),
    })
}

fn read_bounded_line(reader: &mut impl Read) -> Result<Vec<u8>, HerdrError> {
    let mut output = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 4096];
    loop {
        let count = reader.read(&mut chunk).map_err(|error| HerdrError {
            code: if error.kind() == std::io::ErrorKind::TimedOut
                || error.kind() == std::io::ErrorKind::WouldBlock
            {
                "timeout".to_owned()
            } else {
                "socket_error".to_owned()
            },
            message: error.to_string(),
        })?;
        if count == 0 {
            return Err(HerdrError {
                code: "unexpected_eof".to_owned(),
                message: "Herdr socket closed before a newline-delimited response arrived"
                    .to_owned(),
            });
        }

        if let Some(index) = chunk[..count].iter().position(|byte| *byte == b'\n') {
            output.extend_from_slice(&chunk[..index]);
            if output.len() > MAX_RESPONSE_BYTES {
                return Err(response_too_large());
            }
            return Ok(output);
        }

        output.extend_from_slice(&chunk[..count]);
        if output.len() > MAX_RESPONSE_BYTES {
            return Err(response_too_large());
        }
    }
}

fn response_too_large() -> HerdrError {
    HerdrError {
        code: "response_too_large".to_owned(),
        message: format!("Herdr response exceeded {MAX_RESPONSE_BYTES} bytes"),
    }
}

fn io_error(error: std::io::Error, socket_path: &Path) -> HerdrError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => "socket_missing",
        std::io::ErrorKind::ConnectionRefused => "connection_refused",
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => "timeout",
        _ => "socket_error",
    };
    HerdrError {
        code: code.to_owned(),
        message: format!("{}: {error}", socket_path.display()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

    fn temp_socket() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "herdr-mcp-rust-{}-{unique}-{sequence}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn sends_newline_json_and_parses_result() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "ping");
            assert_eq!(request["params"], json!({}));
            let id = request["id"].as_str().unwrap();
            writeln!(stream, "{}", json!({"id": id, "result": {"pong": true}})).unwrap();
        });

        let result = HerdrClient::with_timeout(&socket, Duration::from_secs(1))
            .ping()
            .unwrap();
        assert_eq!(result, json!({"pong": true}));
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }

    #[test]
    fn maps_daemon_error_envelope() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            writeln!(
                stream,
                "{}",
                json!({"id": "ignored", "error": {"code": "bad_method", "message": "nope"}})
            )
            .unwrap();
        });

        let error = HerdrClient::with_timeout(&socket, Duration::from_secs(1))
            .ping()
            .unwrap_err();
        assert_eq!(error.code, "bad_method");
        assert_eq!(error.message, "nope");
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}
