use crate::exec_compact;
use crate::state_store::{ExecSessionFence, StateStore};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

const MAX_BUFFER_PER_STREAM: usize = 512 * 1024;
const SESSION_TTL_MS: u64 = 60 * 60_000;
const KILL_GRACE: Duration = Duration::from_millis(1500);
const OUTPUT_DRAIN_BUDGET: Duration = Duration::from_millis(500);
const RECOVERY_MAX_ENTRIES: usize = 64;
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
    output_readers: AtomicUsize,
    status: Mutex<SessionStatus>,
}

#[derive(Debug)]
struct RegistryInner {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    recovered: Mutex<HashMap<String, String>>,
    state_store: Mutex<StateStore>,
    persistence_failures: AtomicUsize,
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
        let state_store = StateStore::open_in_dir(&state_dir, "state.db")?;
        state_store.prune_exec_sessions(now_ms())?;
        let recovery = recover_state_store(&state_store)?;
        Ok(Self {
            inner: Arc::new(RegistryInner {
                sessions: Mutex::new(HashMap::new()),
                recovered: Mutex::new(recovery.states),
                state_store: Mutex::new(state_store),
                persistence_failures: AtomicUsize::new(0),
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
        let mut process = shell_command(command, &id);
        process
            .current_dir(cwd)
            .env("HERDR_MCP_EXEC_ID", &id)
            .env("PATH", enriched_exec_path())
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
        let output_readers = usize::from(stdout.is_some()) + usize::from(stderr.is_some());
        let session = Arc::new(Session {
            id: id.clone(),
            cwd: cwd.to_path_buf(),
            command: command.to_owned(),
            started_at_ms: now_ms(),
            pid,
            child: Mutex::new(child),
            buffers: Mutex::new(Buffers::default()),
            output_readers: AtomicUsize::new(output_readers),
            status: Mutex::new(SessionStatus::default()),
        });
        if let Some(stdout) = stdout {
            spawn_reader(Arc::clone(&session), StreamKind::Stdout, stdout);
        }
        if let Some(stderr) = stderr {
            spawn_reader(Arc::clone(&session), StreamKind::Stderr, stderr);
        }
        let mut sessions = match self.inner.sessions.lock() {
            Ok(sessions) => sessions,
            Err(_) => {
                terminate_and_wait(&session);
                return Err("exec registry lock poisoned".to_owned());
            }
        };
        let process_group = process_group_for_session(pid);
        let persist_result = self
            .inner
            .state_store
            .lock()
            .map_err(|_| "exec state store lock poisoned".to_owned())
            .and_then(|store| {
                store.record_exec_running(&id, pid, process_group, session.started_at_ms)
            });
        if let Err(error) = persist_result {
            terminate_and_wait(&session);
            return Err(format!(
                "cannot durably register exec session; process terminated before return: {error}"
            ));
        }
        sessions.insert(id.clone(), Arc::clone(&session));
        drop(sessions);
        spawn_monitor(Arc::clone(&session), Arc::downgrade(&self.inner));
        Ok(json!({
            "ok": true,
            "session_id": id,
            "cwd": cwd.to_string_lossy(),
            "command": command,
            "started_at": iso_from_ms(session.started_at_ms),
            "pid": pid,
            "phase": "started",
            "progress": {
                "bytes_read": 0,
                "bytes_total": 0,
                "elapsed_ms": 0,
            },
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
        refresh_session_status(&session, Some(&self.inner));
        let (bytes, bytes_total, truncated) = {
            let Ok(buffers) = session.buffers.lock() else {
                return json!({"ok": false, "reason": "session_state_unavailable"});
            };
            let (bytes, bytes_total) = read_buffer_slice(&buffers, stream_filter, offset, limit);
            (bytes, bytes_total, buffers.truncated)
        };
        let status = session_status(&session);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let next_offset = offset.saturating_add(bytes.len());
        let finished_success_snapshot = status.closed
            && status.exit_code == Some(0)
            && !truncated
            && offset == 0
            && bytes.len() == bytes_total;
        let phase = if status.closed {
            "completed"
        } else {
            "running"
        };
        let elapsed_ms = session_elapsed_ms(&session, &status);
        let mut result = Map::new();
        result.insert("ok".to_owned(), json!(true));
        result.insert("session_id".to_owned(), json!(id));
        result.insert("running".to_owned(), json!(!status.closed));
        result.insert("phase".to_owned(), json!(phase));
        result.insert("exit_code".to_owned(), json!(status.exit_code));
        result.insert("signal".to_owned(), json!(status.signal));
        result.insert("truncated".to_owned(), json!(truncated));
        result.insert("stream".to_owned(), json!(stream));
        result.insert("offset".to_owned(), json!(offset));
        result.insert("next_offset".to_owned(), json!(next_offset));
        result.insert("bytes_total".to_owned(), json!(bytes_total));
        result.insert(
            "progress".to_owned(),
            json!({
                "bytes_read": next_offset,
                "bytes_total": bytes_total,
                "elapsed_ms": elapsed_ms,
            }),
        );
        exec_compact::insert_compacted_or_raw(
            &mut result,
            "text",
            &text,
            finished_success_snapshot,
        );
        Value::Object(result)
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
        let (state_store_ready, state_store_schema) = self
            .inner
            .state_store
            .lock()
            .ok()
            .and_then(|store| store.schema_version().ok().map(|version| (true, version)))
            .unwrap_or((false, 0));
        json!({
            "ready": true,
            "count": views.len(),
            "running": running,
            "reaped_on_boot": self.inner.reaped_on_boot,
            "detached_on_boot": self.inner.detached_on_boot,
            "closed_on_boot": self.inner.closed_on_boot,
            "state_store_ready": state_store_ready,
            "state_store_schema": state_store_schema,
            "persistence_failures": self.inner.persistence_failures.load(Ordering::Relaxed),
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
        if let Ok(mut sessions) = self.inner.sessions.lock() {
            sessions.retain(|_, session| {
                let status = session_status(session);
                status
                    .ended_at_ms
                    .is_none_or(|ended| now.saturating_sub(ended) <= SESSION_TTL_MS)
            });
        }
        if let Ok(store) = self.inner.state_store.lock()
            && store.prune_exec_sessions(now).is_err()
        {
            self.inner
                .persistence_failures
                .fetch_add(1, Ordering::Relaxed);
        }
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

fn session_elapsed_ms(session: &Session, status: &SessionStatus) -> u64 {
    let end_ms = if status.closed {
        status.ended_at_ms.unwrap_or_else(now_ms)
    } else {
        now_ms()
    };
    end_ms.saturating_sub(session.started_at_ms)
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
        session.output_readers.fetch_sub(1, Ordering::Release);
    });
}

fn wait_for_output_readers(session: &Arc<Session>) {
    let deadline = Instant::now() + OUTPUT_DRAIN_BUDGET;
    while session.output_readers.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
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

fn read_buffer_slice(
    buffers: &Buffers,
    stream_filter: Option<StreamKind>,
    offset: usize,
    limit: usize,
) -> (Vec<u8>, usize) {
    debug_assert!(
        buffers
            .chunks
            .windows(2)
            .all(|pair| pair[0].seq <= pair[1].seq)
    );
    let bytes_total = match stream_filter {
        Some(StreamKind::Stdout) => buffers.stdout_bytes,
        Some(StreamKind::Stderr) => buffers.stderr_bytes,
        None => buffers.stdout_bytes.saturating_add(buffers.stderr_bytes),
    };
    if limit == 0 || offset >= bytes_total {
        return (Vec::new(), bytes_total);
    }

    let requested_end = offset.saturating_add(limit).min(bytes_total);
    let mut logical_offset = 0usize;
    let mut output = Vec::with_capacity(requested_end.saturating_sub(offset));
    for chunk in &buffers.chunks {
        if stream_filter.is_some_and(|wanted| chunk.stream != wanted) {
            continue;
        }
        let chunk_start = logical_offset;
        let chunk_end = chunk_start.saturating_add(chunk.data.len());
        logical_offset = chunk_end;
        if chunk_end <= offset {
            continue;
        }
        if chunk_start >= requested_end {
            break;
        }
        let start = offset.saturating_sub(chunk_start).min(chunk.data.len());
        let end = requested_end
            .saturating_sub(chunk_start)
            .min(chunk.data.len());
        if start < end {
            output.extend_from_slice(&chunk.data[start..end]);
        }
    }
    (output, bytes_total)
}

fn spawn_monitor(session: Arc<Session>, registry: Weak<RegistryInner>) {
    thread::spawn(move || {
        loop {
            let registry = registry.upgrade();
            if refresh_session_status(&session, registry.as_deref()) {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
    });
}

fn refresh_session_status(session: &Arc<Session>, registry: Option<&RegistryInner>) -> bool {
    if session_status(session).closed {
        return true;
    }
    let result = session
        .child
        .lock()
        .map_err(|_| "child lock poisoned".to_owned())
        .and_then(|mut child| child.try_wait().map_err(|error| error.to_string()));
    let transitioned = match result {
        Ok(Some(exit)) => {
            wait_for_output_readers(session);
            mark_closed(session, &exit)
        }
        Ok(None) => return false,
        Err(_) => mark_closed_unknown(session),
    };
    if transitioned && let Some(registry) = registry {
        persist_closed_session(registry, session);
    }
    true
}

fn mark_closed(session: &Arc<Session>, exit: &ExitStatus) -> bool {
    let Ok(mut status) = session.status.lock() else {
        return false;
    };
    if status.closed {
        return false;
    }
    status.closed = true;
    status.exit_code = exit.code();
    status.signal = exit_signal(exit);
    status.ended_at_ms = Some(now_ms());
    true
}

fn mark_closed_unknown(session: &Arc<Session>) -> bool {
    let Ok(mut status) = session.status.lock() else {
        return false;
    };
    if status.closed {
        return false;
    }
    status.closed = true;
    status.ended_at_ms = Some(now_ms());
    true
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

pub fn enriched_exec_path() -> String {
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
fn shell_command(command: &str, id: &str) -> Command {
    let shell = resolve_exec_shell();
    let marker = process_marker(id);
    let mut process = Command::new(&shell);
    process.arg0(&marker).args(["-lc", command]);
    process
}

#[cfg(windows)]
fn shell_command(command: &str, _id: &str) -> Command {
    let mut process = Command::new("powershell.exe");
    process.args(["-NoProfile", "-Command", command]);
    process
}

fn process_marker(id: &str) -> String {
    format!("herdr-mcp-exec:{id}")
}

#[cfg(unix)]
pub fn resolve_exec_shell() -> PathBuf {
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

#[cfg(windows)]
pub fn resolve_exec_shell() -> PathBuf {
    PathBuf::from("powershell.exe")
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

fn terminate_and_wait(session: &Arc<Session>) {
    terminate_session(session, true);
    if let Ok(mut child) = session.child.lock() {
        let _ = child.wait();
    }
}

fn process_group_for_session(pid: u32) -> Option<u32> {
    #[cfg(unix)]
    {
        Some(pid)
    }
    #[cfg(windows)]
    {
        let _ = pid;
        None
    }
}

fn persist_closed_session(inner: &RegistryInner, session: &Arc<Session>) {
    let status = session_status(session);
    let ended_at = status.ended_at_ms.unwrap_or_else(now_ms);
    let result = inner
        .state_store
        .lock()
        .map_err(|_| "exec state store lock poisoned".to_owned())
        .and_then(|store| {
            store.settle_exec_session(
                &session.id,
                "closed",
                Some(ended_at),
                status.exit_code,
                status.signal.as_deref(),
                ended_at.saturating_add(SESSION_TTL_MS),
            )
        });
    if result.is_err() {
        inner.persistence_failures.fetch_add(1, Ordering::Relaxed);
    }
}

fn recover_state_store(store: &StateStore) -> Result<RecoveryResult, String> {
    let entries = store.recoverable_exec_sessions(RECOVERY_MAX_ENTRIES)?;
    let now = now_ms();
    let expires_at = now.saturating_add(SESSION_TTL_MS);
    #[cfg(unix)]
    {
        let mut result = RecoveryResult::default();
        let mut kill_later = Vec::new();
        for ExecSessionFence {
            session_id: id,
            pid,
            process_group,
            ..
        } in entries
        {
            if !process_alive(pid) {
                result
                    .states
                    .insert(id.clone(), "closed_before_restart".to_owned());
                result.closed += 1;
                store.settle_exec_session(
                    &id,
                    "closed_before_restart",
                    Some(now),
                    None,
                    None,
                    expires_at,
                )?;
                continue;
            }
            if process_group != Some(pid)
                || !process_has_marker(pid, &id)
                || process_group_id(pid) != Some(pid)
            {
                result
                    .states
                    .insert(id.clone(), "detached_unverified".to_owned());
                result.detached += 1;
                store.settle_exec_session(
                    &id,
                    "detached_unverified",
                    None,
                    None,
                    None,
                    expires_at,
                )?;
                continue;
            }
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
            result
                .states
                .insert(id.clone(), "reaped_on_restart".to_owned());
            result.reaped += 1;
            store.settle_exec_session(
                &id,
                "reaped_on_restart",
                Some(now),
                None,
                Some("SIGTERM"),
                expires_at,
            )?;
            kill_later.push((pid, id));
        }
        if !kill_later.is_empty() {
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(2));
                for (pid, id) in kill_later {
                    if process_has_marker(pid, &id) && process_group_id(pid) == Some(pid) {
                        unsafe {
                            libc::kill(-(pid as i32), libc::SIGKILL);
                        }
                    }
                }
            });
        }
        Ok(result)
    }
    #[cfg(windows)]
    {
        let mut result = RecoveryResult::default();
        for ExecSessionFence {
            session_id: id,
            pid,
            ..
        } in entries
        {
            let state = if process_alive(pid) {
                result.detached += 1;
                "detached_unverified"
            } else {
                result.closed += 1;
                "closed_before_restart"
            };
            result.states.insert(id.clone(), state.to_owned());
            store.settle_exec_session(
                &id,
                state,
                (!process_alive(pid)).then_some(now),
                None,
                None,
                expires_at,
            )?;
        }
        Ok(result)
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

#[cfg(unix)]
fn process_group_id(pid: u32) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pgid="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()
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
    let marker = process_marker(id);
    let command_line = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    if command_line.ok().is_some_and(|output| {
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains(&marker)
    }) {
        return true;
    }
    let environment = Command::new("ps")
        .args(["eww", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    environment.ok().is_some_and(|output| {
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
            resolve_exec_shell()
                .file_name()
                .and_then(|value| value.to_str()),
            Some("zsh" | "bash" | "sh")
        ));
    }

    #[test]
    fn buffer_slice_reads_only_requested_delta_across_chunks() {
        let buffers = Buffers {
            chunks: vec![
                Chunk {
                    seq: 0,
                    stream: StreamKind::Stdout,
                    data: b"abcd".to_vec(),
                },
                Chunk {
                    seq: 1,
                    stream: StreamKind::Stderr,
                    data: b"12".to_vec(),
                },
                Chunk {
                    seq: 2,
                    stream: StreamKind::Stdout,
                    data: b"efgh".to_vec(),
                },
            ],
            next_seq: 3,
            stdout_bytes: 8,
            stderr_bytes: 2,
            truncated: false,
        };

        let (both, both_total) = read_buffer_slice(&buffers, None, 3, 5);
        assert_eq!(both, b"d12ef");
        assert_eq!(both_total, 10);
        let (stdout, stdout_total) = read_buffer_slice(&buffers, Some(StreamKind::Stdout), 3, 3);
        assert_eq!(stdout, b"def");
        assert_eq!(stdout_total, 8);
        let (past_end, total) = read_buffer_slice(&buffers, None, 20, 10);
        assert!(past_end.is_empty());
        assert_eq!(total, 10);
    }

    fn wait_until_closed(registry: &ExecRegistry, id: &str, stream: &str, limit: usize) -> Value {
        for _ in 0..2_000 {
            let view = registry.read(id, stream, 0, limit);
            if view["running"] == false {
                return view;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("session did not exit");
    }

    #[test]
    fn session_captures_output_and_exit() {
        let registry = registry();
        let started = registry
            .start(Path::new("/tmp"), "printf out; printf err >&2; exit 7")
            .unwrap();
        assert_eq!(started["phase"], "started");
        assert_eq!(started["progress"]["bytes_read"], 0);
        assert_eq!(started["progress"]["bytes_total"], 0);
        assert_eq!(started["progress"]["elapsed_ms"], 0);
        let id = started["session_id"].as_str().unwrap().to_owned();
        let view = wait_until_closed(&registry, &id, "both", 65536);
        assert_eq!(view["exit_code"], 7);
        assert_eq!(view["phase"], "completed");
        assert_eq!(view["progress"]["bytes_read"], view["next_offset"]);
        assert_eq!(view["progress"]["bytes_total"], view["bytes_total"]);
        assert!(view["progress"]["elapsed_ms"].as_u64().is_some());
        let text = view["text"].as_str().unwrap();
        assert!(text.contains("out"));
        assert!(text.contains("err"));
        assert!(view.get("compacted").is_none());
    }

    #[test]
    fn small_success_stays_verbatim() {
        let registry = registry();
        let started = registry
            .start(Path::new("/tmp"), "printf 'hello-exec\\n'")
            .unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();
        let view = wait_until_closed(&registry, &id, "stdout", 65536);
        assert_eq!(view["ok"], true);
        assert_eq!(view["exit_code"], 0);
        assert_eq!(view["session_id"], id);
        assert_eq!(view["text"], "hello-exec\n");
        assert!(view.get("compacted").is_none());
        assert!(view.get("counts").is_none());
    }

    #[test]
    fn large_success_compacts_head_and_tail() {
        let registry = registry();
        let started = registry
            .start(
                Path::new("/tmp"),
                "awk 'BEGIN{for(i=0;i<90;i++) print \"line-\" i}'",
            )
            .unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();
        let view = wait_until_closed(&registry, &id, "stdout", 65536);
        assert_eq!(view["ok"], true);
        assert_eq!(view["exit_code"], 0);
        assert_eq!(view["session_id"], id);
        assert_eq!(view["truncated"], false);
        assert_eq!(view["compacted"], true);
        assert_eq!(view["counts"]["lines"], 90);
        assert_eq!(view["counts"]["omitted_lines"], 30);
        let text = view["text"].as_str().unwrap();
        assert!(text.contains("line-0\n"));
        assert!(text.contains("line-19\n"));
        assert!(text.contains("…[omitted 30 lines]…"));
        assert!(text.contains("line-89\n"));
        assert!(!text.contains("line-40\n"));
        assert_eq!(view["next_offset"], view["bytes_total"]);
        assert!(view["bytes_total"].as_u64().unwrap() > text.len() as u64);

        let chunk = registry.read(&id, "stdout", 0, 35);
        assert!(chunk.get("compacted").is_none());
        assert_eq!(
            chunk["text"].as_str().unwrap(),
            "line-0\nline-1\nline-2\nline-3\nline-4\n"
        );
    }

    #[test]
    fn failure_keeps_full_diagnostic_output() {
        let registry = registry();
        let started = registry
            .start(
                Path::new("/tmp"),
                "awk 'BEGIN{for(i=0;i<90;i++) print \"fail-\" i}'; exit 3",
            )
            .unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();
        let view = wait_until_closed(&registry, &id, "stdout", 65536);
        assert_eq!(view["exit_code"], 3);
        assert!(view.get("compacted").is_none());
        let text = view["text"].as_str().unwrap();
        assert!(text.contains("fail-0\n"));
        assert!(text.contains("fail-40\n"));
        assert!(text.contains("fail-89\n"));
        assert!(!text.contains("…[omitted"));
    }

    #[test]
    fn truncated_success_stays_raw() {
        let registry = registry();
        let started = registry
            .start(
                Path::new("/tmp"),
                "awk 'BEGIN{for(i=0;i<25000;i++) print i, \"xxxxxxxxxxxxxxxxxxxx\"}'",
            )
            .unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();
        let view = wait_until_closed(&registry, &id, "stdout", MAX_BUFFER_PER_STREAM);
        assert_eq!(view["exit_code"], 0);
        assert_eq!(view["truncated"], true);
        assert_eq!(view["session_id"], id);
        assert!(view.get("compacted").is_none());
        let text = view["text"].as_str().unwrap();
        assert!(!text.contains("…[omitted"));
        assert_eq!(
            view["bytes_total"].as_u64(),
            Some(MAX_BUFFER_PER_STREAM as u64)
        );
        assert_eq!(text.len(), MAX_BUFFER_PER_STREAM);
    }

    #[test]
    fn in_progress_read_does_not_compact() {
        let registry = registry();
        let started = registry
            .start(
                Path::new("/tmp"),
                "awk 'BEGIN{for(i=0;i<90;i++) print \"line-\" i}'; sleep 30",
            )
            .unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();
        for _ in 0..800 {
            let view = registry.read(&id, "stdout", 0, 65536);
            let text = view["text"].as_str().unwrap_or("");
            if view["running"] == true && text.contains("line-89") {
                assert_eq!(view["phase"], "running");
                assert!(view.get("compacted").is_none());
                assert!(text.contains("line-40\n"));
                assert_eq!(view["progress"]["bytes_read"], view["next_offset"]);
                assert_eq!(view["progress"]["bytes_total"], view["bytes_total"]);
                assert!(view["progress"]["bytes_total"].as_u64().unwrap() > 0);
                assert!(view["progress"]["elapsed_ms"].as_u64().unwrap() > 0);
                assert_eq!(registry.kill(&id)["killed"], true);
                return;
            }
            if view["running"] == false {
                panic!("session exited before in-progress assertion");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = registry.kill(&id);
        panic!("did not observe in-progress output");
    }

    #[test]
    fn in_progress_read_exposes_streaming_progress() {
        let registry = registry();
        let started = registry
            .start(
                Path::new("/tmp"),
                "awk 'BEGIN{for(i=0;i<40;i++) print \"prog-\" i}'; sleep 30",
            )
            .unwrap();
        assert_eq!(started["phase"], "started");
        let id = started["session_id"].as_str().unwrap().to_owned();
        for _ in 0..800 {
            let view = registry.read(&id, "stdout", 0, 65536);
            if view["phase"] == "running"
                && view["progress"]["bytes_total"].as_u64().unwrap_or(0) > 0
            {
                assert_eq!(view["running"], true);
                assert_eq!(view["progress"]["bytes_read"], view["next_offset"]);
                assert_eq!(view["progress"]["bytes_total"], view["bytes_total"]);
                let mid = registry.read(
                    &id,
                    "stdout",
                    view["next_offset"].as_u64().unwrap() as usize / 2,
                    16,
                );
                assert_eq!(mid["phase"], "running");
                assert!(mid["progress"]["bytes_read"].as_u64().unwrap() > 0);
                assert_eq!(mid["progress"]["bytes_total"], view["bytes_total"]);
                assert_eq!(registry.kill(&id)["killed"], true);
                let done = wait_until_closed(&registry, &id, "stdout", 65536);
                assert_eq!(done["phase"], "completed");
                assert_eq!(done["running"], false);
                return;
            }
            if view["running"] == false {
                panic!("session exited before progress assertion");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = registry.kill(&id);
        panic!("did not observe running progress");
    }

    #[test]
    fn read_reaps_completed_session_without_monitor_thread() {
        let registry = registry();
        let id = new_session_id();
        let mut process = shell_command("exit 9", &id);
        process
            .current_dir("/tmp")
            .env("HERDR_MCP_EXEC_ID", &id)
            .env("PATH", enriched_exec_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        process.process_group(0);
        let mut child = process.spawn().unwrap();
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let output_readers = usize::from(stdout.is_some()) + usize::from(stderr.is_some());
        let session = Arc::new(Session {
            id: id.clone(),
            cwd: PathBuf::from("/tmp"),
            command: "exit 9".to_owned(),
            started_at_ms: now_ms(),
            pid,
            child: Mutex::new(child),
            buffers: Mutex::new(Buffers::default()),
            output_readers: AtomicUsize::new(output_readers),
            status: Mutex::new(SessionStatus::default()),
        });
        if let Some(stdout) = stdout {
            spawn_reader(Arc::clone(&session), StreamKind::Stdout, stdout);
        }
        if let Some(stderr) = stderr {
            spawn_reader(Arc::clone(&session), StreamKind::Stderr, stderr);
        }
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .record_exec_running(
                &id,
                pid,
                process_group_for_session(pid),
                session.started_at_ms,
            )
            .unwrap();
        registry
            .inner
            .sessions
            .lock()
            .unwrap()
            .insert(id.clone(), session);

        for _ in 0..500 {
            let view = registry.read(&id, "both", 0, 64);
            if view["running"] == false {
                assert_eq!(view["exit_code"], 9);
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("read did not reap completed session without monitor thread");
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
    fn sqlite_keeps_only_process_fencing_identity() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-state-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let registry = ExecRegistry::new(path.clone()).unwrap();
        let started = registry.start(Path::new("/tmp"), "sleep 30").unwrap();
        let id = started["session_id"].as_str().unwrap();
        let store = StateStore::open_in_dir(&path, "state.db").unwrap();
        assert_eq!(
            store
                .scalar_text("SELECT session_id FROM exec_sessions WHERE state = 'running'")
                .unwrap()
                .as_deref(),
            Some(id)
        );
        let pid = store
            .scalar_i64("SELECT pid FROM exec_sessions WHERE session_id = (SELECT session_id FROM exec_sessions WHERE state = 'running' LIMIT 1)")
            .unwrap()
            .unwrap() as u32;
        #[cfg(unix)]
        {
            assert!(process_has_marker(pid, id));
            assert_eq!(process_group_id(pid), Some(pid));
        }
        assert_eq!(
            store
                .scalar_i64(
                    "SELECT COUNT(*) FROM pragma_table_info('exec_sessions') \
                     WHERE name IN ('command', 'cwd', 'stdout', 'stderr')",
                )
                .unwrap(),
            Some(0)
        );
        let _ = registry.kill(id);
        drop(store);
        drop(registry);
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
        let store = StateStore::open_in_dir(&path, "state.db").unwrap();
        store
            .record_exec_running("es_previous", u32::MAX, Some(u32::MAX), 1)
            .unwrap();
        drop(store);
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
        drop(registry);
        fs::remove_dir_all(path).unwrap();
    }
}
