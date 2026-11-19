use crate::exec_compact;
use crate::herdr::HerdrClient;
use crate::state_store::{ClosedExecSessionRecord, ExecSessionFence, StateStore};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

const MAX_BUFFER_PER_STREAM: usize = 512 * 1024;
const SESSION_TTL_MS: u64 = 60 * 60_000;
const KILL_GRACE: Duration = Duration::from_millis(1500);
const OUTPUT_DRAIN_BUDGET: Duration = Duration::from_millis(500);
const RECOVERY_MAX_ENTRIES: usize = 64;
const PANE_RPC_TIMEOUT: Duration = Duration::from_secs(5);
/// #1010 1b: bounded backoff for transient persistence failures on the
/// session-close path (SQLite busy/locked, momentary filesystem errors).
/// Permanent errors still count once after the last attempt.
const PERSIST_RETRY_ATTEMPTS: u32 = 3;
const PERSIST_RETRY_BACKOFF_MS: u64 = 5;
/// #1010 1b: `.tmp` spool files younger than this are in-flight atomic
/// commits owned by another concurrent close; only a stale leftover from a
/// crashed write is eligible for cleanup.
const SPOOL_TMP_GRACE_MS: u64 = 60_000;
static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);

/// Stage at which a utility-pane exec start failed.
///
/// The stage is owned here, next to the operation that produced it, so callers
/// never have to infer "did anything run?" from an error string.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PaneStartStage {
    /// Failed before `pane.send_text`: nothing was delivered.
    BeforeSend,
    /// Failed at or after `pane.send_text`: delivery cannot be ruled out.
    AfterSend,
}

impl PaneStartStage {
    /// True only when the failure proves no command was delivered.
    pub fn proves_nothing_delivered(self) -> bool {
        matches!(self, Self::BeforeSend)
    }
}

/// Typed utility-pane start failure carrying the stage that produced it and,
/// for task-owned panes, what happened to the pane afterwards.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PaneStartError {
    pub stage: PaneStartStage,
    pub message: String,
    /// Session registered to keep an uncertain-delivery task-owned pane
    /// inspectable and reclaimable through `herdr_exec_read`/`herdr_exec_kill`.
    pub tracked_session_id: Option<String>,
    /// `Some` only when the pane had to be reclaimed without a session:
    /// `true` when `pane.close` succeeded, `false` when it failed and the
    /// pane may remain orphaned.
    pub pane_reclaimed: Option<bool>,
}

impl PaneStartError {
    pub fn before_send(message: impl Into<String>) -> Self {
        Self {
            stage: PaneStartStage::BeforeSend,
            message: message.into(),
            tracked_session_id: None,
            pane_reclaimed: None,
        }
    }

    pub fn after_send(message: impl Into<String>) -> Self {
        Self {
            stage: PaneStartStage::AfterSend,
            message: message.into(),
            tracked_session_id: None,
            pane_reclaimed: None,
        }
    }

    fn with_tracked_session(mut self, session_id: &str) -> Self {
        self.tracked_session_id = Some(session_id.to_owned());
        self
    }

    pub(crate) fn with_pane_reclaimed(mut self, reclaimed: bool) -> Self {
        self.pane_reclaimed = Some(reclaimed);
        self
    }

    pub(crate) fn with_pane_reclaimed_opt(mut self, reclaimed: Option<bool>) -> Self {
        self.pane_reclaimed = reclaimed;
        self
    }
}

impl std::fmt::Display for PaneStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PaneStartError {}

/// Outcome of the pane-level reclaim step a pane-backed termination performs.
/// produce `Closed`/`Failed`; every other backend reports `NotApplicable`.
#[derive(Debug, Clone, Eq, PartialEq)]
enum PaneReclaimOutcome {
    NotApplicable,
    Closed,
    Failed(String),
}

impl PaneReclaimOutcome {
    /// `Some(true)`/`Some(false)` when a pane-close was attempted; `None` when
    /// the session did not own a pane to close.
    fn reclaimed(&self) -> Option<bool> {
        match self {
            PaneReclaimOutcome::Closed => Some(true),
            PaneReclaimOutcome::Failed(_) => Some(false),
            PaneReclaimOutcome::NotApplicable => None,
        }
    }
}

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
    /// True only after BOTH durable pieces of close evidence committed: the
    /// SQLite `exec_sessions` row and the `.spool` output artifact. The
    /// resident-session cap never evicts a closed session while this is
    /// false, because eviction would strand `running=false` with no readable
    /// output. Sessions recovered from a durable row alone (spool missing or
    /// corrupt) keep this false forever; their output is genuinely lost, not
    /// merely unloaded.
    evidence_persisted: bool,
    /// The bounded spool/SQLite retries are exhausted. The resident cap may
    /// evict this closed session; a later read rebuilds it from the durable
    /// row as incomplete evidence instead of pinning the failed close forever.
    persist_abandoned: bool,
    /// `Some(true)`/`Some(false)` once a close-on-complete pane's reclaim was
    /// attempted; `None` when no reclaim was attempted (still open or not
    /// pane-owned).
    pane_reclaimed: Option<bool>,
    pane_reclaim_error: Option<String>,
}

#[derive(Debug)]
enum SessionBackend {
    Native {
        child: Mutex<Child>,
        output_readers: AtomicUsize,
    },
    Pane {
        client: HerdrClient,
        pane_id: String,
        script_path: PathBuf,
        spool: PaneSpoolPaths,
        stdout_offset: Mutex<u64>,
        stderr_offset: Mutex<u64>,
        close_on_complete: bool,
    },
    Completed,
}

#[derive(Debug, Clone)]
struct PaneSpoolPaths {
    stdout: PathBuf,
    stderr: PathBuf,
    status: PathBuf,
    status_tmp: PathBuf,
}

#[derive(Debug)]
struct Session {
    id: String,
    cwd: PathBuf,
    command: String,
    started_at_ms: u64,
    pid: Option<u32>,
    backend: SessionBackend,
    buffers: Mutex<Buffers>,
    status: Mutex<SessionStatus>,
    /// Set when a durable `exec_sessions` row is loaded but its `.spool`
    /// output artifact is absent or corrupt. Such a session must never be
    /// reported as `completed` with empty output: reads refuse with
    /// `session_evidence_incomplete` instead. A present but unreadable spool
    /// uses `session_spool_corrupt` so it is not confused with a missing
    /// artifact or with a committed spool that really captured zero bytes.
    evidence_missing: bool,
    spool_corrupt: bool,
}

#[derive(Debug)]
struct RegistryInner {
    state_dir: PathBuf,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    recovered: Mutex<HashMap<String, String>>,
    state_store: Mutex<StateStore>,
    client: Option<HerdrClient>,
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

const RECOVERY_SPOOL_CAP: usize = 64 * 1024;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SavedCompletionEvidence {
    version: u32,
    session_id: String,
    stdout_bytes: usize,
    stderr_bytes: usize,
    truncated: bool,
    stdout_tail: String,
    stderr_tail: String,
}

fn exec_spool_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("exec-spools")
}

fn exec_spool_path(state_dir: &Path, session_id: &str) -> PathBuf {
    exec_spool_dir(state_dir).join(format!("{}.spool", marker_safe_id(session_id)))
}

fn extract_stream_tail(buffers: &Buffers, stream: StreamKind, max_bytes: usize) -> Vec<u8> {
    let mut chunks_data = Vec::new();
    let mut total_len = 0usize;
    for chunk in buffers.chunks.iter().rev() {
        if chunk.stream == stream {
            chunks_data.push(&chunk.data[..]);
            total_len = total_len.saturating_add(chunk.data.len());
            if total_len >= max_bytes {
                break;
            }
        }
    }
    chunks_data.reverse();
    let mut combined = Vec::with_capacity(total_len.min(max_bytes));
    for slice in chunks_data {
        combined.extend_from_slice(slice);
    }
    if combined.len() > max_bytes {
        let skip = combined.len() - max_bytes;
        combined[skip..].to_vec()
    } else {
        combined
    }
}

fn parse_summary_count(line: &str, labels: &[&str]) -> Option<u64> {
    let trimmed = line.trim();
    labels.iter().find_map(|label| {
        trimmed
            .strip_prefix(label)
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
    })
}

fn release_gate_phase(command: &str, output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .find_map(|line| {
            line.trim()
                .strip_prefix("[release-gate] PASS phase=")
                .map(str::to_owned)
        })
        .or_else(|| {
            let marker = "scripts/release-gate.sh";
            let suffix = command.split_once(marker)?.1.trim();
            Some(
                suffix
                    .split_whitespace()
                    .next()
                    .filter(|value| matches!(*value, "rust" | "node" | "hygiene" | "full"))
                    .unwrap_or("full")
                    .to_owned(),
            )
        })
}

fn completion_evidence(session: &Session, status: &SessionStatus) -> Value {
    let (output, bytes_total) = session
        .buffers
        .lock()
        .map(|buffers| {
            let mut ordered = Vec::with_capacity(buffers.stdout_bytes + buffers.stderr_bytes);
            for chunk in &buffers.chunks {
                ordered.extend_from_slice(&chunk.data);
            }
            (
                String::from_utf8_lossy(&ordered).into_owned(),
                buffers.stdout_bytes.saturating_add(buffers.stderr_bytes),
            )
        })
        .unwrap_or_default();

    let passed = output
        .lines()
        .filter_map(|line| parse_summary_count(line, &["# pass ", "ℹ pass "]))
        .sum::<u64>();
    let failed = output
        .lines()
        .filter_map(|line| parse_summary_count(line, &["# fail ", "ℹ fail "]))
        .sum::<u64>();
    let tests = output
        .lines()
        .filter_map(|line| parse_summary_count(line, &["# tests ", "ℹ tests "]))
        .sum::<u64>();
    let first_failure = if status.exit_code == Some(0) {
        None
    } else {
        output.lines().find(|line| {
            let line = line.trim();
            line.contains("FAILED")
                || line.starts_with("not ok")
                || line.contains("[release-gate] ERROR:")
        })
    };

    let mut result = json!({
        "phase": "completed",
        "exit_code": status.exit_code,
        "full_log_available": bytes_total > 0,
        "log_session_id": session.id,
    });
    if let Some(object) = result.as_object_mut() {
        if let Some(gate_phase) = release_gate_phase(&session.command, &output) {
            object.insert("gate_phase".to_owned(), json!(gate_phase));
        }
        if tests > 0 || passed > 0 || failed > 0 {
            object.insert(
                "tests".to_owned(),
                json!({"total": tests, "passed": passed, "failed": failed}),
            );
        }
        if let Some(first_failure) = first_failure {
            object.insert("first_failure".to_owned(), json!(first_failure.trim()));
        }
    }
    result
}

fn write_session_spool(state_dir: &Path, session: &Session) -> Result<(), String> {
    let spool_dir = exec_spool_dir(state_dir);
    fs::create_dir_all(&spool_dir).map_err(|error| {
        format!(
            "cannot create exec spool directory {}: {error}",
            spool_dir.display()
        )
    })?;
    #[cfg(unix)]
    let _ = fs::set_permissions(&spool_dir, fs::Permissions::from_mode(0o700));

    let path = exec_spool_path(state_dir, &session.id);
    let (stdout_tail, stderr_tail, stdout_bytes, stderr_bytes, truncated) = {
        let Ok(buffers) = session.buffers.lock() else {
            return Err("session buffers lock poisoned".to_owned());
        };
        let stdout_data = extract_stream_tail(&buffers, StreamKind::Stdout, RECOVERY_SPOOL_CAP);
        let stderr_data = extract_stream_tail(&buffers, StreamKind::Stderr, RECOVERY_SPOOL_CAP);
        let truncated = buffers.truncated
            || buffers.stdout_bytes > RECOVERY_SPOOL_CAP
            || buffers.stderr_bytes > RECOVERY_SPOOL_CAP;
        (
            BASE64.encode(&stdout_data),
            BASE64.encode(&stderr_data),
            buffers.stdout_bytes,
            buffers.stderr_bytes,
            truncated,
        )
    };

    let artifact = SavedCompletionEvidence {
        version: 1,
        session_id: session.id.clone(),
        stdout_bytes,
        stderr_bytes,
        truncated,
        stdout_tail,
        stderr_tail,
    };

    let payload = serde_json::to_vec(&artifact)
        .map_err(|error| format!("cannot serialize session artifact {}: {error}", session.id))?;

    let tmp_path = spool_dir.join(format!("{}.tmp", marker_safe_id(&session.id)));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);

    let mut file = options
        .open(&tmp_path)
        .map_err(|error| format!("cannot open tmp spool file {}: {error}", tmp_path.display()))?;
    file.write_all(&payload)
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            format!(
                "cannot write tmp spool file {}: {error}",
                tmp_path.display()
            )
        })?;
    #[cfg(unix)]
    let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
    drop(file);

    fs::rename(&tmp_path, &path).map_err(|error| {
        format!(
            "cannot atomically commit spool file {}: {error}",
            path.display()
        )
    })?;

    Ok(())
}

fn load_session_from_spool_or_record(
    state_dir: &Path,
    record: &ClosedExecSessionRecord,
) -> Arc<Session> {
    let spool_path = exec_spool_path(state_dir, &record.session_id);
    let mut chunks = Vec::new();
    let mut stdout_bytes = 0;
    let mut stderr_bytes = 0;
    let mut truncated = false;
    let mut evidence_loaded = false;
    let mut spool_corrupt = false;

    match fs::read(&spool_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => spool_corrupt = true,
        Ok(bytes) => match serde_json::from_slice::<SavedCompletionEvidence>(&bytes) {
            Ok(artifact) if artifact.version == 1 && artifact.session_id == record.session_id => {
                match decode_spool_stream(&artifact.stdout_tail, artifact.stdout_bytes).and_then(
                    |stdout_data| {
                        decode_spool_stream(&artifact.stderr_tail, artifact.stderr_bytes)
                            .map(|stderr_data| (stdout_data, stderr_data))
                    },
                ) {
                    Ok((stdout_data, stderr_data)) => {
                        let stdout_matches = spool_stream_length_matches(
                            artifact.stdout_bytes,
                            stdout_data.len(),
                            artifact.truncated,
                        );
                        let stderr_matches = spool_stream_length_matches(
                            artifact.stderr_bytes,
                            stderr_data.len(),
                            artifact.truncated,
                        );
                        stdout_bytes = stdout_data.len();
                        stderr_bytes = stderr_data.len();
                        let mut next_seq = 0u64;
                        if !stdout_data.is_empty() {
                            chunks.push(Chunk {
                                seq: next_seq,
                                stream: StreamKind::Stdout,
                                data: stdout_data,
                            });
                            next_seq = next_seq.saturating_add(1);
                        }
                        if !stderr_data.is_empty() {
                            chunks.push(Chunk {
                                seq: next_seq,
                                stream: StreamKind::Stderr,
                                data: stderr_data,
                            });
                        }
                        truncated = artifact.truncated;
                        // Each stream keeps its own length invariant. A
                        // truncation on stderr does not excuse a stdout tail
                        // that is shorter than its declared count.
                        if !stdout_matches || !stderr_matches {
                            chunks.clear();
                            stdout_bytes = 0;
                            stderr_bytes = 0;
                            spool_corrupt = true;
                        } else {
                            evidence_loaded = true;
                        }
                    }
                    Err(()) => spool_corrupt = true,
                }
            }
            _ => spool_corrupt = true,
        },
    }

    let next_seq = chunks.last().map_or(0, |c| c.seq.saturating_add(1));
    let buffers = Buffers {
        chunks,
        next_seq,
        stdout_bytes,
        stderr_bytes,
        truncated,
    };
    let status = SessionStatus {
        closed: true,
        exit_code: record.exit_code,
        signal: record.signal.clone(),
        ended_at_ms: record.ended_at_ms,
        // A row-only recovery has no durable output anywhere; it must not be
        // reported as a completed session with empty output.
        evidence_persisted: evidence_loaded,
        persist_abandoned: false,
        pane_reclaimed: None,
        pane_reclaim_error: None,
    };
    Arc::new(Session {
        id: record.session_id.clone(),
        cwd: PathBuf::new(),
        command: String::new(),
        started_at_ms: record.started_at_ms,
        pid: None,
        backend: SessionBackend::Completed,
        buffers: Mutex::new(buffers),
        status: Mutex::new(status),
        evidence_missing: !evidence_loaded,
        spool_corrupt,
    })
}

/// Decode one base64 spool tail. A legal JSON document with a broken tail is
/// corrupt evidence, never a successful empty capture. The decoded length must
/// match the declared byte count, or be the bounded tail of a truncated stream.
fn decode_spool_stream(encoded: &str, declared_bytes: usize) -> Result<Vec<u8>, ()> {
    let decoded = BASE64.decode(encoded).map_err(|_| ())?;
    if decoded.len() > RECOVERY_SPOOL_CAP || decoded.len() > declared_bytes {
        return Err(());
    }
    if declared_bytes > RECOVERY_SPOOL_CAP && decoded.len() != RECOVERY_SPOOL_CAP {
        return Err(());
    }
    Ok(decoded)
}

/// A stream under the spool cap must decode to exactly its declared length.
/// Only a stream that itself exceeds the cap may be the shorter retained tail,
/// and that tail is exactly one cap. The other stream is checked on its own.
fn spool_stream_length_matches(declared_bytes: usize, decoded_len: usize, truncated: bool) -> bool {
    if declared_bytes > RECOVERY_SPOOL_CAP {
        truncated && decoded_len == RECOVERY_SPOOL_CAP
    } else {
        decoded_len == declared_bytes
    }
}

fn clean_expired_spools(state_dir: &Path, unexpired_session_ids: &HashSet<String>) {
    let spool_dir = exec_spool_dir(state_dir);
    if !spool_dir.exists() {
        return;
    }
    let unexpired_safe_ids = unexpired_session_ids
        .iter()
        .map(|id| marker_safe_id(id))
        .collect::<HashSet<_>>();
    if let Ok(entries) = fs::read_dir(&spool_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("spool") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                    && !unexpired_safe_ids.contains(stem)
                {
                    let _ = fs::remove_file(&path);
                }
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("tmp") {
                // #1010 1b: a young `.tmp` is an in-flight atomic commit by a
                // concurrent session close; deleting it turns the writer's
                // rename into an ENOENT persistence failure. Only stale
                // leftovers past the grace window are removed.
                let stale = fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|mtime| mtime.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_millis(SPOOL_TMP_GRACE_MS));
                if stale {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExecRegistry {
    inner: Arc<RegistryInner>,
}

impl ExecRegistry {
    #[cfg(test)]
    pub fn new(state_dir: PathBuf) -> Result<Self, String> {
        Self::new_with_client(state_dir, None)
    }

    pub fn new_with_client(
        state_dir: PathBuf,
        client: Option<HerdrClient>,
    ) -> Result<Self, String> {
        fs::create_dir_all(&state_dir)
            .map_err(|error| format!("cannot create exec state directory: {error}"))?;
        #[cfg(unix)]
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure exec state directory: {error}"))?;
        let spool_dir = exec_spool_dir(&state_dir);
        fs::create_dir_all(&spool_dir)
            .map_err(|error| format!("cannot create exec spool directory: {error}"))?;
        #[cfg(unix)]
        let _ = fs::set_permissions(&spool_dir, fs::Permissions::from_mode(0o700));

        let state_store = StateStore::open_in_dir(&state_dir, "state.db")?;
        let now = now_ms();
        bound_completed_pidless_sessions(&state_store, &state_dir, now)?;
        state_store.prune_exec_sessions(now)?;
        let recovery = recover_state_store(&state_store)?;

        let unexpired_ids = state_store
            .unexpired_exec_session_ids(now)
            .unwrap_or_default();
        clean_expired_spools(&state_dir, &unexpired_ids);

        let closed_records = state_store
            .closed_exec_sessions(now, RECOVERY_MAX_ENTRIES)
            .unwrap_or_default();

        let mut initial_sessions = HashMap::new();
        for record in &closed_records {
            let session = load_session_from_spool_or_record(&state_dir, record);
            // A row-only recovery (`.spool` missing/corrupt) stays out of the
            // resident map: `session()` rebuilds it on demand, so a spool that
            // commits late is picked up instead of being shadowed by a stale
            // empty projection.
            if !session.evidence_missing {
                initial_sessions.insert(record.session_id.clone(), session);
            }
        }

        Ok(Self {
            inner: Arc::new(RegistryInner {
                state_dir,
                sessions: Mutex::new(initial_sessions),
                recovered: Mutex::new(recovery.states),
                state_store: Mutex::new(state_store),
                client,
                persistence_failures: AtomicUsize::new(0),
                reaped_on_boot: recovery.reaped,
                detached_on_boot: recovery.detached,
                closed_on_boot: recovery.closed,
            }),
        })
    }

    #[cfg(test)]
    pub fn start(&self, cwd: &Path, command: &str) -> Result<Value, String> {
        self.start_native(cwd, command)
    }

    pub(crate) fn start_native(&self, cwd: &Path, command: &str) -> Result<Value, String> {
        if command.is_empty() {
            return Err("command must not be empty".to_owned());
        }
        self.start_native_process(cwd, command, |id| shell_command(command, id))
    }

    pub(crate) fn start_native_program(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
    ) -> Result<Value, String> {
        if program.is_empty() {
            return Err("program must not be empty".to_owned());
        }
        let rendered = render_exec_argv(program, args);
        self.start_native_process(cwd, &rendered, |_| {
            let mut process = Command::new(program);
            process.args(args);
            process
        })
    }

    fn start_native_process<F>(
        &self,
        cwd: &Path,
        command: &str,
        build_process: F,
    ) -> Result<Value, String>
    where
        F: FnOnce(&str) -> Command,
    {
        self.prune();
        let id = new_session_id();
        let mut process = build_process(&id);
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
            pid: Some(pid),
            backend: SessionBackend::Native {
                child: Mutex::new(child),
                output_readers: AtomicUsize::new(output_readers),
            },
            buffers: Mutex::new(Buffers::default()),
            status: Mutex::new(SessionStatus::default()),
            evidence_missing: false,
            spool_corrupt: false,
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
        // The spawned command may mutate the working tree under `cwd` before
        // any completion event; bound cached git-status staleness from start.
        crate::projects::invalidate_git_status_cache_for_path(cwd);
        Ok(json!({
            "ok": true,
            "session_id": id,
            "cwd": cwd.to_string_lossy(),
            "command": command,
            "started_at": iso_from_ms(session.started_at_ms),
            "pid": pid,
            "backend": "native",
            "phase": "started",
            "progress": {
                "bytes_read": 0,
                "bytes_total": 0,
                "elapsed_ms": 0,
            },
        }))
    }

    /// Start one command in an existing canonical utility pane.
    pub fn start_in_existing_pane(
        &self,
        cwd: &Path,
        command: &str,
        pane_id: &str,
    ) -> Result<Value, PaneStartError> {
        self.start_in_pane(cwd, command, pane_id, false, "utility_pane")
    }

    /// Start one command in a task-owned pane that is automatically reclaimed
    /// when the durable session completes or is killed.
    pub fn start_in_private_pane(
        &self,
        cwd: &Path,
        command: &str,
        pane_id: &str,
    ) -> Result<Value, PaneStartError> {
        self.start_in_pane(cwd, command, pane_id, true, "private_pane")
    }

    /// Failures are typed by the stage that produced them, because only the
    /// stage decides what a caller may claim: a failure before `pane.send_text`
    /// proves nothing was delivered, while a failure at or after that call
    /// cannot prove either way.
    fn start_in_pane(
        &self,
        cwd: &Path,
        command: &str,
        pane_id: &str,
        close_on_complete: bool,
        backend: &str,
    ) -> Result<Value, PaneStartError> {
        self.prune();
        if command.is_empty() {
            return Err(PaneStartError::before_send("command must not be empty"));
        }
        let client = self
            .inner
            .client
            .clone()
            .ok_or_else(|| PaneStartError::before_send("Herdr pane backend is unavailable"))?;
        let id = new_session_id();
        let script_path = pane_script_path(&id);
        let spool = pane_spool_paths(&id);
        write_pane_script(&script_path, cwd, command, &spool)
            .map_err(PaneStartError::before_send)?;
        let session = Arc::new(Session {
            id: id.clone(),
            cwd: cwd.to_path_buf(),
            command: command.to_owned(),
            started_at_ms: now_ms(),
            pid: None,
            backend: SessionBackend::Pane {
                client: client.clone(),
                pane_id: pane_id.to_owned(),
                script_path: script_path.clone(),
                spool: spool.clone(),
                stdout_offset: Mutex::new(0),
                stderr_offset: Mutex::new(0),
                close_on_complete,
            },
            buffers: Mutex::new(Buffers::default()),
            status: Mutex::new(SessionStatus::default()),
            evidence_missing: false,
            spool_corrupt: false,
        });
        let launch_line = format!(
            "{} {}",
            shell_quote(resolve_exec_shell().to_string_lossy().as_ref()),
            shell_quote(script_path.to_string_lossy().as_ref()),
        );
        if let Err(error) = client.call_with_timeout(
            "pane.send_text",
            json!({"pane_id": pane_id, "text": format!("{launch_line}\n")}),
            PANE_RPC_TIMEOUT,
        ) {
            if close_on_complete {
                // The launch line may already have reached the task-owned
                // pane. Register it as a trackable session so
                // `herdr_exec_read`/`herdr_exec_kill` can inspect or reclaim
                // it; only fall back to pane.close when tracking itself
                // failed.
                if let Err(tracking_error) = self.track_orphan_pane_session(&session) {
                    cleanup_pane_files(&script_path, &spool);
                    let pane_reclaimed = client
                        .call_with_timeout(
                            "pane.close",
                            json!({"pane_id": pane_id}),
                            PANE_RPC_TIMEOUT,
                        )
                        .is_ok();
                    return Err(PaneStartError::after_send(format!(
                        "cannot start pane command: {error}; pane session tracking failed: {tracking_error}"
                    ))
                    .with_pane_reclaimed(pane_reclaimed));
                }
                return Err(PaneStartError::after_send(format!(
                    "cannot start pane command: {error}"
                ))
                .with_tracked_session(&id));
            }
            cleanup_pane_files(&script_path, &spool);
            return Err(PaneStartError::after_send(format!(
                "cannot start pane command: {error}"
            )));
        }
        let started_at_ms = session.started_at_ms;
        if let Err(error) = self
            .inner
            .state_store
            .lock()
            .map_err(|_| "exec state store lock poisoned".to_owned())
            .and_then(|store| store.record_pane_exec_running(&id, started_at_ms))
        {
            let outcome = terminate_session(&session, true, None);
            // The launch line already reached the pane, so the caller must not
            // be told that nothing ran. Report whether the owned pane was
            // actually closed instead of claiming it unconditionally.
            return Err(PaneStartError::after_send(format!(
                "cannot durably register pane exec session; session terminated before return{}: {error}",
                match &outcome {
                    PaneReclaimOutcome::Closed => " and its pane was closed",
                    PaneReclaimOutcome::Failed(_) => {
                        "; the owned pane could not be closed"
                    }
                    PaneReclaimOutcome::NotApplicable => "",
                }
            ))
            .with_pane_reclaimed_opt(outcome.reclaimed()));
        }
        self.inner
            .sessions
            .lock()
            .map_err(|_| {
                // The launch line was already delivered before this registry
                // bookkeeping step.
                let outcome = terminate_session(&session, true, None);
                PaneStartError::after_send("exec registry lock poisoned")
                    .with_pane_reclaimed_opt(outcome.reclaimed())
            })?
            .insert(id.clone(), Arc::clone(&session));
        spawn_monitor(Arc::clone(&session), Arc::downgrade(&self.inner));
        Ok(json!({
            "ok": true,
            "session_id": id,
            "cwd": cwd.to_string_lossy(),
            "command": command,
            "started_at": iso_from_ms(started_at_ms),
            "pid": Value::Null,
            "backend": backend,
            "pane_id": pane_id,
            "phase": "started",
            "progress": {
                "bytes_read": 0,
                "bytes_total": 0,
                "elapsed_ms": 0,
            },
        }))
    }

    /// Register a task-owned pane whose `pane.send_text` failed after bytes may
    /// already have been delivered. The command outcome is unknown, so the
    /// pane keeps running under a real session id: the monitor reaps it when
    /// the launch line completes, and `herdr_exec_kill` reclaims the pane if it
    /// never does. Without this the pane would be unaddressable forever.
    ///
    /// Platform-neutral on purpose: the Windows candidate build also compiles
    /// this start path, so a `cfg(unix)`-only helper breaks that lane.
    fn track_orphan_pane_session(&self, session: &Arc<Session>) -> Result<(), String> {
        self.inner
            .state_store
            .lock()
            .map_err(|_| "exec state store lock poisoned".to_owned())
            .and_then(|store| store.record_pane_exec_running(&session.id, session.started_at_ms))?;
        self.inner
            .sessions
            .lock()
            .map_err(|_| "exec registry lock poisoned".to_owned())?
            .insert(session.id.clone(), Arc::clone(session));
        spawn_monitor(Arc::clone(session), Arc::downgrade(&self.inner));
        Ok(())
    }

    pub fn read(&self, id: &str, stream: &str, offset: usize, limit: usize) -> Value {
        self.prune();
        let Some(session) = self
            .session(id)
            .or_else(|| self.committed_spool_session(id))
        else {
            return self.recovered_or_missing(id);
        };
        let Some(stream_filter) = parse_stream(stream) else {
            return json!({"ok": false, "code": "invalid_params", "message": "stream must be stdout, stderr, or both"});
        };
        refresh_session_status(&session, Some(&self.inner));
        {
            // A durable row whose output is not in this process is missing
            // evidence, not an empty completion. A session that is still
            // resident keeps serving its memory buffers even after persist
            // retries are abandoned: the buffers are the remaining copy until
            // `prune_loaded_closed_sessions` drops the session.
            let status = session.status.lock();
            let closed = status.as_ref().map(|status| status.closed).unwrap_or(true);
            if closed && session.evidence_missing {
                let corrupt = session.spool_corrupt;
                let reason = if corrupt {
                    "session_spool_corrupt"
                } else {
                    "session_evidence_incomplete"
                };
                let message = if corrupt {
                    "the durable exec session row exists but its output spool is unreadable; captured output is unavailable"
                } else {
                    "the durable exec session row exists but its output spool never committed; captured output is unavailable"
                };
                return json!({
                    "ok": false,
                    "reason": reason,
                    "session_id": id,
                    "recovered": true,
                    "message": message,
                });
            }
        }
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
        let is_recovered = matches!(session.backend, SessionBackend::Completed);
        let mut result = Map::new();
        result.insert("ok".to_owned(), json!(true));
        result.insert("session_id".to_owned(), json!(id));
        result.insert("running".to_owned(), json!(!status.closed));
        result.insert("phase".to_owned(), json!(phase));
        result.insert("exit_code".to_owned(), json!(status.exit_code));
        result.insert("signal".to_owned(), json!(status.signal));
        if is_recovered {
            result.insert("recovered".to_owned(), json!(true));
        }
        if session.started_at_ms > 0 {
            result.insert(
                "started_at".to_owned(),
                json!(iso_from_ms(session.started_at_ms)),
            );
        }
        if let Some(ended_at_ms) = status.ended_at_ms {
            result.insert("finished_at".to_owned(), json!(iso_from_ms(ended_at_ms)));
        }
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

        if let Some(pane_reclaimed) = status.pane_reclaimed {
            result.insert("pane_reclaimed".to_owned(), json!(pane_reclaimed));
        }
        if let Some(error) = &status.pane_reclaim_error {
            result.insert("pane_reclaim_error".to_owned(), json!(error));
        }
        if status.closed {
            result.insert(
                "completion".to_owned(),
                completion_evidence(&session, &status),
            );
        }
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
            let mut result = Map::new();
            result.insert("ok".to_owned(), json!(true));
            result.insert("session_id".to_owned(), json!(id));
            result.insert("killed".to_owned(), json!(false));
            result.insert("exit_code".to_owned(), json!(status.exit_code));
            result.insert("signal".to_owned(), json!(status.signal));
            if let Some(pane_reclaimed) = status.pane_reclaimed {
                result.insert("pane_reclaimed".to_owned(), json!(pane_reclaimed));
            }
            if let Some(error) = status.pane_reclaim_error {
                result.insert("pane_reclaim_error".to_owned(), json!(error));
            }
            return Value::Object(result);
        }
        let outcome = terminate_session(&session, false, Some(&self.inner));
        let weak = Arc::downgrade(&session);
        thread::spawn(move || {
            thread::sleep(KILL_GRACE);
            if let Some(session) = weak.upgrade()
                && !session_status(&session).closed
            {
                terminate_session(&session, true, None);
            }
        });
        let mut result = Map::new();
        result.insert("ok".to_owned(), json!(true));
        result.insert("session_id".to_owned(), json!(id));
        result.insert("killed".to_owned(), json!(true));
        result.insert("exit_code".to_owned(), Value::Null);
        result.insert("signal".to_owned(), Value::Null);
        match outcome {
            PaneReclaimOutcome::Closed => {
                result.insert("pane_reclaimed".to_owned(), json!(true));
            }
            PaneReclaimOutcome::Failed(error) => {
                result.insert("pane_reclaimed".to_owned(), json!(false));
                result.insert("pane_reclaim_error".to_owned(), json!(error));
            }
            PaneReclaimOutcome::NotApplicable => {}
        }
        Value::Object(result)
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

    /// Return the newest live exec session that this runtime owns on `pane_id`.
    ///
    /// Utility-pane scheduling uses this as an ownership proof before waiting
    /// for a busy canonical pane. A busy pane with no matching live registry
    /// session is treated as foreign/interactive contention and is never waited
    /// on or interrupted automatically.
    pub fn running_pane_session_id(&self, pane_id: &str) -> Option<String> {
        self.prune();
        let sessions = self.inner.sessions.lock().ok()?;
        sessions
            .values()
            .filter(|session| {
                matches!(
                    &session.backend,
                    SessionBackend::Pane {
                        pane_id: owned_pane,
                        ..
                    } if owned_pane == pane_id
                ) && !session_status(session).closed
            })
            .max_by_key(|session| session.started_at_ms)
            .map(|session| session.id.clone())
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

    /// Cheap liveness snapshot for the HTTP `/health` route.
    ///
    /// Unlike `diagnostics`, this must never perform pruning, filesystem
    /// cleanup, SQLite work, or wait for an exec-session mutex. The health
    /// watchdog uses a short timeout and must distinguish a live-but-busy
    /// runtime from a dead process instead of turning normal maintenance
    /// contention into a restart loop.
    pub fn health_diagnostics(&self) -> Value {
        let (count, running, sessions_busy) = match self.inner.sessions.try_lock() {
            Ok(sessions) => {
                let mut running = 0usize;
                let mut status_busy = false;
                for session in sessions.values() {
                    match session.status.try_lock() {
                        Ok(status) => running += usize::from(!status.closed),
                        Err(_) => status_busy = true,
                    }
                }
                (json!(sessions.len()), json!(running), status_busy)
            }
            Err(_) => (Value::Null, Value::Null, true),
        };
        let (state_store_ready, state_store_busy) = match self.inner.state_store.try_lock() {
            Ok(_) => (true, false),
            Err(std::sync::TryLockError::WouldBlock) => (true, true),
            Err(std::sync::TryLockError::Poisoned(_)) => (false, false),
        };
        json!({
            "ready": true,
            "count": count,
            "running": running,
            "sessions_busy": sessions_busy,
            "reaped_on_boot": self.inner.reaped_on_boot,
            "detached_on_boot": self.inner.detached_on_boot,
            "closed_on_boot": self.inner.closed_on_boot,
            "state_store_ready": state_store_ready,
            "state_store_busy": state_store_busy,
            "state_store_schema": crate::state_store::SCHEMA_VERSION,
            "persistence_failures": self.inner.persistence_failures.load(Ordering::Relaxed),
        })
    }

    fn committed_spool_session(&self, id: &str) -> Option<Arc<Session>> {
        let state = self
            .inner
            .recovered
            .lock()
            .ok()
            .and_then(|recovered| recovered.get(id).cloned())?;
        if !state.starts_with("settle_failed") {
            return None;
        }
        let record = ClosedExecSessionRecord {
            session_id: id.to_owned(),
            started_at_ms: now_ms(),
            ended_at_ms: Some(now_ms()),
            exit_code: None,
            signal: None,
            expires_at_ms: Some(now_ms().saturating_add(SESSION_TTL_MS)),
        };
        let session = load_session_from_spool_or_record(&self.inner.state_dir, &record);
        (!session.evidence_missing && !session.spool_corrupt).then_some(session)
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
        if let Ok(sessions) = self.inner.sessions.lock()
            && let Some(session) = sessions.get(id)
        {
            return Some(Arc::clone(session));
        }
        let record = self.inner.state_store.lock().ok().and_then(|store| {
            store
                .get_closed_exec_session(id)
                .ok()
                .flatten()
                .or_else(|| store.get_abandoned_exec_session(id).ok().flatten())
                .or_else(|| store.get_unsettled_exec_session(id).ok().flatten())
        })?;
        let mut record = record;
        if record.ended_at_ms.is_none() && record.expires_at_ms.is_none() {
            let ended_at = now_ms();
            let expires_at = ended_at.saturating_add(SESSION_TTL_MS);
            if let Ok(store) = self.inner.state_store.lock()
                && store
                    .bound_unsettled_exec_session(id, ended_at, expires_at)
                    .unwrap_or(false)
            {
                record.ended_at_ms = Some(ended_at);
                record.expires_at_ms = Some(expires_at);
            }
        }
        let session = load_session_from_spool_or_record(&self.inner.state_dir, &record);
        // A row whose `.spool` has not committed yet is an in-flight close, a
        // crashed write, or lost output. Do not cache the empty projection:
        // the next read must re-check for the artifact rather than serving a
        // stale empty result after it lands.
        if !session.evidence_missing
            && let Ok(mut sessions) = self.inner.sessions.lock()
        {
            sessions.insert(id.to_owned(), Arc::clone(&session));
        }
        Some(session)
    }

    fn prune(&self) {
        let now = now_ms();
        if let Ok(store) = self.inner.state_store.lock() {
            let _ = bound_completed_pidless_sessions(&store, &self.inner.state_dir, now);
        }
        if let Ok(mut sessions) = self.inner.sessions.lock() {
            sessions.retain(|id, session| {
                let status = session_status(session);
                let keep = status
                    .ended_at_ms
                    .is_none_or(|ended| now.saturating_sub(ended) <= SESSION_TTL_MS);
                if !keep {
                    let spool_path = exec_spool_path(&self.inner.state_dir, id);
                    let _ = fs::remove_file(spool_path);
                }
                keep
            });
            prune_loaded_closed_sessions(&mut sessions, RECOVERY_MAX_ENTRIES);
        }
        if let Ok(store) = self.inner.state_store.lock() {
            if store.prune_exec_sessions(now).is_err() {
                self.inner
                    .persistence_failures
                    .fetch_add(1, Ordering::Relaxed);
            }
            if let Ok(unexpired_ids) = store.unexpired_exec_session_ids(now) {
                clean_expired_spools(&self.inner.state_dir, &unexpired_ids);
            }
        }
    }
}

/// Bind a TTL only for a pid-less running row whose committed spool is valid.
/// A pane that is still active has no such spool and stays unbounded. A native
/// row keeps its pid and is not selected here.
fn bound_completed_pidless_sessions(
    store: &StateStore,
    state_dir: &Path,
    now: u64,
) -> Result<(), String> {
    let expires_at = now.saturating_add(SESSION_TTL_MS);
    for session_id in store.unsettled_pidless_exec_session_ids()? {
        let record = ClosedExecSessionRecord {
            session_id: session_id.clone(),
            started_at_ms: now,
            ended_at_ms: None,
            exit_code: None,
            signal: None,
            expires_at_ms: None,
        };
        let session = load_session_from_spool_or_record(state_dir, &record);
        if session.evidence_missing || session.spool_corrupt {
            continue;
        }
        store.bound_unsettled_exec_session(&session_id, now, expires_at)?;
    }
    Ok(())
}

/// Bound the resident set of completed sessions while leaving every durable
/// spool and `exec_sessions` row untouched, so an evicted session is still
/// readable through `read`/`wait` recovery instead of being lost.
///
/// The keep order deliberately mirrors the `closed_exec_sessions` query
/// (`COALESCE(ended_at, started_at) DESC, started_at DESC, session_id DESC`),
/// so the boot-time load and this steady-state cap agree on which sessions are
/// hot. The oldest entries are dropped first.
fn prune_loaded_closed_sessions(sessions: &mut HashMap<String, Arc<Session>>, max_closed: usize) {
    let mut closed = sessions
        .iter()
        .filter_map(|(id, session)| {
            let status = session_status(session);
            // A closed session whose durable evidence fully committed can
            // leave memory: the spool and row remain. A close whose bounded
            // persist retries are exhausted can leave too, marked incomplete,
            // so permanent failures cannot pin an unbounded resident set. An
            // in-flight close (not yet abandoned) stays: its buffers are still
            // the only copy of the output.
            (status.closed && (status.evidence_persisted || status.persist_abandoned)).then(|| {
                (
                    status.ended_at_ms.unwrap_or(session.started_at_ms),
                    session.started_at_ms,
                    id.clone(),
                )
            })
        })
        .collect::<Vec<_>>();
    if closed.len() <= max_closed {
        return;
    }
    closed.sort_unstable();
    let remove_count = closed.len() - max_closed;
    for (_, _, id) in closed.drain(..remove_count) {
        let _ = sessions.remove(&id);
    }
}

fn session_view(session: &Arc<Session>) -> Value {
    let status = session_status(session);
    let truncated = session
        .buffers
        .lock()
        .map(|buffers| buffers.truncated)
        .unwrap_or(false);
    let is_recovered = matches!(session.backend, SessionBackend::Completed);
    let command = if session.command.is_empty() {
        Value::Null
    } else if session.command.chars().count() > 200 {
        json!(format!(
            "{}…",
            session.command.chars().take(200).collect::<String>()
        ))
    } else {
        json!(session.command)
    };
    let cwd = if session.cwd.as_os_str().is_empty() {
        Value::Null
    } else {
        json!(session.cwd.to_string_lossy())
    };
    let mut view = json!({
        "session_id": session.id,
        "cwd": cwd,
        "command": command,
        "started_at": iso_from_ms(session.started_at_ms),
        "running": !status.closed,
        "exit_code": status.exit_code,
        "signal": status.signal,
        "truncated": truncated,
    });
    if is_recovered && let Some(obj) = view.as_object_mut() {
        obj.insert("recovered".to_owned(), json!(true));
        if session.evidence_missing {
            obj.insert("evidence_incomplete".to_owned(), json!(true));
        }
    }
    view
}

fn session_status(session: &Arc<Session>) -> SessionStatus {
    session.status.lock().map_or_else(
        |_| SessionStatus {
            closed: true,
            exit_code: None,
            signal: None,
            ended_at_ms: Some(now_ms()),
            // Poisoned status: keep the session resident rather than risking
            // eviction of evidence we cannot inspect.
            evidence_persisted: false,
            persist_abandoned: false,
            pane_reclaimed: None,
            pane_reclaim_error: None,
        },
        |status| SessionStatus {
            closed: status.closed,
            exit_code: status.exit_code,
            signal: status.signal.clone(),
            ended_at_ms: status.ended_at_ms,
            evidence_persisted: status.evidence_persisted,
            persist_abandoned: status.persist_abandoned,
            pane_reclaimed: status.pane_reclaimed,
            pane_reclaim_error: status.pane_reclaim_error.clone(),
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
        if let SessionBackend::Native { output_readers, .. } = &session.backend {
            output_readers.fetch_sub(1, Ordering::Release);
        }
    });
}

fn wait_for_output_readers(session: &Arc<Session>) -> bool {
    let SessionBackend::Native { output_readers, .. } = &session.backend else {
        return true;
    };
    let deadline = Instant::now() + OUTPUT_DRAIN_BUDGET;
    while output_readers.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    output_readers.load(Ordering::Acquire) == 0
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

fn marker_safe_id(id: &str) -> String {
    id.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn pane_script_path(id: &str) -> PathBuf {
    env::temp_dir().join(format!("herdr-mcp-pane-exec-{}.sh", marker_safe_id(id)))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn render_exec_argv(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

fn pane_spool_paths(id: &str) -> PaneSpoolPaths {
    let base = env::temp_dir().join(format!("herdr-mcp-pane-exec-{}", marker_safe_id(id)));
    PaneSpoolPaths {
        stdout: base.with_extension("stdout"),
        stderr: base.with_extension("stderr"),
        status: base.with_extension("status"),
        status_tmp: base.with_extension("status.tmp"),
    }
}

fn write_pane_script(
    path: &Path,
    cwd: &Path,
    command: &str,
    spool: &PaneSpoolPaths,
) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o700);
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot create pane exec script {}: {error}", path.display()))?;
    let body = format!(
        "#!{}\nset +e\numask 077\nexport PAGER=cat\nexport GIT_PAGER=cat\nexport GH_PAGER=cat\nexport SYSTEMD_PAGER=cat\nexport MANPAGER=cat\nexport DELTA_PAGER=cat\ntrap 'rm -f -- \"$0\"' EXIT\n: > {}\n: > {}\nrm -f -- {} {}\ncd -- {}\ncd_ec=$?\nif [ \"$cd_ec\" -ne 0 ]; then printf '%s\\n' \"$cd_ec\" > {}; mv -f -- {} {}; exit \"$cd_ec\"; fi\n(\n{}\n) > {} 2> {}\nec=$?\nprintf '%s\\n' \"$ec\" > {}\nmv -f -- {} {}\nexit \"$ec\"\n",
        resolve_exec_shell().display(),
        shell_quote(spool.stdout.to_string_lossy().as_ref()),
        shell_quote(spool.stderr.to_string_lossy().as_ref()),
        shell_quote(spool.status.to_string_lossy().as_ref()),
        shell_quote(spool.status_tmp.to_string_lossy().as_ref()),
        shell_quote(cwd.to_string_lossy().as_ref()),
        shell_quote(spool.status_tmp.to_string_lossy().as_ref()),
        shell_quote(spool.status_tmp.to_string_lossy().as_ref()),
        shell_quote(spool.status.to_string_lossy().as_ref()),
        command,
        shell_quote(spool.stdout.to_string_lossy().as_ref()),
        shell_quote(spool.stderr.to_string_lossy().as_ref()),
        shell_quote(spool.status_tmp.to_string_lossy().as_ref()),
        shell_quote(spool.status_tmp.to_string_lossy().as_ref()),
        shell_quote(spool.status.to_string_lossy().as_ref()),
    );
    file.write_all(body.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot write pane exec script {}: {error}", path.display()))?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure pane exec script {}: {error}", path.display()))?;
    Ok(())
}

fn drain_spool_file(session: &Arc<Session>, path: &Path, stream: StreamKind, offset: &Mutex<u64>) {
    let Ok(mut offset) = offset.lock() else {
        return;
    };
    let Ok(mut file) = fs::File::open(path) else {
        return;
    };
    let Ok(metadata) = file.metadata() else {
        return;
    };
    let end = metadata.len();
    if end <= *offset {
        return;
    }
    let room = session
        .buffers
        .lock()
        .map(|buffers| match stream {
            StreamKind::Stdout => MAX_BUFFER_PER_STREAM.saturating_sub(buffers.stdout_bytes),
            StreamKind::Stderr => MAX_BUFFER_PER_STREAM.saturating_sub(buffers.stderr_bytes),
        })
        .unwrap_or(0);
    let available = end.saturating_sub(*offset);
    let take = available.min(room as u64) as usize;
    if take > 0 && file.seek(SeekFrom::Start(*offset)).is_ok() {
        let mut bytes = vec![0_u8; take];
        if file.read_exact(&mut bytes).is_ok() {
            push_chunk(session, stream, &bytes);
        }
    }
    if available > room as u64
        && let Ok(mut buffers) = session.buffers.lock()
    {
        buffers.truncated = true;
    }
    *offset = end;
}

fn read_pane_exit_code(path: &Path) -> Option<i32> {
    let raw = fs::read_to_string(path).ok()?;
    raw.trim().parse::<i32>().ok()
}

fn cleanup_pane_files(script_path: &Path, spool: &PaneSpoolPaths) {
    for path in [
        script_path,
        spool.stdout.as_path(),
        spool.stderr.as_path(),
        spool.status.as_path(),
        spool.status_tmp.as_path(),
    ] {
        let _ = fs::remove_file(path);
    }
}

fn refresh_pane_session(session: &Arc<Session>, registry: Option<&RegistryInner>) -> bool {
    let SessionBackend::Pane {
        client,
        pane_id,
        script_path,
        spool,
        stdout_offset,
        stderr_offset,
        close_on_complete,
    } = &session.backend
    else {
        return false;
    };
    drain_spool_file(session, &spool.stdout, StreamKind::Stdout, stdout_offset);
    drain_spool_file(session, &spool.stderr, StreamKind::Stderr, stderr_offset);
    let Some(exit_code) = read_pane_exit_code(&spool.status) else {
        // A task-owned pane whose launch never produced an exit marker would
        // poll forever; bound the session so the monitor eventually reclaims
        // the pane instead of keeping an unaddressable pane open.
        if *close_on_complete && now_ms().saturating_sub(session.started_at_ms) > SESSION_TTL_MS {
            terminate_session(session, false, registry);
            return true;
        }
        return false;
    };
    let transitioned = complete_session(session, registry, Some(exit_code), None);
    if transitioned {
        cleanup_pane_files(script_path, spool);
        if *close_on_complete {
            let outcome = match client.call_with_timeout(
                "pane.close",
                json!({"pane_id": pane_id}),
                PANE_RPC_TIMEOUT,
            ) {
                Ok(_) => PaneReclaimOutcome::Closed,
                Err(error) => PaneReclaimOutcome::Failed(error.message),
            };
            record_pane_reclaim(session, &outcome);
        }
    }
    true
}

fn refresh_session_status(session: &Arc<Session>, registry: Option<&RegistryInner>) -> bool {
    if session_status(session).closed {
        return true;
    }
    match &session.backend {
        SessionBackend::Completed => true,
        SessionBackend::Native { child, .. } => {
            let result = child
                .lock()
                .map_err(|_| "child lock poisoned".to_owned())
                .and_then(|mut child| child.try_wait().map_err(|error| error.to_string()));
            match result {
                Ok(Some(exit)) => {
                    if !wait_for_output_readers(session) {
                        return false;
                    }
                    let signal = exit_signal(&exit);
                    complete_session(session, registry, exit.code(), signal.as_deref());
                    true
                }
                Ok(None) => false,
                Err(_) => {
                    complete_session(session, registry, None, None);
                    true
                }
            }
        }
        SessionBackend::Pane { .. } => refresh_pane_session(session, registry),
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
    // Keep the marked shell as the process-group leader for the full request.
    // A bare `shell -lc "sleep 30"` may exec its final command, replacing argv[0]
    // and making restart fencing unable to prove process ownership.
    let wrapped = format!("{command}\n__herdr_mcp_exec_status=$?\nexit $__herdr_mcp_exec_status");
    let mut process = Command::new(&shell);
    process.arg0(&marker).args(["-lc", &wrapped]);
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

fn record_pane_reclaim(session: &Arc<Session>, outcome: &PaneReclaimOutcome) {
    let (reclaimed, error) = match outcome {
        PaneReclaimOutcome::Closed => (Some(true), None),
        PaneReclaimOutcome::Failed(error) => (Some(false), Some(error.clone())),
        PaneReclaimOutcome::NotApplicable => (None, None),
    };
    if reclaimed.is_none() {
        return;
    }
    if let Ok(mut status) = session.status.lock() {
        status.pane_reclaimed = reclaimed;
        status.pane_reclaim_error = error;
    }
}

/// Terminate one session and, for close-on-complete pane sessions, reclaim the
/// task-owned pane. The pane-level outcome is returned and recorded on the
/// session status: a failed `pane.close` is never folded into the session
/// close, because the session and the pane it ran in are separate resources.
fn terminate_session(
    session: &Arc<Session>,
    force: bool,
    registry: Option<&RegistryInner>,
) -> PaneReclaimOutcome {
    match &session.backend {
        SessionBackend::Completed => return PaneReclaimOutcome::NotApplicable,
        SessionBackend::Native { child, .. } => {
            #[cfg(unix)]
            {
                let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
                if let Some(pid) = session.pid {
                    let group = -(pid as i32);
                    let delivered = unsafe { libc::kill(group, signal) } == 0;
                    if !delivered && let Ok(mut child) = child.lock() {
                        let _ = child.kill();
                    }
                }
            }
            #[cfg(windows)]
            {
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                }
            }
            return PaneReclaimOutcome::NotApplicable;
        }
        SessionBackend::Pane { .. } => {}
    }
    let SessionBackend::Pane {
        client,
        pane_id,
        script_path,
        spool,
        close_on_complete,
        ..
    } = &session.backend
    else {
        return PaneReclaimOutcome::NotApplicable;
    };
    let outcome = if *close_on_complete {
        match client.call_with_timeout("pane.close", json!({"pane_id": pane_id}), PANE_RPC_TIMEOUT)
        {
            Ok(_) => PaneReclaimOutcome::Closed,
            Err(error) => PaneReclaimOutcome::Failed(error.message),
        }
    } else {
        // Shared utility pane: interrupt the command but keep the pane
        // alive; it is not owned by this exec session.
        let _ = client.call_with_timeout(
            "pane.send_keys",
            json!({"pane_id": pane_id, "keys": ["C-c"]}),
            PANE_RPC_TIMEOUT,
        );
        PaneReclaimOutcome::NotApplicable
    };
    cleanup_pane_files(script_path, spool);
    let signal = if *close_on_complete {
        if force { "SIGKILL" } else { "SIGTERM" }
    } else {
        "SIGINT"
    };
    complete_session(session, registry, None, Some(signal));
    record_pane_reclaim(session, &outcome);
    outcome
}

fn terminate_and_wait(session: &Arc<Session>) {
    terminate_session(session, true, None);
    if let SessionBackend::Native { child, .. } = &session.backend
        && let Ok(mut child) = child.lock()
    {
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

/// #1010 1b: a persistence failure string worth one bounded retry: SQLite
/// lock contention or a transient filesystem error. Everything else (schema
/// mismatches, permission denials, missing durable rows) is permanent.
fn is_transient_persistence_error(error: &str) -> bool {
    let lowered = error.to_ascii_lowercase();
    lowered.contains("locked")
        || lowered.contains("busy")
        || lowered.contains("temporar")
        || lowered.contains("interrupt")
        || lowered.contains("would block")
        || lowered.contains("resource temporarily unavailable")
}

/// #1010 1b: retry the durable settle for transient failures only. The state
/// store mutex is taken per attempt and released between attempts, so one
/// slow close never serializes the rest of the registry behind a retry sleep.
fn settle_closed_evidence(
    inner: &RegistryInner,
    session: &Arc<Session>,
    ended_at: u64,
    exit_code: Option<i32>,
    signal: Option<&str>,
) -> Result<(), String> {
    let mut attempt = 0u32;
    loop {
        let result = inner
            .state_store
            .lock()
            .map_err(|_| "exec state store lock poisoned".to_owned())
            .and_then(|store| {
                store.settle_exec_session(
                    &session.id,
                    "closed",
                    Some(ended_at),
                    exit_code,
                    signal,
                    ended_at.saturating_add(SESSION_TTL_MS),
                )
            });
        match result {
            Ok(()) => return Ok(()),
            Err(error)
                if attempt + 1 < PERSIST_RETRY_ATTEMPTS
                    && is_transient_persistence_error(&error) =>
            {
                attempt += 1;
                thread::sleep(Duration::from_millis(
                    PERSIST_RETRY_BACKOFF_MS * u64::from(attempt),
                ));
            }
            Err(error) => return Err(error),
        }
    }
}

/// #1010 1b: same bounded retry for the disk spool commit. Skipped entirely
/// when the spool already exists, so a duplicated completion path never pays
/// for a second identical write.
fn persist_session_spool(state_dir: &Path, session: &Arc<Session>) -> Result<(), String> {
    if exec_spool_path(state_dir, &session.id).exists() {
        return Ok(());
    }
    let mut attempt = 0u32;
    loop {
        match write_session_spool(state_dir, session) {
            Ok(()) => return Ok(()),
            Err(error)
                if attempt + 1 < PERSIST_RETRY_ATTEMPTS
                    && is_transient_persistence_error(&error) =>
            {
                attempt += 1;
                thread::sleep(Duration::from_millis(
                    PERSIST_RETRY_BACKOFF_MS * u64::from(attempt),
                ));
            }
            Err(error) => return Err(error),
        }
    }
}

fn persist_closed_evidence(
    inner: &RegistryInner,
    session: &Arc<Session>,
    ended_at: u64,
    exit_code: Option<i32>,
    signal: Option<&str>,
) {
    // Runs outside `session.status`: callers mark the transition inside the
    // lock, then persist here so the close path never holds a session mutex
    // across SQLite or spool I/O.
    //
    // Order matters for crash windows. The `.spool` artifact is committed
    // FIRST and the SQLite settle SECOND: a crash between the two leaves a
    // `running` row whose restart recovery settles it as
    // `closed_before_restart` — the spool survives as orphaned evidence and no
    // reader can observe a `closed` row whose output is missing. The old
    // settle-then-spool order could leave exactly that state.
    let spool_result = persist_session_spool(&inner.state_dir, session);
    if spool_result.is_err() {
        inner.persistence_failures.fetch_add(1, Ordering::Relaxed);
    }
    let settle_result = settle_closed_evidence(inner, session, ended_at, exit_code, signal);
    let mut durable_degraded = false;
    if settle_result.is_err() {
        inner.persistence_failures.fetch_add(1, Ordering::Relaxed);
        // The row would otherwise stay `running` with a null `expires_at`.
        // Mark it abandoned so eviction can still rebuild a non-replayable
        // incomplete fact instead of reporting `session_not_found`.
        let expires_at = ended_at.saturating_add(SESSION_TTL_MS);
        match mark_exec_session_abandoned(inner, session, ended_at, exit_code, signal, expires_at) {
            Ok(()) => durable_degraded = true,
            Err(_) => {
                inner.persistence_failures.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    if let Ok(mut status) = session.status.lock() {
        if spool_result.is_ok() && settle_result.is_ok() {
            status.evidence_persisted = true;
            status.persist_abandoned = false;
        } else if settle_result.is_ok() || durable_degraded {
            // A closed or abandoned row now exists, so eviction can rebuild a
            // readable fact. Both writes failing leaves the row `running`;
            // dropping the resident session would turn the next read into
            // `session_not_found`, including when the spool already landed.
            status.persist_abandoned = true;
        }
    }
}

fn mark_exec_session_abandoned(
    inner: &RegistryInner,
    session: &Arc<Session>,
    ended_at: u64,
    exit_code: Option<i32>,
    signal: Option<&str>,
    expires_at: u64,
) -> Result<(), String> {
    let store = inner
        .state_store
        .lock()
        .map_err(|_| "exec state store lock poisoned".to_owned())?;
    store.mark_exec_session_abandoned(&session.id, ended_at, exit_code, signal, expires_at)
}

fn complete_session(
    session: &Arc<Session>,
    registry: Option<&RegistryInner>,
    exit_code: Option<i32>,
    signal: Option<&str>,
) -> bool {
    let ended_at = {
        let Ok(mut status) = session.status.lock() else {
            return false;
        };
        if status.closed {
            return false;
        }
        let ended_at = now_ms();
        // #1010 1b: commit the transition inside the status lock but persist
        // the durable evidence after releasing it — SQLite settle and the
        // spool write never hold `session.status` across disk I/O.
        status.closed = true;
        status.exit_code = exit_code;
        status.signal = signal.map(str::to_owned);
        status.ended_at_ms = Some(ended_at);
        // A registry-less close (unit tests, kill fallback) has no durable
        // evidence obligation; treat it as already persisted so the resident
        // cap can evict it normally.
        status.evidence_persisted = registry.is_none();
        ended_at
    };
    if let Some(registry) = registry {
        persist_closed_evidence(registry, session, ended_at, exit_code, signal);
    }
    true
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
                if let Err(error) = store.settle_exec_session(
                    &id,
                    "closed_before_restart",
                    Some(now),
                    None,
                    None,
                    expires_at,
                ) {
                    // The process is already gone. A locked database must not
                    // hide a committed spool; the row stays auditable as the
                    // pre-restart state until a later writable settle.
                    result
                        .states
                        .insert(id.clone(), format!("settle_failed:{error}"));
                }
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

    /// #967: a failed `pane.close` during termination is reported and recorded
    /// on the session status instead of being folded into the session close.
    #[cfg(unix)]
    #[test]
    fn pane_kill_reports_close_failure_instead_of_claiming_reclaim() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let id = format!(
            "es_pane_close_fail_{}_{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let (session, socket) = pane_session(&id);
        let _sock_guard_socket = crate::test_env::TempUnixSocketGuard::file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request: Value = serde_json::from_str(
                BufReader::new(stream.try_clone().unwrap())
                    .lines()
                    .next()
                    .unwrap()
                    .unwrap()
                    .as_str(),
            )
            .unwrap();
            assert_eq!(request["method"], "pane.close");
            writeln!(
                stream,
                "{}",
                json!({"id": request["id"].clone(), "error": {"code": "close_failed", "message": "pane already gone"}})
            )
            .unwrap();
        });

        let outcome = terminate_session(&session, false, None);
        assert!(matches!(outcome, PaneReclaimOutcome::Failed(_)));
        let status = session_status(&session);
        assert!(status.closed);
        assert_eq!(status.pane_reclaimed, Some(false));
        assert!(
            status
                .pane_reclaim_error
                .as_deref()
                .is_some_and(|error| error.contains("pane already gone")),
            "{:?}",
            status.pane_reclaim_error
        );
        server.join().unwrap();
        fs::remove_file(socket).ok();
    }

    /// #967: a failed `pane.close` on the completion path is recorded on the
    /// session status so reads do not claim a reclaimed pane.
    #[cfg(unix)]
    #[test]
    fn pane_completion_close_failure_is_recorded_on_status() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let id = format!(
            "es_pane_done_close_fail_{}_{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let (session, socket) = pane_session(&id);
        let _sock_guard_socket = crate::test_env::TempUnixSocketGuard::file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request: Value = serde_json::from_str(
                BufReader::new(stream.try_clone().unwrap())
                    .lines()
                    .next()
                    .unwrap()
                    .unwrap()
                    .as_str(),
            )
            .unwrap();
            assert_eq!(request["method"], "pane.close");
            writeln!(
                stream,
                "{}",
                json!({"id": request["id"].clone(), "error": {"code": "close_failed", "message": "pane already gone"}})
            )
            .unwrap();
        });
        let spool = pane_spool(&session).clone();
        fs::write(&spool.status, "0\n").unwrap();

        assert!(refresh_pane_session(&session, None));
        let status = session_status(&session);
        assert!(status.closed);
        assert_eq!(status.exit_code, Some(0));
        assert_eq!(status.pane_reclaimed, Some(false));
        assert!(status.pane_reclaim_error.is_some());
        server.join().unwrap();
        fs::remove_file(socket).ok();
    }

    #[cfg(unix)]
    fn pane_session(id: &str) -> (Arc<Session>, PathBuf) {
        pane_session_with_close(id, true)
    }

    #[cfg(unix)]
    fn pane_session_with_close(id: &str, close_on_complete: bool) -> (Arc<Session>, PathBuf) {
        let socket = env::temp_dir().join(format!(
            "herdr-mcp-pane-monitor-{}-{}.sock",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let script_path = pane_script_path(id);
        let spool = pane_spool_paths(id);
        cleanup_pane_files(&script_path, &spool);
        let session = Arc::new(Session {
            id: id.to_owned(),
            cwd: PathBuf::from("/tmp"),
            command: "pane-test".to_owned(),
            started_at_ms: now_ms(),
            pid: None,
            backend: SessionBackend::Pane {
                client: HerdrClient::new(&socket),
                pane_id: "pane-test".to_owned(),
                script_path,
                spool,
                stdout_offset: Mutex::new(0),
                stderr_offset: Mutex::new(0),
                close_on_complete,
            },
            buffers: Mutex::new(Buffers::default()),
            status: Mutex::new(SessionStatus::default()),
            evidence_missing: false,
            spool_corrupt: false,
        });
        (session, socket)
    }

    #[cfg(unix)]
    #[test]
    fn existing_utility_pane_session_keeps_operation_output_and_does_not_close_pane() {
        let id = format!(
            "es_utility_owned_{}_{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let (session, socket) = pane_session_with_close(&id, false);
        let _sock_guard_socket = crate::test_env::TempUnixSocketGuard::file(&socket);
        let registry = registry();
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .record_pane_exec_running(&id, session.started_at_ms)
            .unwrap();
        let spool = pane_spool(&session).clone();
        fs::write(&spool.stdout, "current-command-only").unwrap();
        fs::write(&spool.status, "0\n").unwrap();

        assert!(refresh_pane_session(&session, Some(&registry.inner)));
        let status = session_status(&session);
        assert!(status.closed);
        assert_eq!(status.exit_code, Some(0));
        let buffers = session.buffers.lock().unwrap();
        let (output, _) = read_buffer_slice(&buffers, None, 0, usize::MAX);
        assert_eq!(output, b"current-command-only");
        assert!(!spool.status.exists());
        assert!(
            !socket.exists(),
            "utility completion must not call pane.close"
        );
    }
    /// `read` reports `running=false` as soon as the status transition
    /// commits; the spool and SQLite settle finish afterwards on the same
    /// thread that triggered the close (possibly the monitor). Restart tests
    /// must wait for BOTH durable pieces before dropping the registry, or the
    /// restart may observe the row without its spool artifact.
    fn wait_for_durable_evidence(path: &Path, id: &str) {
        for _ in 0..500 {
            let spool_ready = exec_spool_path(path, id).exists();
            let row_ready = StateStore::open_in_dir(path, "state.db")
                .and_then(|store| store.get_closed_exec_session(id))
                .ok()
                .flatten()
                .is_some();
            if spool_ready && row_ready {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("durable close evidence for {id} did not land");
    }

    #[cfg(unix)]
    fn pane_spool(session: &Session) -> &PaneSpoolPaths {
        let SessionBackend::Pane { spool, .. } = &session.backend else {
            unreachable!("pane test constructed a non-pane session");
        };
        spool
    }

    #[cfg(unix)]
    fn wait_for_pane_close(session: &Arc<Session>) -> SessionStatus {
        for _ in 0..500 {
            let status = session_status(session);
            if status.closed {
                return status;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("pane session monitor did not close");
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

    #[cfg(unix)]
    #[test]
    fn native_program_mode_passes_literal_argv_without_shell_expansion() {
        let registry = registry();
        let args = vec!["%s\\n".to_owned(), "$HOME;$(uname)* literal".to_owned()];
        let started = registry
            .start_native_program(Path::new("/tmp"), "/usr/bin/printf", &args)
            .unwrap();
        assert_eq!(
            started["command"],
            "'/usr/bin/printf' '%s\\n' '$HOME;$(uname)* literal'"
        );
        let id = started["session_id"].as_str().unwrap().to_owned();
        let view = wait_until_closed(&registry, &id, "stdout", 65536);
        assert_eq!(view["exit_code"], 0);
        assert_eq!(view["text"], "$HOME;$(uname)* literal\n");
        let listed = registry
            .list_views()
            .into_iter()
            .find(|entry| entry["session_id"] == id)
            .unwrap();
        assert_eq!(listed["command"], started["command"]);
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
                "awk 'BEGIN{for(i=0;i<240;i++) print \"line-\" i}'",
            )
            .unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();
        let view = wait_until_closed(&registry, &id, "stdout", 65536);
        assert_eq!(view["ok"], true);
        assert_eq!(view["exit_code"], 0);
        assert_eq!(view["session_id"], id);
        assert_eq!(view["truncated"], false);
        assert_eq!(view["compacted"], true);
        assert_eq!(view["counts"]["lines"], 240);
        assert_eq!(view["counts"]["omitted_lines"], 80);
        let text = view["text"].as_str().unwrap();
        assert!(text.contains("line-0\n"));
        assert!(text.contains("line-39\n"));
        assert!(text.contains("…[omitted 80 lines]…"));
        assert!(text.contains("line-239\n"));
        assert!(!text.contains("line-60\n"));
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
    fn uniform_json_success_compacts_but_chunk_reads_recover_raw_bytes() {
        let registry = registry();
        let command = r#"awk 'BEGIN{printf "["; for(i=0;i<600;i++){if(i)printf ","; printf "{\"id\":%d,\"name\":\"worker-%03d\",\"status\":\"idle\",\"workspace\":\"w1\"}",i,i}; printf "]"}'"#;
        let started = registry.start(Path::new("/tmp"), command).unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();
        let view = wait_until_closed(&registry, &id, "stdout", 65_536);
        assert_eq!(view["exit_code"], 0);
        assert_eq!(view["truncated"], false);
        assert_eq!(view["compacted"], true);
        assert_eq!(view["counts"]["strategy"], "json_object_table_v1");
        let compacted = view["text"].as_str().unwrap();
        let total = view["bytes_total"].as_u64().unwrap() as usize;
        assert!(compacted.len() < total);

        let mut offset = 0;
        let mut raw = String::new();
        while offset < total {
            let chunk = registry.read(&id, "stdout", offset, 4_096);
            assert!(chunk.get("compacted").is_none());
            raw.push_str(chunk["text"].as_str().unwrap());
            let next = chunk["next_offset"].as_u64().unwrap() as usize;
            assert!(next > offset);
            offset = next;
        }
        assert_eq!(raw.len(), total);
        let decoded: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(decoded.as_array().unwrap().len(), 600);
        assert_eq!(decoded[0]["name"], "worker-000");
        assert_eq!(decoded[599]["name"], "worker-599");
    }

    #[test]
    fn completed_incremental_read_exposes_compact_completion_evidence() {
        let registry = registry();
        let started = registry
            .start(
                Path::new("/tmp"),
                "printf '[release-gate] Node tests\\n# tests 12\\n# pass 12\\n# fail 0\\n[release-gate] PASS phase=node\\n'",
            )
            .unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();
        let full = wait_until_closed(&registry, &id, "stdout", 65536);
        let total = full["bytes_total"].as_u64().unwrap() as usize;
        let incremental = registry.read(&id, "stdout", total, 1024);

        assert_eq!(incremental["next_offset"], total);
        assert_eq!(incremental["completion"]["phase"], "completed");
        assert_eq!(incremental["completion"]["exit_code"], 0);
        assert_eq!(incremental["completion"]["gate_phase"], "node");
        assert_eq!(incremental["completion"]["tests"]["passed"], 12);
        assert_eq!(incremental["completion"]["tests"]["failed"], 0);
        assert_eq!(incremental["completion"]["full_log_available"], true);
        assert_eq!(incremental["completion"]["log_session_id"], id);
    }

    #[test]
    fn failure_keeps_full_diagnostic_output() {
        let registry = registry();
        let started = registry
            .start(
                Path::new("/tmp"),
                "printf 'FAILED test_release_gate\\n'; awk 'BEGIN{for(i=0;i<90;i++) print \"fail-\" i}'; exit 3",
            )
            .unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();
        let view = wait_until_closed(&registry, &id, "stdout", 65536);
        assert_eq!(view["exit_code"], 3);
        assert_eq!(view["completion"]["exit_code"], 3);
        assert_eq!(
            view["completion"]["first_failure"],
            "FAILED test_release_gate"
        );
        assert_eq!(view["completion"]["full_log_available"], true);
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
            pid: Some(pid),
            backend: SessionBackend::Native {
                child: Mutex::new(child),
                output_readers: AtomicUsize::new(output_readers),
            },
            buffers: Mutex::new(Buffers::default()),
            status: Mutex::new(SessionStatus::default()),
            evidence_missing: false,
            spool_corrupt: false,
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
            thread::sleep(Duration::from_millis(50));
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

    #[cfg(unix)]
    #[test]
    fn pane_monitor_observes_normal_exit_without_read_polling() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let id = format!(
            "es_pane_monitor_normal_{}_{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let (session, socket) = pane_session(&id);
        let _sock_guard_socket = crate::test_env::TempUnixSocketGuard::file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request: Value = serde_json::from_str(
                BufReader::new(stream.try_clone().unwrap())
                    .lines()
                    .next()
                    .unwrap()
                    .unwrap()
                    .as_str(),
            )
            .unwrap();
            assert_eq!(request["method"], "pane.close");
            writeln!(
                stream,
                "{}",
                json!({"id": request["id"].clone(), "result": {"ok": true}})
            )
            .unwrap();
        });
        let registry = registry();
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .record_pane_exec_running(&id, session.started_at_ms)
            .unwrap();
        let spool = pane_spool(&session).clone();
        fs::write(&spool.stdout, "out").unwrap();
        fs::write(&spool.stderr, "err").unwrap();

        spawn_monitor(Arc::clone(&session), Arc::downgrade(&registry.inner));
        fs::write(&spool.status, "7\n").unwrap();

        let status = wait_for_pane_close(&session);
        server.join().unwrap();
        assert_eq!(status.exit_code, Some(7));
        assert_eq!(status.signal, None);
        let buffers = session.buffers.lock().unwrap();
        let (output, _) = read_buffer_slice(&buffers, None, 0, usize::MAX);
        assert_eq!(output, b"outerr");
        assert!(!spool.status.exists());
        let store = registry.inner.state_store.lock().unwrap();
        assert_eq!(
            store
                .scalar_text(&format!(
                    "SELECT state FROM exec_sessions WHERE session_id = '{}'",
                    session.id
                ))
                .unwrap()
                .as_deref(),
            Some("closed")
        );
        drop(store);
        assert_eq!(registry.diagnostics()["persistence_failures"], 0);
        fs::remove_file(socket).ok();
    }

    #[cfg(unix)]
    #[test]
    fn pane_monitor_exits_after_forced_close_without_status_file() {
        let id = format!(
            "es_pane_monitor_forced_{}_{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let (session, socket) = pane_session(&id);
        let _sock_guard_socket = crate::test_env::TempUnixSocketGuard::file(&socket);
        let weak = Arc::downgrade(&session);
        spawn_monitor(Arc::clone(&session), Weak::new());

        assert!(complete_session(&session, None, None, Some("pane_closed")));
        drop(session);
        for _ in 0..500 {
            if weak.upgrade().is_none() {
                fs::remove_file(socket).ok();
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("pane monitor retained a force-closed session");
    }

    #[cfg(unix)]
    #[test]
    fn pane_monitor_survives_registry_restart_until_status_can_be_reaped() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let id = format!(
            "es_pane_monitor_restart_{}_{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let (session, socket) = pane_session(&id);
        let _sock_guard_socket = crate::test_env::TempUnixSocketGuard::file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request: Value = serde_json::from_str(
                BufReader::new(stream.try_clone().unwrap())
                    .lines()
                    .next()
                    .unwrap()
                    .unwrap()
                    .as_str(),
            )
            .unwrap();
            assert_eq!(request["method"], "pane.close");
            writeln!(
                stream,
                "{}",
                json!({"id": request["id"].clone(), "result": {"ok": true}})
            )
            .unwrap();
        });
        let registry = registry();
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .record_pane_exec_running(&id, session.started_at_ms)
            .unwrap();
        let state_path = registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .path()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let weak_registry = Arc::downgrade(&registry.inner);
        let spool = pane_spool(&session).clone();
        spawn_monitor(Arc::clone(&session), weak_registry);
        drop(registry);
        let restarted = ExecRegistry::new(state_path).unwrap();
        assert_eq!(restarted.diagnostics()["reaped_on_boot"], 0);
        assert_eq!(restarted.diagnostics()["closed_on_boot"], 0);

        fs::write(&spool.status, "0\n").unwrap();

        let status = wait_for_pane_close(&session);
        server.join().unwrap();
        assert_eq!(status.exit_code, Some(0));
        assert!(!spool.status.exists());
        fs::remove_file(socket).ok();
    }

    #[cfg(unix)]
    #[test]
    fn duplicate_pane_monitors_close_once_and_both_exit() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let id = format!(
            "es_pane_monitor_duplicate_{}_{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let (session, socket) = pane_session(&id);
        let _sock_guard_socket = crate::test_env::TempUnixSocketGuard::file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let close_count = Arc::new(AtomicUsize::new(0));
        let server_count = Arc::clone(&close_count);
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(300);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        let request: Value = serde_json::from_str(
                            BufReader::new(stream.try_clone().unwrap())
                                .lines()
                                .next()
                                .unwrap()
                                .unwrap()
                                .as_str(),
                        )
                        .unwrap();
                        assert_eq!(request["method"], "pane.close");
                        server_count.fetch_add(1, Ordering::Relaxed);
                        writeln!(
                            stream,
                            "{}",
                            json!({"id": request["id"].clone(), "result": {"ok": true}})
                        )
                        .unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("pane close listener failed: {error}"),
                }
            }
        });
        let weak = Arc::downgrade(&session);
        let spool = pane_spool(&session).clone();
        spawn_monitor(Arc::clone(&session), Weak::new());
        spawn_monitor(Arc::clone(&session), Weak::new());
        fs::write(&spool.status, "0\n").unwrap();

        assert_eq!(wait_for_pane_close(&session).exit_code, Some(0));
        drop(session);
        for _ in 0..500 {
            if weak.upgrade().is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(weak.upgrade().is_none());
        server.join().unwrap();
        assert_eq!(close_count.load(Ordering::Relaxed), 1);
        fs::remove_file(socket).ok();
    }

    #[test]
    fn pane_spool_paths_are_session_scoped() {
        let a = pane_spool_paths("es_demo_a");
        let b = pane_spool_paths("es_demo_b");
        assert_ne!(a.stdout, b.stdout);
        assert_ne!(a.stderr, b.stderr);
        assert_ne!(a.status, b.status);
        assert!(a.stdout.to_string_lossy().ends_with(".stdout"));
        assert!(a.stderr.to_string_lossy().ends_with(".stderr"));
        assert!(a.status.to_string_lossy().ends_with(".status"));
        assert!(a.status_tmp.to_string_lossy().ends_with(".status.tmp"));
    }

    #[test]
    fn pane_script_spools_streams_and_publishes_status_atomically() {
        let id = format!(
            "es_script_test_{}_{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let script = pane_script_path(&id);
        let spool = pane_spool_paths(&id);
        write_pane_script(
            &script,
            Path::new("/tmp"),
            "printf out; printf err >&2; exit 7",
            &spool,
        )
        .unwrap();
        let body = fs::read_to_string(&script).unwrap();
        assert!(body.contains(spool.stdout.to_string_lossy().as_ref()));
        assert!(body.contains(spool.stderr.to_string_lossy().as_ref()));
        assert!(body.contains(spool.status_tmp.to_string_lossy().as_ref()));
        assert!(body.contains(spool.status.to_string_lossy().as_ref()));
        assert!(body.contains("mv -f --"));
        cleanup_pane_files(&script, &spool);
    }

    #[test]
    fn pane_exit_code_requires_complete_integer_status() {
        let id = format!(
            "es_status_test_{}_{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let spool = pane_spool_paths(&id);
        fs::write(&spool.status, "7\n").unwrap();
        assert_eq!(read_pane_exit_code(&spool.status), Some(7));
        fs::write(&spool.status, "partial").unwrap();
        assert_eq!(read_pane_exit_code(&spool.status), None);
        cleanup_pane_files(Path::new("/tmp/no-pane-script"), &spool);
    }

    #[test]
    fn completed_session_evidence_survives_restart_without_reexecution() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-restart-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let marker_path = env::temp_dir().join(format!(
            "herdr-mcp-exec-marker-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&marker_path);

        let registry = ExecRegistry::new(path.clone()).unwrap();
        let cmd = format!(
            "printf 'marker-run\\n' >> {}; printf 'hello-stdout\\n'; printf 'hello-stderr\\n' >&2; exit 42",
            shell_quote(&marker_path.to_string_lossy())
        );
        let started = registry.start(Path::new("/tmp"), &cmd).unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();

        let initial_view = wait_until_closed(&registry, &id, "both", 65536);
        assert_eq!(initial_view["phase"], "completed");
        assert_eq!(initial_view["running"], false);
        assert_eq!(initial_view["exit_code"], 42);
        assert!(
            initial_view["text"]
                .as_str()
                .unwrap()
                .contains("hello-stdout")
        );
        assert!(
            initial_view["text"]
                .as_str()
                .unwrap()
                .contains("hello-stderr")
        );
        assert!(initial_view["started_at"].is_string());
        assert!(initial_view["finished_at"].is_string());

        let marker_content = fs::read_to_string(&marker_path).unwrap();
        assert_eq!(marker_content.matches("marker-run").count(), 1);

        wait_for_durable_evidence(&path, &id);

        #[cfg(unix)]
        {
            let spool = exec_spool_path(&path, &id);
            let mode = fs::metadata(&spool).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        drop(registry);

        let restarted = ExecRegistry::new(path.clone()).unwrap();

        let restarted_both = restarted.read(&id, "both", 0, 65536);
        assert_eq!(restarted_both["ok"], true);
        assert_eq!(restarted_both["phase"], "completed");
        assert_eq!(restarted_both["running"], false);
        assert_eq!(restarted_both["exit_code"], 42);
        assert_eq!(restarted_both["signal"], Value::Null);
        assert_eq!(restarted_both["recovered"], true);
        assert!(restarted_both["started_at"].is_string());
        assert!(restarted_both["finished_at"].is_string());
        assert_eq!(restarted_both["started_at"], initial_view["started_at"]);
        assert_eq!(restarted_both["finished_at"], initial_view["finished_at"]);
        let both_text = restarted_both["text"].as_str().unwrap();
        assert!(both_text.contains("hello-stdout"));
        assert!(both_text.contains("hello-stderr"));

        // Verify spool does NOT persist command strings or cwd
        let spool_content = fs::read_to_string(exec_spool_path(&path, &id)).unwrap();
        assert!(!spool_content.contains("marker-run"));
        assert!(!spool_content.contains("printf"));

        let restarted_stdout = restarted.read(&id, "stdout", 0, 65536);
        assert_eq!(restarted_stdout["ok"], true);
        assert_eq!(restarted_stdout["recovered"], true);
        assert_eq!(restarted_stdout["text"], "hello-stdout\n");

        let restarted_stderr = restarted.read(&id, "stderr", 0, 65536);
        assert_eq!(restarted_stderr["ok"], true);
        assert_eq!(restarted_stderr["recovered"], true);
        assert_eq!(restarted_stderr["text"], "hello-stderr\n");

        let marker_after = fs::read_to_string(&marker_path).unwrap();
        assert_eq!(marker_after.matches("marker-run").count(), 1);

        let views = restarted.list_views();
        let recovered_view = views.iter().find(|v| v["session_id"] == id).unwrap();
        assert_eq!(recovered_view["recovered"], true);
        assert_eq!(recovered_view["running"], false);
        assert_eq!(recovered_view["exit_code"], 42);
        assert_eq!(recovered_view["command"], Value::Null);

        let missing = restarted.read("es_nonexistent_12345", "both", 0, 64);
        assert_eq!(missing["ok"], false);
        assert_eq!(missing["reason"], "session_not_found");

        let expired_id = "es_expired_restart_test";
        let store = StateStore::open_in_dir(&path, "state.db").unwrap();
        store
            .record_exec_running(expired_id, 99999, None, 100)
            .unwrap();
        store
            .settle_exec_session(expired_id, "closed", Some(200), Some(0), None, 300)
            .unwrap();
        drop(store);
        let restarted_with_expired = ExecRegistry::new(path.clone()).unwrap();
        let expired_view = restarted_with_expired.read(expired_id, "both", 0, 64);
        assert_eq!(expired_view["ok"], false);
        assert_eq!(expired_view["reason"], "session_not_found");

        drop(restarted_with_expired);
        drop(restarted);
        let _ = fs::remove_file(&marker_path);
        let _ = fs::remove_dir_all(&path);
    }

    #[test]
    fn unexpired_spools_beyond_recovery_batch_limit_are_not_cleaned_up() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-beyond-batch-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let registry = ExecRegistry::new(path.clone()).unwrap();
        let store = StateStore::open_in_dir(&path, "state.db").unwrap();
        let now = now_ms();

        // Create 70 completed sessions (more than RECOVERY_MAX_ENTRIES=64)
        for i in 0..70 {
            let session_id = format!("es_batch_{i}");
            store
                .record_exec_running(&session_id, 1000 + i, None, now)
                .unwrap();
            store
                .settle_exec_session(
                    &session_id,
                    "closed",
                    Some(now + 10),
                    Some(0),
                    None,
                    now + SESSION_TTL_MS,
                )
                .unwrap();
            let spool_session = Session {
                id: session_id.clone(),
                cwd: PathBuf::new(),
                command: String::new(),
                started_at_ms: now,
                pid: None,
                backend: SessionBackend::Completed,
                buffers: Mutex::new(Buffers {
                    chunks: vec![Chunk {
                        seq: 0,
                        stream: StreamKind::Stdout,
                        data: format!("output-{i}\n").into_bytes(),
                    }],
                    next_seq: 1,
                    stdout_bytes: format!("output-{i}\n").len(),
                    stderr_bytes: 0,
                    truncated: false,
                }),
                status: Mutex::new(SessionStatus {
                    closed: true,
                    exit_code: Some(0),
                    signal: None,
                    ended_at_ms: Some(now + 10),
                    evidence_persisted: true,
                    ..SessionStatus::default()
                }),
                evidence_missing: false,
                spool_corrupt: false,
            };
            write_session_spool(&path, &spool_session).unwrap();
        }
        drop(store);
        drop(registry);

        // Reopen registry - must NOT delete the 65th+ spool
        let restarted = ExecRegistry::new(path.clone()).unwrap();

        // Check session 69 (the 70th session)
        let s69 = restarted.read("es_batch_69", "stdout", 0, 64);
        assert_eq!(s69["ok"], true);
        assert_eq!(s69["phase"], "completed");
        assert_eq!(s69["text"], "output-69\n");
        assert_eq!(s69["recovered"], true);

        drop(restarted);
        let _ = fs::remove_dir_all(&path);
    }

    fn loaded_completed_session(
        id: &str,
        started_at_ms: u64,
        ended_at_ms: u64,
        output: &str,
    ) -> Arc<Session> {
        Arc::new(Session {
            id: id.to_owned(),
            cwd: PathBuf::new(),
            command: String::new(),
            started_at_ms,
            pid: None,
            backend: SessionBackend::Completed,
            buffers: Mutex::new(Buffers {
                chunks: vec![Chunk {
                    seq: 0,
                    stream: StreamKind::Stdout,
                    data: output.as_bytes().to_vec(),
                }],
                next_seq: 1,
                stdout_bytes: output.len(),
                stderr_bytes: 0,
                truncated: false,
            }),
            status: Mutex::new(SessionStatus {
                closed: true,
                exit_code: Some(0),
                signal: None,
                ended_at_ms: Some(ended_at_ms),
                evidence_persisted: true,
                ..SessionStatus::default()
            }),
            evidence_missing: false,
            spool_corrupt: false,
        })
    }

    #[test]
    fn loaded_closed_sessions_are_capped_without_losing_durable_evidence() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-loaded-cap-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let registry = ExecRegistry::new(path.clone()).unwrap();
        let now = now_ms();
        let base = now.saturating_sub(120_000);
        let total_closed = RECOVERY_MAX_ENTRIES + 6;

        for i in 0..total_closed {
            let session_id = format!("es_loaded_{i:03}");
            let started_at_ms = base + i as u64;
            let ended_at_ms = started_at_ms + 1;
            let session = loaded_completed_session(
                &session_id,
                started_at_ms,
                ended_at_ms,
                &format!("output-{i}\n"),
            );
            {
                let store = registry.inner.state_store.lock().unwrap();
                store
                    .record_exec_running(
                        &session_id,
                        20_000 + u32::try_from(i).unwrap(),
                        None,
                        started_at_ms,
                    )
                    .unwrap();
                store
                    .settle_exec_session(
                        &session_id,
                        "closed",
                        Some(ended_at_ms),
                        Some(0),
                        None,
                        now + SESSION_TTL_MS,
                    )
                    .unwrap();
            }
            write_session_spool(&path, &session).unwrap();
            registry
                .inner
                .sessions
                .lock()
                .unwrap()
                .insert(session_id, session);
        }

        let running_id = "es_loaded_running".to_owned();
        {
            let store = registry.inner.state_store.lock().unwrap();
            store.record_pane_exec_running(&running_id, now).unwrap();
        }
        registry.inner.sessions.lock().unwrap().insert(
            running_id.clone(),
            Arc::new(Session {
                id: running_id.clone(),
                cwd: PathBuf::new(),
                command: "still-running".to_owned(),
                started_at_ms: now,
                pid: None,
                backend: SessionBackend::Completed,
                buffers: Mutex::new(Buffers::default()),
                status: Mutex::new(SessionStatus::default()),
                evidence_missing: false,
                spool_corrupt: false,
            }),
        );

        registry.prune();
        let views = registry.list_views();
        assert_eq!(views.len(), RECOVERY_MAX_ENTRIES + 1);
        assert!(
            views
                .iter()
                .any(|view| view["session_id"] == running_id.as_str() && view["running"] == true)
        );
        assert!(
            !views
                .iter()
                .any(|view| view["session_id"] == "es_loaded_000")
        );
        let newest = format!("es_loaded_{:03}", total_closed - 1);
        assert!(
            views
                .iter()
                .any(|view| view["session_id"] == newest.as_str())
        );

        // Evicting the oldest completion must not delete its durable evidence.
        let oldest_spool = exec_spool_path(&path, "es_loaded_000");
        assert!(oldest_spool.exists());
        let recovered = registry.read("es_loaded_000", "stdout", 0, 64);
        assert_eq!(recovered["ok"], true);
        assert_eq!(recovered["phase"], "completed");
        assert_eq!(recovered["recovered"], true);
        assert_eq!(recovered["text"], "output-0\n");
        assert!(oldest_spool.exists());

        // A recovery read rebinds the session, but the next prune re-evicts it
        // instead of letting the resident set grow past the cap.
        let rebound = registry.list_views();
        assert_eq!(rebound.len(), RECOVERY_MAX_ENTRIES + 1);
        assert!(
            !rebound
                .iter()
                .any(|view| view["session_id"] == "es_loaded_000")
        );

        drop(registry);
        let _ = fs::remove_dir_all(&path);
    }

    #[test]
    fn loaded_closed_sessions_at_the_recovery_limit_are_retained() {
        let registry = registry();
        let now = now_ms();
        for i in 0..RECOVERY_MAX_ENTRIES {
            let id = format!("es_at_cap_{i:03}");
            let session = loaded_completed_session(&id, now.saturating_sub(1), now, "output\n");
            registry.inner.sessions.lock().unwrap().insert(id, session);
        }

        registry.prune();
        {
            let sessions = registry.inner.sessions.lock().unwrap();
            assert_eq!(sessions.len(), RECOVERY_MAX_ENTRIES);
            assert!(sessions.contains_key("es_at_cap_000"));
        }

        // One past the cap evicts exactly the oldest completion, not the newest.
        registry.inner.sessions.lock().unwrap().insert(
            "es_at_cap_extra".to_owned(),
            loaded_completed_session("es_at_cap_extra", now, now, "output\n"),
        );
        registry.prune();
        {
            let sessions = registry.inner.sessions.lock().unwrap();
            assert_eq!(sessions.len(), RECOVERY_MAX_ENTRIES);
            assert!(!sessions.contains_key("es_at_cap_000"));
            assert!(sessions.contains_key("es_at_cap_extra"));
            assert!(sessions.contains_key("es_at_cap_063"));
        }
    }

    #[test]
    fn health_diagnostics_does_not_wait_for_exec_maintenance_locks() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-health-nonblocking-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let registry = ExecRegistry::new(path.clone()).unwrap();

        let sessions = registry.inner.sessions.lock().unwrap();
        let state_store = registry.inner.state_store.lock().unwrap();
        let health = registry.health_diagnostics();

        assert_eq!(health["ready"], true);
        assert_eq!(health["sessions_busy"], true);
        assert!(health["count"].is_null());
        assert!(health["running"].is_null());
        assert_eq!(health["state_store_ready"], true);
        assert_eq!(health["state_store_busy"], true);
        assert_eq!(
            health["state_store_schema"],
            crate::state_store::SCHEMA_VERSION
        );

        drop(state_store);
        drop(sessions);
        drop(registry);
        let _ = fs::remove_dir_all(&path);
    }

    #[test]
    fn recovered_read_beyond_spool_cap_reaches_terminal_offset_without_unread_gap() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-large-spool-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let registry = ExecRegistry::new(path.clone()).unwrap();

        let started = registry
            .start(
                Path::new("/tmp"),
                "awk 'BEGIN{for(i=0;i<4000;i++) print \"line-\" i, \"padding-01234567890123456789\"}'; exit 0",
            )
            .unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();

        let initial_view = wait_until_closed(&registry, &id, "stdout", MAX_BUFFER_PER_STREAM);
        assert_eq!(initial_view["phase"], "completed");
        assert_eq!(initial_view["exit_code"], 0);
        let original_total = initial_view["bytes_total"].as_u64().unwrap() as usize;
        assert!(original_total > RECOVERY_SPOOL_CAP);

        wait_for_durable_evidence(&path, &id);

        drop(registry);

        let restarted = ExecRegistry::new(path.clone()).unwrap();

        let recovered_view = restarted.read(&id, "stdout", 0, 262_144);
        assert_eq!(recovered_view["ok"], true);
        assert_eq!(recovered_view["phase"], "completed");
        assert_eq!(recovered_view["running"], false);
        assert_eq!(recovered_view["recovered"], true);
        assert_eq!(recovered_view["truncated"], true);

        let bytes_total = recovered_view["bytes_total"].as_u64().unwrap() as usize;
        let next_offset = recovered_view["next_offset"].as_u64().unwrap() as usize;
        assert_eq!(bytes_total, RECOVERY_SPOOL_CAP);
        assert_eq!(next_offset, bytes_total);

        let text = recovered_view["text"].as_str().unwrap();
        assert_eq!(text.len(), RECOVERY_SPOOL_CAP);
        assert!(text.contains("line-3999"));

        let terminal_view = restarted.read(&id, "stdout", next_offset, 65536);
        assert_eq!(terminal_view["ok"], true);
        assert_eq!(terminal_view["phase"], "completed");
        assert_eq!(terminal_view["running"], false);
        assert_eq!(terminal_view["recovered"], true);
        assert_eq!(terminal_view["text"], "");
        assert_eq!(terminal_view["offset"], next_offset);
        assert_eq!(terminal_view["next_offset"], next_offset);
        assert_eq!(terminal_view["bytes_total"], bytes_total);

        drop(restarted);
        let _ = fs::remove_dir_all(&path);
    }

    /// #1010 1b: 48 short sessions start and close concurrently on one
    /// registry; every close must persist its spool exactly once with zero
    /// persistence failures, and the session mutex must not stay held across
    /// the SQLite settle or the spool write.
    #[test]
    fn concurrent_session_close_persists_each_spool_once_without_failures() {
        let registry = registry();
        const SESSIONS: usize = 48;
        let ids: Vec<String> = (0..SESSIONS)
            .map(|index| {
                registry
                    .start(Path::new("/tmp"), &format!("printf 'stress-{index}\\n'"))
                    .unwrap()["session_id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let views: Vec<Value> = thread::scope(|scope| {
            let handles: Vec<_> = ids
                .iter()
                .map(|id| scope.spawn(|| wait_until_closed(&registry, id, "stdout", 65536)))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        for (index, view) in views.iter().enumerate() {
            assert_eq!(view["exit_code"], 0, "session {index}");
            assert_eq!(view["running"], false, "session {index}");
            assert_eq!(view["phase"], "completed", "session {index}");
        }
        // Completion now publishes `running=false` before the durable settle
        // and spool commit finish — that is the whole point of moving disk I/O
        // out of the status lock — so wait for each spool to land rather than
        // asserting in the same instant.
        for id in &ids {
            let spool = exec_spool_path(&registry.inner.state_dir, id);
            let mut landed = false;
            for _ in 0..500 {
                if spool.exists() {
                    landed = true;
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            assert!(landed, "missing spool for {id}");
        }
        assert_eq!(registry.diagnostics()["persistence_failures"], 0);
    }

    /// Construct a session in the closed-but-unpersisted intermediate state a
    /// caller can observe between the status transition and the durable commit.
    fn unpersisted_closed_session(id: &str, output: &str) -> Arc<Session> {
        Arc::new(Session {
            id: id.to_owned(),
            cwd: PathBuf::new(),
            command: String::new(),
            started_at_ms: now_ms(),
            pid: None,
            backend: SessionBackend::Completed,
            buffers: Mutex::new(Buffers {
                chunks: vec![Chunk {
                    seq: 0,
                    stream: StreamKind::Stdout,
                    data: output.as_bytes().to_vec(),
                }],
                next_seq: 1,
                stdout_bytes: output.len(),
                stderr_bytes: 0,
                truncated: false,
            }),
            status: Mutex::new(SessionStatus {
                closed: true,
                exit_code: Some(0),
                signal: None,
                ended_at_ms: Some(now_ms()),
                // Deliberately false: the durable row/spool are still
                // in-flight, so the resident cap must not evict this entry.
                evidence_persisted: false,
                ..SessionStatus::default()
            }),
            evidence_missing: false,
            spool_corrupt: false,
        })
    }

    #[test]
    fn unpersisted_closed_session_survives_resident_cap_eviction() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-unpersisted-cap-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let registry = ExecRegistry::new(path.clone()).unwrap();
        let now = now_ms();

        // One closed session whose durable evidence is still in-flight, plus
        // enough fully persisted closures that it would be the first eviction.
        let in_flight = unpersisted_closed_session("es_in_flight", "in-flight-output\n");
        registry
            .inner
            .sessions
            .lock()
            .unwrap()
            .insert("es_in_flight".to_owned(), Arc::clone(&in_flight));
        for i in 0..RECOVERY_MAX_ENTRIES + 4 {
            let session_id = format!("es_done_{i:03}");
            let session = loaded_completed_session(
                &session_id,
                now + i as u64,
                now + i as u64,
                &format!("output-{i}\n"),
            );
            registry
                .inner
                .sessions
                .lock()
                .unwrap()
                .insert(session_id, session);
        }

        registry.prune();

        // The oldest persisted closures are evicted; the unpersisted session is
        // kept because its memory buffers are the only remaining output copy.
        assert!(
            !registry
                .list_views()
                .iter()
                .any(|view| view["session_id"] == "es_done_000")
        );
        let view = registry.read("es_in_flight", "stdout", 0, 64);
        assert_eq!(view["ok"], true);
        assert_eq!(view["running"], false);
        assert_eq!(view["text"], "in-flight-output\n");

        // Once the spool commits, a recovery read must not leave a stale empty
        // projection behind: reading after `write_session_spool` sees output.
        drop(in_flight);
        let _ = fs::remove_dir_all(&path);
    }

    /// user operator | Given a closed session whose bounded persist retries are
    /// exhausted | When the resident cap prunes | Then that session is evicted
    /// and a later read rebuilds the durable row as incomplete evidence.
    #[test]
    fn abandoned_persist_session_is_evicted_and_rebuilt_incomplete() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-abandoned-cap-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let registry = ExecRegistry::new(path.clone()).unwrap();
        let now = now_ms();
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .record_exec_running("es_abandoned", 7_000, None, now)
            .unwrap();
        let abandoned = unpersisted_closed_session("es_abandoned", "only-in-memory\n");
        let spool_dir = exec_spool_dir(&path);
        fs::remove_dir_all(&spool_dir).unwrap();
        fs::write(&spool_dir, "not-a-directory").unwrap();
        persist_closed_evidence(&registry.inner, &abandoned, now, Some(1), None);
        assert!(
            abandoned.status.lock().unwrap().persist_abandoned,
            "exhausted persist retries must abandon the resident pin"
        );
        assert!(!abandoned.status.lock().unwrap().evidence_persisted);
        fs::remove_file(&spool_dir).unwrap();
        fs::create_dir_all(&spool_dir).unwrap();

        registry
            .inner
            .sessions
            .lock()
            .unwrap()
            .insert("es_abandoned".to_owned(), Arc::clone(&abandoned));
        for i in 0..RECOVERY_MAX_ENTRIES + 2 {
            let session_id = format!("es_keep_{i:03}");
            let session = loaded_completed_session(
                &session_id,
                now + 1_000 + i as u64,
                now + 1_000 + i as u64,
                &format!("kept-{i}\n"),
            );
            registry
                .inner
                .sessions
                .lock()
                .unwrap()
                .insert(session_id, session);
        }
        registry.prune();
        assert!(
            !registry
                .list_views()
                .iter()
                .any(|view| view["session_id"] == "es_abandoned"),
            "an abandoned close must be eligible for the resident cap"
        );
        let view = registry.read("es_abandoned", "stdout", 0, 64);
        assert_eq!(view["ok"], false, "{view}");
        assert_eq!(view["reason"], "session_evidence_incomplete");
        assert_eq!(view["recovered"], true);

        drop(abandoned);
        drop(registry);
        let _ = fs::remove_dir_all(&path);
    }

    /// user operator | Given a completed session still resident after persist
    /// retries fail | When it is read before the resident cap evicts it | Then
    /// the caller still receives the completed phase, exit fact, and memory
    /// output.
    #[test]
    fn user_operator_reads_completed_output_while_abandoned_session_stays_resident() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-resident-abandoned-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let registry = ExecRegistry::new(path.clone()).unwrap();
        let now = now_ms();
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .record_exec_running("es_resident_abandoned", 8_000, None, now)
            .unwrap();
        let session = unpersisted_closed_session("es_resident_abandoned", "still-in-memory\n");
        session.status.lock().unwrap().exit_code = Some(7);
        session.status.lock().unwrap().signal = Some("SIGTERM".to_owned());
        let spool_dir = exec_spool_dir(&path);
        fs::remove_dir_all(&spool_dir).unwrap();
        fs::write(&spool_dir, "not-a-directory").unwrap();
        persist_closed_evidence(&registry.inner, &session, now, Some(7), Some("SIGTERM"));
        fs::remove_file(&spool_dir).unwrap();
        fs::create_dir_all(&spool_dir).unwrap();
        assert!(session.status.lock().unwrap().persist_abandoned);
        registry
            .inner
            .sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), Arc::clone(&session));

        let view = registry.read("es_resident_abandoned", "stdout", 0, 64);
        assert_eq!(view["ok"], true, "{view}");
        assert_eq!(view["phase"], "completed");
        assert_eq!(view["exit_code"], 7);
        assert_eq!(view["signal"], "SIGTERM");
        assert_eq!(view["text"], "still-in-memory\n");
        assert_eq!(view["running"], false);

        drop(session);
        drop(registry);
        let _ = fs::remove_dir_all(&path);
    }

    /// user operator | Given a native command whose spool directory cannot be
    /// written | When the durable exec path observes the exit | Then it reports
    /// phase completed from the resident buffers instead of timing out as
    /// running.
    #[test]
    fn user_operator_observes_completed_phase_when_native_persist_fails() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-native-persist-fail-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        let spool_dir = exec_spool_dir(&path);
        fs::create_dir_all(&spool_dir).unwrap();
        let registry = ExecRegistry::new(path.clone()).unwrap();
        fs::remove_dir_all(&spool_dir).unwrap();
        fs::write(&spool_dir, "not-a-directory").unwrap();
        let cwd = path.join("cwd");
        fs::create_dir_all(&cwd).unwrap();

        let started = registry.start(&cwd, "printf native-output").unwrap();
        let session_id = started["session_id"].as_str().unwrap();
        let mut view = Value::Null;
        for _ in 0..100 {
            view = registry.read(session_id, "stdout", 0, 64);
            if view["phase"] == "completed" {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(view["ok"], true, "{view}");
        assert_eq!(view["phase"], "completed", "{view}");
        assert_eq!(view["text"], "native-output");
        assert_ne!(view["phase"], "running");

        drop(registry);
        let _ = fs::remove_file(&spool_dir);
        let _ = fs::remove_dir_all(&path);
    }

    /// user operator | Given settle_exec_session fails and the resident cap
    /// then drops the session | When the session is read before and after a
    /// restart | Then the row is abandoned with an expiry and the read stays
    /// incomplete instead of session_not_found.
    #[test]
    fn user_operator_rebuilds_abandoned_row_after_settle_failure_and_restart() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-settle-abandoned-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let registry = ExecRegistry::new(path.clone()).unwrap();
        let now = now_ms();
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .record_exec_running("es_settle_fail", 11_000, None, now)
            .unwrap();
        let session = unpersisted_closed_session("es_settle_fail", "committed-spool\n");
        write_session_spool(&path, &session).unwrap();
        // The durable row stays `running` with a null expiry. Settle is forced
        // to fail, then the abandoned marker records the non-replayable fact.
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .fail_next_exec_settle_for_test();
        persist_closed_evidence(&registry.inner, &session, now, Some(4), None);
        assert!(session.status.lock().unwrap().persist_abandoned);
        registry
            .inner
            .sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), Arc::clone(&session));

        let resident = registry.read("es_settle_fail", "stdout", 0, 64);
        assert_eq!(resident["phase"], "completed", "{resident}");
        assert_eq!(resident["text"], "committed-spool\n");

        registry
            .inner
            .sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), Arc::clone(&session));
        for i in 0..RECOVERY_MAX_ENTRIES + 1 {
            let session_id = format!("es_fill_{i:03}");
            registry.inner.sessions.lock().unwrap().insert(
                session_id.clone(),
                loaded_completed_session(&session_id, now + 2_000 + i as u64, now + 2_000, "x\n"),
            );
        }
        registry.prune();
        let evicted = registry.read("es_settle_fail", "stdout", 0, 64);
        assert_eq!(evicted["ok"], true, "{evicted}");
        assert_eq!(evicted["phase"], "completed");
        assert_eq!(evicted["text"], "committed-spool\n");
        assert_eq!(evicted["exit_code"], 4);

        drop(session);
        drop(registry);
        let restarted = ExecRegistry::new(path.clone()).unwrap();
        let booted = restarted.read("es_settle_fail", "stdout", 0, 64);
        assert_eq!(booted["ok"], true, "{booted}");
        assert_eq!(booted["phase"], "completed");
        assert_eq!(booted["text"], "committed-spool\n");
        assert_eq!(booted["exit_code"], 4);
        assert_ne!(booted["reason"], "session_not_found");
        drop(restarted);
        let _ = fs::remove_dir_all(&path);
    }

    /// user operator | Given a committed spool whose JSON is legal but whose
    /// base64 tail is not | When the closed row is read | Then the result is
    /// session_spool_corrupt and a real empty tail stays successful.
    #[test]
    fn user_operator_rejects_legal_json_with_corrupt_base64_as_spool_corrupt() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-bad-base64-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let store = StateStore::open_in_dir(&path, "state.db").unwrap();
        let now = now_ms();
        for id in ["es_bad_base64", "es_zero"] {
            store.record_exec_running(id, 12_000, None, now).unwrap();
            store
                .settle_exec_session(
                    id,
                    "closed",
                    Some(now + 3),
                    Some(0),
                    None,
                    now + SESSION_TTL_MS,
                )
                .unwrap();
        }
        drop(store);
        fs::create_dir_all(exec_spool_dir(&path)).unwrap();
        fs::write(
            exec_spool_path(&path, "es_bad_base64"),
            br#"{"version":1,"session_id":"es_bad_base64","stdout_bytes":4,"stderr_bytes":0,"truncated":false,"stdout_tail":"!!!!","stderr_tail":""}"#,
        )
        .unwrap();
        let empty = unpersisted_closed_session("es_zero", "");
        empty.buffers.lock().unwrap().chunks.clear();
        empty.buffers.lock().unwrap().stdout_bytes = 0;
        write_session_spool(&path, &empty).unwrap();

        let registry = ExecRegistry::new(path.clone()).unwrap();
        let corrupt = registry.read("es_bad_base64", "both", 0, 64);
        assert_eq!(corrupt["ok"], false, "{corrupt}");
        assert_eq!(corrupt["reason"], "session_spool_corrupt");
        let zero = registry.read("es_zero", "both", 0, 64);
        assert_eq!(zero["ok"], true, "{zero}");
        assert_eq!(zero["phase"], "completed");
        assert_eq!(zero["text"], "");

        drop(registry);
        let _ = fs::remove_dir_all(&path);
    }

    /// user operator | Given a closed row with a corrupt spool and another with
    /// a committed empty spool | When each is read | Then corruption is
    /// `session_spool_corrupt` and a real zero-byte capture stays successful.
    #[test]
    fn corrupt_spool_is_not_reported_as_empty_output() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-spool-corrupt-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let store = StateStore::open_in_dir(&path, "state.db").unwrap();
        let now = now_ms();
        for id in ["es_corrupt", "es_empty"] {
            store.record_exec_running(id, 9_000, None, now).unwrap();
            store
                .settle_exec_session(
                    id,
                    "closed",
                    Some(now + 5),
                    Some(0),
                    None,
                    now + SESSION_TTL_MS,
                )
                .unwrap();
        }
        drop(store);

        fs::create_dir_all(exec_spool_dir(&path)).unwrap();
        fs::write(exec_spool_path(&path, "es_corrupt"), b"{not-json").unwrap();
        let empty = unpersisted_closed_session("es_empty", "");
        empty.buffers.lock().unwrap().chunks.clear();
        empty.buffers.lock().unwrap().stdout_bytes = 0;
        empty.buffers.lock().unwrap().next_seq = 0;
        write_session_spool(&path, &empty).unwrap();

        let registry = ExecRegistry::new(path.clone()).unwrap();
        let corrupt = registry.read("es_corrupt", "both", 0, 64);
        assert_eq!(corrupt["ok"], false, "{corrupt}");
        assert_eq!(corrupt["reason"], "session_spool_corrupt");
        let empty_view = registry.read("es_empty", "both", 0, 64);
        assert_eq!(empty_view["ok"], true, "{empty_view}");
        assert_eq!(empty_view["text"], "");
        assert_eq!(empty_view["phase"], "completed");

        drop(registry);
        let _ = fs::remove_dir_all(&path);
    }

    /// user operator | Given both the closed settle and the abandoned marker
    /// fail | When the resident cap runs and the process restarts | Then the
    /// session stays resident and a later read is not session_not_found.
    #[test]
    fn user_operator_keeps_session_resident_when_both_state_writes_fail() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-query-only-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let registry = ExecRegistry::new(path.clone()).unwrap();
        let now = now_ms();
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .record_pane_exec_running("es_query_only", now)
            .unwrap();
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .fail_exec_state_writes_for_test();
        let session = unpersisted_closed_session("es_query_only", "still-readable\n");
        write_session_spool(&path, &session).unwrap();
        persist_closed_evidence(&registry.inner, &session, now, Some(9), None);
        assert!(
            !session.status.lock().unwrap().persist_abandoned,
            "a failed abandoned marker must not make the resident session evictable"
        );
        assert!(!session.status.lock().unwrap().evidence_persisted);
        registry
            .inner
            .sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), Arc::clone(&session));
        for i in 0..RECOVERY_MAX_ENTRIES + 2 {
            let session_id = format!("es_query_fill_{i:03}");
            registry.inner.sessions.lock().unwrap().insert(
                session_id.clone(),
                loaded_completed_session(&session_id, now + 3_000 + i as u64, now + 3_000, "y\n"),
            );
        }
        registry.prune();
        assert!(
            registry
                .list_views()
                .iter()
                .any(|view| view["session_id"] == "es_query_only"),
            "both durable writes failed, so the memory session must stay"
        );
        let resident = registry.read("es_query_only", "stdout", 0, 64);
        assert_eq!(resident["ok"], true, "{resident}");
        assert_eq!(resident["phase"], "completed");
        assert_eq!(resident["text"], "still-readable\n");
        assert_ne!(resident["reason"], "session_not_found");

        drop(session);
        drop(registry);
        let restarted = ExecRegistry::new(path.clone()).unwrap();
        let booted = restarted.read("es_query_only", "stdout", 0, 64);
        // The row stayed `running` because neither state write landed. Restart
        // still finds it through the unsettled lookup and serves the spool
        // that did commit. It must not become `session_not_found`.
        assert_eq!(booted["ok"], true, "{booted}");
        assert_eq!(booted["phase"], "completed");
        assert_eq!(booted["text"], "still-readable\n");
        assert_ne!(booted["reason"], "session_not_found");
        assert!(exec_spool_path(&path, "es_query_only").is_file());
        let bounded = restarted
            .inner
            .state_store
            .lock()
            .unwrap()
            .get_unsettled_exec_session("es_query_only")
            .unwrap()
            .expect("bounded unsettled row");
        assert!(bounded.ended_at_ms.is_some());
        assert!(bounded.expires_at_ms.is_some());
        restarted
            .inner
            .state_store
            .lock()
            .unwrap()
            .expire_exec_session_for_test("es_query_only", 1)
            .unwrap();
        restarted.prune();
        assert!(
            restarted
                .inner
                .state_store
                .lock()
                .unwrap()
                .get_unsettled_exec_session("es_query_only")
                .unwrap()
                .is_none(),
            "a bounded degraded row must leave after its TTL"
        );
        drop(restarted);
    }

    /// user operator | Given an unread pid-less row with a committed spool and
    /// an active pane row without one | When the registry restarts and prunes
    /// after the TTL | Then only the completed row is bounded and removed.
    #[test]
    fn user_operator_expires_unread_completed_pidless_row_without_reading_it() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-unread-ttl-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let store = StateStore::open_in_dir(&path, "state.db").unwrap();
        let now = now_ms();
        store
            .record_pane_exec_running("es_unread_done", now)
            .unwrap();
        store
            .record_pane_exec_running("es_active_pane", now)
            .unwrap();
        drop(store);
        let completed = unpersisted_closed_session("es_unread_done", "unread-output\n");
        write_session_spool(&path, &completed).unwrap();

        let registry = ExecRegistry::new(path.clone()).unwrap();
        let bounded = registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .get_unsettled_exec_session("es_unread_done")
            .unwrap()
            .expect("startup bounds the committed pidless row");
        assert!(bounded.expires_at_ms.is_some());
        let active = registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .get_unsettled_exec_session("es_active_pane")
            .unwrap()
            .expect("an active pane row stays running");
        assert_eq!(active.expires_at_ms, None);
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .expire_exec_session_for_test("es_unread_done", 1)
            .unwrap();
        registry.prune();
        assert!(
            registry
                .inner
                .state_store
                .lock()
                .unwrap()
                .get_unsettled_exec_session("es_unread_done")
                .unwrap()
                .is_none(),
            "the unread completed row expires without a session read"
        );
        assert!(
            registry
                .inner
                .state_store
                .lock()
                .unwrap()
                .get_unsettled_exec_session("es_active_pane")
                .unwrap()
                .is_some(),
            "the active pane row is not removed"
        );
        drop(registry);
        let _ = fs::remove_dir_all(&path);
    }

    /// user operator | Given a real native command whose spool committed but
    /// both state updates failed | When the dead process is recovered after
    /// restart | Then the committed spool is readable and no exit code is
    /// invented.
    #[cfg(unix)]
    #[test]
    fn user_operator_reads_committed_native_spool_after_restart_state_writes_fail() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-native-restart-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        let cwd = path.join("cwd");
        fs::create_dir_all(&cwd).unwrap();
        let registry = ExecRegistry::new(path.clone()).unwrap();
        let started = registry.start(&cwd, "printf native-saved").unwrap();
        let session_id = started["session_id"].as_str().unwrap().to_owned();
        let mut completed = false;
        for _ in 0..100 {
            let view = registry.read(&session_id, "stdout", 0, 64);
            if view["phase"] == "completed" {
                completed = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(completed, "the native command must finish before restart");
        let native_pid = registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .exec_session_pid_for_test(&session_id)
            .unwrap()
            .expect("native session records a pid");
        assert_ne!(native_pid, 0);
        drop(registry);
        fs::remove_dir_all(path.join("exec-spools")).unwrap();
        let registry = ExecRegistry::new(path.clone()).unwrap();
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .record_exec_running("es_native_blocked", native_pid, Some(native_pid), now_ms())
            .unwrap();
        registry
            .inner
            .state_store
            .lock()
            .unwrap()
            .fail_exec_state_writes_for_test();
        let session = unpersisted_closed_session("es_native_blocked", "native-saved");
        write_session_spool(&path, &session).unwrap();
        let spool = fs::read(exec_spool_path(&path, "es_native_blocked")).unwrap();
        persist_closed_evidence(&registry.inner, &session, now_ms(), Some(0), None);
        assert!(!session.status.lock().unwrap().persist_abandoned);
        let session_id = "es_native_blocked".to_owned();
        drop(session);
        drop(registry);

        let restarted = ExecRegistry::new(path.clone()).unwrap();
        let booted = restarted.read(&session_id, "stdout", 0, 64);
        assert_eq!(booted["ok"], true, "{booted}");
        assert_eq!(booted["text"], "native-saved");
        assert_eq!(booted["phase"], "completed");
        assert_ne!(
            restarted
                .inner
                .recovered
                .lock()
                .unwrap()
                .get(&session_id)
                .map(String::as_str),
            Some("closed_before_restart"),
            "a committed spool must be readable instead of only a recovery refusal"
        );
        assert_eq!(
            fs::read(exec_spool_path(&path, &session_id)).unwrap(),
            spool
        );
        drop(restarted);
        let _ = fs::remove_dir_all(&path);
    }

    /// user operator | Given one stream is a real truncated tail and the other
    /// declares bytes it did not store | When the spool is read | Then it is
    /// session_spool_corrupt while a real empty capture stays successful.
    #[test]
    fn user_operator_rejects_a_short_stream_hidden_by_the_other_streams_truncation() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-mixed-truncation-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let store = StateStore::open_in_dir(&path, "state.db").unwrap();
        let now = now_ms();
        for id in ["es_mixed", "es_real_zero"] {
            store.record_exec_running(id, 22_000, None, now).unwrap();
            store
                .settle_exec_session(
                    id,
                    "closed",
                    Some(now + 4),
                    Some(0),
                    None,
                    now + SESSION_TTL_MS,
                )
                .unwrap();
        }
        drop(store);
        fs::create_dir_all(exec_spool_dir(&path)).unwrap();
        let stderr_tail = "A".repeat(RECOVERY_SPOOL_CAP);
        let stderr_b64 = BASE64.encode(stderr_tail.as_bytes());
        let mixed = format!(
            r#"{{"version":1,"session_id":"es_mixed","stdout_bytes":100,"stderr_bytes":{},"truncated":true,"stdout_tail":"","stderr_tail":"{stderr_b64}"}}"#,
            RECOVERY_SPOOL_CAP + 50
        );
        fs::write(exec_spool_path(&path, "es_mixed"), mixed).unwrap();
        let empty = unpersisted_closed_session("es_real_zero", "");
        empty.buffers.lock().unwrap().chunks.clear();
        empty.buffers.lock().unwrap().stdout_bytes = 0;
        empty.buffers.lock().unwrap().stderr_bytes = 0;
        write_session_spool(&path, &empty).unwrap();

        let registry = ExecRegistry::new(path.clone()).unwrap();
        let mixed_view = registry.read("es_mixed", "both", 0, 64);
        assert_eq!(mixed_view["ok"], false, "{mixed_view}");
        assert_eq!(mixed_view["reason"], "session_spool_corrupt");
        let zero = registry.read("es_real_zero", "both", 0, 64);
        assert_eq!(zero["ok"], true, "{zero}");
        assert_eq!(zero["phase"], "completed");
        assert_eq!(zero["text"], "");
        drop(registry);
        let _ = fs::remove_dir_all(&path);
    }

    #[test]
    fn settled_row_without_spool_reports_incomplete_evidence() {
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-row-no-spool-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        // A `closed` durable row with no `.spool` — the crash window between
        // settle and spool rename, or a permanently failed spool write.
        let store = StateStore::open_in_dir(&path, "state.db").unwrap();
        let now = now_ms();
        store
            .record_exec_running("es_row_only", 42_000, None, now)
            .unwrap();
        store
            .settle_exec_session(
                "es_row_only",
                "closed",
                Some(now + 10),
                Some(3),
                None,
                now + SESSION_TTL_MS,
            )
            .unwrap();
        drop(store);

        let registry = ExecRegistry::new(path.clone()).unwrap();
        let view = registry.read("es_row_only", "both", 0, 64);
        assert_eq!(view["ok"], false);
        assert_eq!(view["reason"], "session_evidence_incomplete");
        assert_eq!(view["recovered"], true);

        // A second read must not observe a cached empty projection.
        let again = registry.read("es_row_only", "both", 0, 64);
        assert_eq!(again["ok"], false);
        assert_eq!(again["reason"], "session_evidence_incomplete");

        // Boot-time load of the same row-only state must refuse identically.
        drop(registry);
        let restarted = ExecRegistry::new(path.clone()).unwrap();
        let booted = restarted.read("es_row_only", "both", 0, 64);
        assert_eq!(booted["ok"], false);
        assert_eq!(booted["reason"], "session_evidence_incomplete");

        // A spool that commits late is picked up by the next read.
        let session = unpersisted_closed_session("es_row_only", "late-output\n");
        write_session_spool(&path, &session).unwrap();
        let late = restarted.read("es_row_only", "stdout", 0, 64);
        assert_eq!(late["ok"], true);
        assert_eq!(late["phase"], "completed");
        assert_eq!(late["text"], "late-output\n");
        assert_eq!(late["exit_code"], 3);

        drop(restarted);
        let _ = fs::remove_dir_all(&path);
    }

    #[test]
    fn spool_written_before_settle_survives_crash_window() {
        // Crash between `persist_session_spool` and `settle_closed_evidence`:
        // the row stays `running`, the spool exists. Restart recovery settles
        // it as closed_before_restart — an honest recovery refusal, never a
        // completed session with empty output.
        let path = env::temp_dir().join(format!(
            "herdr-mcp-exec-spool-first-crash-{}-{}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let store = StateStore::open_in_dir(&path, "state.db").unwrap();
        let now = now_ms();
        store
            .record_exec_running("es_crash_mid", 99_000, None, now)
            .unwrap();
        let session = unpersisted_closed_session("es_crash_mid", "mid-crash-output\n");
        write_session_spool(&path, &session).unwrap();
        drop(store);

        let restarted = ExecRegistry::new(path.clone()).unwrap();
        let view = restarted.read("es_crash_mid", "both", 0, 64);
        assert_eq!(view["ok"], false);
        assert_eq!(view["reason"], "session_recovered_after_restart");
        assert_eq!(view["recovery_state"], "closed_before_restart");

        drop(restarted);
        let _ = fs::remove_dir_all(&path);
    }
}
