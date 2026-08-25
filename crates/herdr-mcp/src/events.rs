use crate::herdr::{HerdrClient, HerdrError};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const MAX_EVENT_LINE_BYTES: usize = 1024 * 1024;
const READ_TICK: Duration = Duration::from_millis(200);
static NEXT_SUBSCRIPTION_ID: AtomicU64 = AtomicU64::new(1);

pub struct EventStream {
    #[cfg(unix)]
    stream: std::os::unix::net::UnixStream,
    buffer: Vec<u8>,
    deadline: Instant,
}

impl EventStream {
    pub fn subscribe(
        client: &HerdrClient,
        subscriptions: Vec<Value>,
        duration: Duration,
    ) -> Result<Self, HerdrError> {
        subscribe_socket(client, subscriptions, duration)
    }

    #[cfg(test)]
    pub fn next_event(&mut self) -> Result<Option<Value>, HerdrError> {
        next_event_until(self, self.deadline)
    }

    pub fn poll_event(&mut self, max_wait: Duration) -> Result<Option<Value>, HerdrError> {
        let poll_deadline = (Instant::now() + max_wait).min(self.deadline);
        next_event_until(self, poll_deadline)
    }

    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

#[cfg(unix)]
fn subscribe_socket(
    client: &HerdrClient,
    subscriptions: Vec<Value>,
    duration: Duration,
) -> Result<EventStream, HerdrError> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(client.socket_path()).map_err(|error| HerdrError {
        code: match error.kind() {
            std::io::ErrorKind::NotFound => "socket_missing",
            std::io::ErrorKind::ConnectionRefused => "connection_refused",
            _ => "socket_error",
        }
        .to_owned(),
        message: format!("{}: {error}", client.socket_path().display()),
    })?;
    let io_timeout = duration.min(READ_TICK).max(Duration::from_millis(10));
    stream
        .set_read_timeout(Some(io_timeout))
        .map_err(stream_error)?;
    stream
        .set_write_timeout(Some(io_timeout))
        .map_err(stream_error)?;

    let id = format!(
        "rust-events-{}",
        NEXT_SUBSCRIPTION_ID.fetch_add(1, Ordering::Relaxed)
    );
    let request = json!({
        "id": id,
        "method": "events.subscribe",
        "params": {"subscriptions": subscriptions},
    });
    let mut encoded = serde_json::to_vec(&request).map_err(|error| HerdrError {
        code: "encode_error".to_owned(),
        message: error.to_string(),
    })?;
    encoded.push(b'\n');
    stream.write_all(&encoded).map_err(stream_error)?;

    Ok(EventStream {
        stream,
        buffer: Vec::with_capacity(8192),
        deadline: Instant::now() + duration,
    })
}

#[cfg(windows)]
fn subscribe_socket(
    _client: &HerdrClient,
    _subscriptions: Vec<Value>,
    _duration: Duration,
) -> Result<EventStream, HerdrError> {
    Err(HerdrError {
        code: "unsupported_transport".to_owned(),
        message: "Windows named-pipe event transport has not landed yet".to_owned(),
    })
}

#[cfg(unix)]
fn next_event_until(
    stream: &mut EventStream,
    call_deadline: Instant,
) -> Result<Option<Value>, HerdrError> {
    loop {
        if let Some(line) = take_line(&mut stream.buffer) {
            if let Some(event) = parse_event_line(&line)? {
                return Ok(Some(event));
            }
            continue;
        }
        if Instant::now() >= call_deadline {
            return Ok(None);
        }

        let mut chunk = [0_u8; 8192];
        match stream.stream.read(&mut chunk) {
            Ok(0) => return Ok(None),
            Ok(count) => {
                if stream.buffer.len().saturating_add(count) > MAX_EVENT_LINE_BYTES {
                    return Err(HerdrError {
                        code: "event_frame_too_large".to_owned(),
                        message: format!(
                            "events.subscribe frame exceeded {MAX_EVENT_LINE_BYTES} bytes"
                        ),
                    });
                }
                stream.buffer.extend_from_slice(&chunk[..count]);
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                continue;
            }
            Err(error) => return Err(stream_error(error)),
        }
    }
}

#[cfg(windows)]
fn next_event_until(
    _stream: &mut EventStream,
    _call_deadline: Instant,
) -> Result<Option<Value>, HerdrError> {
    Err(HerdrError {
        code: "unsupported_transport".to_owned(),
        message: "Windows named-pipe event transport has not landed yet".to_owned(),
    })
}

fn take_line(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let newline = buffer.iter().position(|byte| *byte == b'\n')?;
    let mut line = buffer.drain(..=newline).collect::<Vec<_>>();
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Some(line)
}

fn parse_event_line(line: &[u8]) -> Result<Option<Value>, HerdrError> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let envelope: Value = serde_json::from_slice(line).map_err(|error| HerdrError {
        code: "parse_error".to_owned(),
        message: format!("invalid events.subscribe envelope: {error}"),
    })?;

    if let Some(error) = envelope.get("error") {
        return Err(HerdrError {
            code: error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("event_stream_error")
                .to_owned(),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        });
    }

    let Some(event) = envelope.get("event") else {
        return Ok(None);
    };
    if event.is_object() {
        return Ok(Some(event.clone()));
    }
    let Some(event_name) = event.as_str() else {
        return Ok(None);
    };
    let Some(mut payload) = envelope.get("data").and_then(Value::as_object).cloned() else {
        return Ok(None);
    };
    payload.insert("event".to_owned(), json!(event_name));
    Ok(Some(Value::Object(payload)))
}

fn stream_error(error: std::io::Error) -> HerdrError {
    HerdrError {
        code: if error.kind() == std::io::ErrorKind::TimedOut
            || error.kind() == std::io::ErrorKind::WouldBlock
        {
            "timeout"
        } else {
            "socket_error"
        }
        .to_owned(),
        message: error.to_string(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEST_SOCKET: AtomicU64 = AtomicU64::new(1);

    fn socket_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_TEST_SOCKET.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "herdr-mcp-events-{}-{unique}-{sequence}.sock",
            std::process::id()
        ))
    }

    #[test]
    fn normalizes_string_and_object_event_envelopes() {
        let socket = socket_path();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(peer.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["method"], "events.subscribe");
            assert_eq!(
                request["params"]["subscriptions"][0]["type"],
                "pane.updated"
            );

            writeln!(
                peer,
                "{}",
                json!({"id": request["id"], "result": {"ok": true}})
            )
            .unwrap();
            writeln!(
                peer,
                "{}",
                json!({
                    "event": "pane_updated",
                    "data": {"type": "pane_updated", "pane": {"pane_id": "p1"}}
                })
            )
            .unwrap();
            writeln!(
                peer,
                "{}",
                json!({"event": {"event": "pane_closed", "pane_id": "p1"}})
            )
            .unwrap();
        });

        let client = HerdrClient::new(&socket);
        let mut stream = EventStream::subscribe(
            &client,
            vec![json!({"type": "pane.updated"})],
            Duration::from_secs(1),
        )
        .unwrap();
        let first = stream.next_event().unwrap().unwrap();
        let second = stream.next_event().unwrap().unwrap();
        assert_eq!(first["event"], "pane_updated");
        assert_eq!(first["pane"]["pane_id"], "p1");
        assert_eq!(second["event"], "pane_closed");

        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }

    #[test]
    fn daemon_error_ends_subscription() {
        let socket = socket_path();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(peer.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            writeln!(
                peer,
                "{}",
                json!({"error": {"code": "invalid_subscription", "message": "bad"}})
            )
            .unwrap();
        });

        let client = HerdrClient::new(&socket);
        let mut stream = EventStream::subscribe(&client, vec![], Duration::from_secs(1)).unwrap();
        let error = stream.next_event().unwrap_err();
        assert_eq!(error.code, "invalid_subscription");
        assert_eq!(error.message, "bad");

        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}
