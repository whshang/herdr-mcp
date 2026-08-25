use crate::mutation;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

const MAX_BUFFER_PER_STREAM: usize = 512 * 1024;
const SESSION_TTL_MS: u64 = 60 * 60_000;
const KILL_GRACE: Duration = Duration::from_millis(1500);
const JOURNAL_MAX_ENTRIES: usize = 64;
static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum StreamKind {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone)]
struct Chunk {
    seq: u64,
    stream: StreamKind,
    data: Vec<u8>,
}

#[derive(Debug, Default)]
struct Buffers {
    chunks: Vec<Chunk>,
    next_seq: u64,
    stdout_bytes: usize,
    stderr_bytes: usize,
    truncated: bool,
}

#[derive(Debug, Default)]
struct SessionStatus {
    closed: bool,
    exit_code: Option<i32>,
    signal: Option<String>,
    ended_at_ms: Option<u64>,
}

#[derive(Debug)]
struct Session {
    id: String,
    cwd: PathBuf,
    command: String,
    started_at_ms: u64,
    pid: u32,
    child: Mutex<Child>,
    buffers: Mutex<Buffers>,
    status: Mutex<SessionStatus>,
}

#[derive(Debug)]
struct RegistryInner {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    recovered: Mutex<HashMap<String, String>>,
    journal_lock: Mutex<()>,
    journal_path: PathBuf,
    reaped_on_boot: usize,
    detached_on_boot: usize,
    closed_on_boot: usize,
}

#[derive(Debug, Default)]
struct RecoveryResult {
    states: HashMap<String, String>,
    reaped: usize,
    detached: usize,
    closed: usize,
}

#[derive(Clone, Debug)]
pub struct ExecRegistry {
    inner: Arc<RegistryInner>,
}

impl ExecRegistry {
    pub fn new(state_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&state_dir)
            .map_err(|error| format!("cannot create exec state directory: {error}"))?;
        #[cfg(unix)]
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure exec state directory: {error}"))?;
        let journal_path = state_dir.join("exec-sessions-rust.json");
        let recovery = recover_journal(&journal_path);
        write_journal_value(&journal_path, &json!({"sessions": []}))?;
        Ok(Self {
            inner: Arc::new(RegistryInner {
                sessions: Mutex::new(HashMap::new()),
                recovered: Mutex::new(recovery.states),
                journal_lock: Mutex::new(()),
                journal_path,
                reaped_on_boot: recovery.reaped,
                detached_on_boot: recovery.detached,
                closed_on_boot: recovery.closed,
            }),
        })
    }

    pub fn start(&self, cwd: &Path, command: &str) -> Result<Value, String> {
        self.prune();
        if command.is_empty() {
            return Err("command must not be empty".to_owned());
        }
        let id = new_session_id();
        let mut process = shell_command(command);
        process
            .current_dir(cwd)
            .env("HERDR_MCP_EXEC_ID", &id)
            .env("PATH", enriched_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        process.process_group(0);
        let mut child = process
            .spawn()
            .map_err(|error| format!("cannot start background command: {error}"))?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let session = Arc::new(Session {
            id: id.clone(),
            cwd: cwd.to_path_buf(),
            command: command.to_owned(),
            started_at_ms: now_ms(),
            pid,
            child: Mutex::new(child),
            buffers: Mutex::new(Buffers::default()),
            status: Mutex::new(SessionStatus::default()),
        });
        if let Some(stdout) = stdout {
            spawn_reader(Arc::clone(&session), StreamKind::Stdout, stdout);
        }
        if let Some(stderr) = stderr {
            spawn_reader(Arc::clone(&session), StreamKind::Stderr, stderr);
        }
        self.inner
            .sessions
            .lock()
            .map_err(|_| "exec registry lock poisoned".to_owned())?
            .insert(id.clone(), Arc::clone(&session));
        self.save_journal();
        spawn_monitor(Arc::clone(&session), Arc::downgrade(&self.inner));
        Ok(json!({
            "ok": true,
            "session_id": id,
            "cwd": cwd.to_string_lossy(),
            "command": command,
            "started_at": iso_from_ms(session.started_at_ms),
            "pid": pid,
        }))
    }

    pub fn read(&self, id: &str, stream: &str, offset: usize, limit: usize) -> Value {
        self.prune();
        let Some(session) = self.session(id) else {
            return self.recovered_or_missing(id);
        };
        let Some(stream_filter) = parse_stream(stream) else {
            return json!({"ok": false, "code": "invalid_params", "message": "stream must be stdout, stderr, or both"});
        };
        let (bytes, truncated) = {
            let Ok(buffers) = session.buffers.lock() else {
                return json!({"ok": false, "reason": "session_state_unavailable"});
            };
            let mut chunks = buffers.chunks.clone();
            chunks.sort_by_key(|chunk| chunk.seq);
            let bytes = chunks
                .into_iter()
                .filter(|chunk| stream_filter.is_none_or(|wanted| chunk.stream == wanted))
                .flat_map(|chunk| chunk.data)
                .collect::<Vec<_>>();
            (bytes, buffers.truncated)
        };
        let end = offset.saturating_add(limit).min(bytes.len());
        let slice = if offset >= bytes.len() {
            &[][..]
        } else {
            &bytes[offset..end]
        };
        let status = session_status(&session);
        json!({
            "ok": true,
            "session_id": id,
            "running": !status.closed,
            "exit_code": status.exit_code,
            "signal": status.signal,
            "truncated": truncated,
            "stream": stream,
            "offset": offset,
            "text": String::from_utf8_lossy(slice),
            "next_offset": offset.saturating_add(slice.len()),
            "bytes_total": bytes.len(),
        })
    }

    pub fn kill(&self, id: &str) -> Value {
        self.prune();
        let Some(session) = self.session(id) else {
            return self.recovered_or_missing(id);
        };
        let status = session_status(&session);
        if status.closed {
            return json!({
                "ok": true,
                "session_id": id,
                "killed": false,
                "exit_code": status.exit_code,
                "signal": status.signal,
            });
        }
        terminate_session(&session, false);
        let weak = Arc::downgrade(&session);
        thread::spawn(move || {
            thread::sleep(KILL_GRACE);
            if let Some(session) = weak.upgrade()
                && !session_status(&session).closed
            {
                terminate_session(&session, true);
            }
        });
        json!({
            "ok": true,
            "session_id": id,
            "killed": true,
            "exit_code": Value::Null,
            "signal": Value::Null,
        })
    }

    pub fn list_views(&self) -> Vec<Value> {
        self.prune();
        let sessions = self
            .inner
            .sessions
            .lock()
            .map(|sessions| sessions.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut views = sessions.iter().map(session_view).collect::<Vec<_>>();
        if let Ok(recovered) = self.inner.recovered.lock() {
            views.extend(recovered.iter().map(|(id, state)| {
                json!({
                    "session_id": id,
                    "cwd": Value::Null,
                    "command": Value::Null,
                    "started_at": Value::Null,
                    "running": false,
                    "exit_code": Value::Null,
                    "signal": Value::Null,
                    "truncated": false,
                    "recovered": true,
                    "recovery_state": state,
                })
            }));
        }
        views.sort_by(|left, right| {
            left.get("started_at")
                .and_then(Value::as_str)
                .cmp(&right.get("started_at").and_then(Value::as_str))
        });
        views
    }

    pub fn diagnostics(&self) -> Value {
        let views = self.list_views();
        let running = views
            .iter()
            .filter(|view| view.get("running").and_then(Value::as_bool) == Some(true))
            .count();
        json!({
            "ready": true,
            "count": views.len(),
            "running": running,
            "reaped_on_boot": self.inner.reaped_on_boot,
            "detached_on_boot": self.inner.detached_on_boot,
            "closed_on_boot": self.inner.closed_on_boot,
        })
    }

    fn recovered_or_missing(&self, id: &str) -> Value {
        let state = self
            .inner
            .recovered
            .lock()
            .ok()
            .and_then(|recovered| recovered.get(id).cloned());
        match state {
            Some(state) => json!({
                "ok": false,
                "reason": "session_recovered_after_restart",
                "session_id": id,
                "recovery_state": state,
                "hint": "the previous runtime no longer owns a live handle; observe process/filesystem evidence before deciding whether to repeat work",
            }),
            None => json!({"ok": false, "reason": "session_not_found"}),
        }
    }

    fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.inner
            .sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(id).cloned())
    }

    fn prune(&self) {
        let now = now_ms();
        let changed = if let Ok(mut sessions) = self.inner.sessions.lock() {
            let before = sessions.len();
            sessions.retain(|_, session| {
                let status = session_status(session);
                status
                    .ended_at_ms
                    .is_none_or(|ended| now.saturating_sub(ended) <= SESSION_TTL_MS)
            });
            sessions.len() != before
        } else {
            false
        };
        if changed {
            self.save_journal();
        }
    }

    fn save_journal(&self) {
        save_journal_inner(&self.inner);
    }
}

fn session_view(session: &Arc<Session>) -> Value {
    let status = session_status(session);
    let truncated = session
        .buffers
        .lock()
        .map(|buffers| buffers.truncated)
        .unwrap_or(false);
    let command = if session.command.chars().count() > 200 {
        format!("{}…", session.command.chars().take(200).collect::<String>())
    } else {
        session.command.clone()
    };
    json!({
        "session_id": session.id,
        "cwd": session.cwd.to_string_lossy(),
        "command": command,
        "started_at": iso_from_ms(session.started_at_ms),
        "running": !status.closed,
        "exit_code": status.exit_code,
        "signal": status.signal,
        "truncated": truncated,
    })
}

fn session_status(session: &Arc<Session>) -> SessionStatus {
    session.status.lock().map_or_else(
        |_| SessionStatus {
            closed: true,
            exit_code: None,
            signal: None,
            ended_at_ms: Some(now_ms()),
        },
        |status| SessionStatus {
            closed: status.closed,
            exit_code: status.exit_code,
            signal: status.signal.clone(),
            ended_at_ms: status.ended_at_ms,
        },
    )
}

fn spawn_reader<R>(session: Arc<Session>, stream: StreamKind, mut reader: R)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            push_chunk(&session, stream, &buffer[..read]);
        }
    });
}

fn push_chunk(session: &Arc<Session>, stream: StreamKind, chunk: &[u8]) {
    let Ok(mut buffers) = session.buffers.lock() else {
        return;
    };
    let used = match stream {
        StreamKind::Stdout => buffers.stdout_bytes,
        StreamKind::Stderr => buffers.stderr_bytes,
    };
    if used >= MAX_BUFFER_PER_STREAM {
        buffers.truncated = true;
        return;
    }
    let room = MAX_BUFFER_PER_STREAM - used;
    let take = chunk.len().min(room);
    let seq = buffers.next_seq;
    buffers.next_seq = buffers.next_seq.saturating_add(1);
    buffers.chunks.push(Chunk {
        seq,
        stream,
        data: chunk[..take].to_vec(),
    });
    match stream {
        StreamKind::Stdout => buffers.stdout_bytes += take,
        StreamKind::Stderr => buffers.stderr_bytes += take,
    }
    if take < chunk.len() {
        buffers.truncated = true;
    }
}

fn spawn_monitor(session: Arc<Session>, registry: Weak<RegistryInner>) {
    thread::spawn(move || {
        loop {
            let result = session
                .child
                .lock()
                .map_err(|_| "child lock poisoned".to_owned())
                .and_then(|mut child| child.try_wait().map_err(|error| error.to_string()));
            match result {
                Ok(Some(exit)) => {
                    mark_closed(&session, &exit);
                    if let Some(registry) = registry.upgrade() {
                        save_journal_inner(&registry);
                    }
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(25)),
                Err(_) => {
                    if let Ok(mut status) = session.status.lock() {
                        status.closed = true;
                        status.ended_at_ms = Some(now_ms());
                    }
                    if let Some(registry) = registry.upgrade() {
                        save_journal_inner(&registry);
                    }
                    break;
                }
            }
        }
    });
}

fn mark_closed(session: &Arc<Session>, exit: &ExitStatus) {
    if let Ok(mut status) = session.status.lock() {
        status.closed = true;
        status.exit_code = exit.code();
        status.signal = exit_signal(exit);
        status.ended_at_ms = Some(now_ms());
    }
}

fn parse_stream(stream: &str) -> Option<Option<StreamKind>> {
    match stream {
        "both" => Some(None),
        "stdout" => Some(Some(StreamKind::Stdout)),
        "stderr" => Some(Some(StreamKind::Stderr)),
        _ => None,
    }
}

fn new_session_id() -> String {
    let seq = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    format!("es_{:x}-{:x}-{:x}", std::process::id(), now_ms(), seq)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn iso_from_ms(ms: u64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|value| value.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

fn enriched_path() -> String {
    let mut values = Vec::<String>::new();
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        for relative in [
            ".local/bin",
            ".cargo/bin",
            ".npm-global/bin",
            ".local/share/mise/shims",
        ] {
            let candidate = home.join(relative);
            if candidate.is_dir() {
                values.push(candidate.to_string_lossy().into_owned());
            }
        }
    }
    for candidate in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        if Path::new(candidate).is_dir() {
            values.push(candidate.to_owned());
        }
    }
    if let Some(current) = env::var_os("PATH") {
        values.extend(env::split_paths(&current).map(|path| path.to_string_lossy().into_owned()));
    }
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(value.clone()));
    env::join_paths(values.iter().map(Path::new))
        .ok()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| values.join(if cfg!(windows) { ";" } else { ":" }))
}

#[cfg(unix)]
fn shell_command(command: &str) -> Command {
    let shell = resolve_unix_shell();
    let mut process = Command::new(shell);
    process.args(["-lc", command]);
    process
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut process = Command::new("powershell.exe");
    process.args(["-NoProfile", "-Command", command]);
    process
}

#[cfg(unix)]
fn resolve_unix_shell() -> PathBuf {
    let mut candidates = Vec::new();
    for key in ["HERDR_MCP_EXEC_SHELL", "SHELL"] {
        if let Some(value) = env::var_os(key) {
            candidates.push(PathBuf::from(value));
        }
    }
    candidates.extend(
        ["/bin/zsh", "/bin/bash", "/bin/sh"]
            .into_iter()
            .map(PathBuf::from),
    );
    candidates
        .into_iter()
        .find(|candidate| is_executable_shell(candidate))
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

#[cfg(unix)]
fn is_executable_shell(path: &Path) -> bool {
    let allowed = matches!(
        path.file_name().and_then(|value| value.to_str()),
        Some("zsh" | "bash" | "sh")
    );
    allowed
        && fs::metadata(path).ok().is_some_and(|metadata| {
            metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
        })
}

#[cfg(unix)]
fn exit_signal(status: &ExitStatus) -> Option<String> {
    status.signal().map(signal_name)
}

#[cfg(windows)]
fn exit_signal(_status: &ExitStatus) -> Option<String> {
    None
}

#[cfg(unix)]
fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGTERM => "SIGTERM".to_owned(),
        libc::SIGKILL => "SIGKILL".to_owned(),
        libc::SIGINT => "SIGINT".to_owned(),
        libc::SIGHUP => "SIGHUP".to_owned(),
        other => format!("SIG{other}"),
    }
}

fn terminate_session(session: &Arc<Session>, force: bool) {
    #[cfg(unix)]
    {
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        let group = -(session.pid as i32);
        let delivered = unsafe { libc::kill(group, signal) } == 0;
        if !delivered && let Ok(mut child) = session.child.lock() {
            let _ = child.kill();
        }
    }
    #[cfg(windows)]
    {
        if let Ok(mut child) = session.child.lock() {
            let _ = child.kill();
        }
    }
}

fn save_journal_inner(inner: &RegistryInner) {
    let Ok(_journal_guard) = inner.journal_lock.lock() else {
        return;
    };
    let entries = inner
        .sessions
        .lock()
        .map(|sessions| {
            sessions
                .values()
                .filter(|session| !session_status(session).closed)
                .take(JOURNAL_MAX_ENTRIES)
                .map(|session| {
                    json!({
                        "id": session.id,
                        "pid": session.pid,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let _ = write_journal_value(&inner.journal_path, &json!({"sessions": entries}));
}

fn write_journal_value(path: &Path, value: &Value) -> Result<(), String> {
    if !path.exists() {
        #[cfg(unix)]
        {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .map_err(|error| format!("cannot create secure exec journal: {error}"))?;
        }
        #[cfg(windows)]
        {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|error| format!("cannot create exec journal: {error}"))?;
        }
    }
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("cannot encode exec journal: {error}"))?;
    mutation::atomic_write(path, &bytes)?;
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("cannot secure exec journal: {error}"))?;
    }
    Ok(())
}

fn recover_journal(path: &Path) -> RecoveryResult {
    let Ok(data) = fs::read(path) else {
        return RecoveryResult::default();
    };
    let Ok(value) = serde_json::from_slice::<Value>(&data) else {
        return RecoveryResult::default();
    };
    let entries = value
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    #[cfg(unix)]
    {
        let mut result = RecoveryResult::default();
        let mut kill_later = Vec::new();
        for entry in entries.into_iter().take(JOURNAL_MAX_ENTRIES) {
            let Some(id) = entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(pid) = entry
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
            else {
                continue;
            };
            if !process_alive(pid) {
                result
                    .states
                    .insert(id.to_owned(), "closed_before_restart".to_owned());
                result.closed += 1;
                continue;
            }
            if !process_has_marker(pid, id) {
                result
                    .states
                    .insert(id.to_owned(), "detached_unverified".to_owned());
                result.detached += 1;
                continue;
            }
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
            result
                .states
                .insert(id.to_owned(), "reaped_on_restart".to_owned());
            result.reaped += 1;
            kill_later.push((pid, id.to_owned()));
        }
        if !kill_later.is_empty() {
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(2));
                for (pid, id) in kill_later {
                    if process_has_marker(pid, &id) {
                        unsafe {
                            libc::kill(-(pid as i32), libc::SIGKILL);
                        }
                    }
                }
            });
        }
        result
    }
    #[cfg(windows)]
    {
        let mut result = RecoveryResult::default();
        for entry in entries.into_iter().take(JOURNAL_MAX_ENTRIES) {
            let Some(id) = entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(pid) = entry
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
            else {
                continue;
            };
            let state = if process_alive(pid) {
                result.detached += 1;
                "detached_unverified"
            } else {
                result.closed += 1;
                "closed_before_restart"
            };
            result.states.insert(id.to_owned(), state.to_owned());
        }
        result
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .is_some_and(|output| output.status.success() && !output.stdout.is_empty())
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .is_some_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
        })
}

#[cfg(unix)]
fn process_has_marker(pid: u32, id: &str) -> bool {
    let output = Command::new("ps")
        .args(["eww", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    output.ok().is_some_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains(&format!("HERDR_MCP_EXEC_ID={id}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ExecRegistry {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-registry-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        ExecRegistry::new(path).unwrap()
    }

    #[test]
    fn shell_resolution_is_compatible() {
        #[cfg(unix)]
        assert!(matches!(
            resolve_unix_shell()
                .file_name()
                .and_then(|value| value.to_str()),
            Some("zsh" | "bash" | "sh")
        ));
    }

    #[test]
    fn session_captures_output_and_exit() {
        let registry = registry();
        let started = registry
            .start(Path::new("/tmp"), "printf out; printf err >&2; exit 7")
            .unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();
        for _ in 0..100 {
            let view = registry.read(&id, "both", 0, 65536);
            if view["running"] == false {
                assert_eq!(view["exit_code"], 7);
                let text = view["text"].as_str().unwrap();
                assert!(text.contains("out"));
                assert!(text.contains("err"));
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("session did not exit");
    }

    #[test]
    fn kill_stops_long_session() {
        let registry = registry();
        let started = registry.start(Path::new("/tmp"), "sleep 30").unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();
        assert_eq!(registry.kill(&id)["killed"], true);
        for _ in 0..100 {
            let view = registry.read(&id, "both", 0, 64);
            if view["running"] == false {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("killed session did not close");
    }

    #[test]
    fn journal_keeps_only_process_fencing_identity() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-journal-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let registry = ExecRegistry::new(path.clone()).unwrap();
        let started = registry.start(Path::new("/tmp"), "sleep 30").unwrap();
        let id = started["session_id"].as_str().unwrap();
        let journal: Value =
            serde_json::from_slice(&fs::read(path.join("exec-sessions-rust.json")).unwrap())
                .unwrap();
        let entry = &journal["sessions"][0];
        assert_eq!(entry["id"], id);
        assert!(entry.get("pid").is_some());
        assert!(entry.get("command").is_none());
        assert!(entry.get("cwd").is_none());
        assert!(entry.get("started_at_ms").is_none());
        let _ = registry.kill(id);
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn restart_tombstone_is_truthful_for_closed_previous_session() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-recovery-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("exec-sessions-rust.json"),
            br#"{"sessions":[{"id":"es_previous","pid":4294967295}]}"#,
        )
        .unwrap();
        let registry = ExecRegistry::new(path.clone()).unwrap();
        let recovered = registry.read("es_previous", "both", 0, 64);
        assert_eq!(recovered["reason"], "session_recovered_after_restart");
        assert_eq!(recovered["recovery_state"], "closed_before_restart");
        assert_eq!(registry.diagnostics()["closed_on_boot"], 1);
        assert!(
            registry
                .list_views()
                .iter()
                .any(|view| view["session_id"] == "es_previous" && view["recovered"] == true)
        );
        fs::remove_dir_all(path).unwrap();
    }
}
