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

    if let Ok(bytes) = fs::read(&spool_path)
        && let Ok(artifact) = serde_json::from_slice::<SavedCompletionEvidence>(&bytes)
        && artifact.version == 1
        && artifact.session_id == record.session_id
    {
        let stdout_data = BASE64.decode(&artifact.stdout_tail).unwrap_or_default();
        let stderr_data = BASE64.decode(&artifact.stderr_tail).unwrap_or_default();
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
    })
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
                let _ = fs::remove_file(&path);
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
            initial_sessions.insert(record.session_id.clone(), session);
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
        self.start_in_workspace(cwd, command, None)
    }

    pub fn start_in_workspace(
        &self,
        cwd: &Path,
        command: &str,
        workspace_id: Option<&str>,
    ) -> Result<Value, String> {
        self.prune();
        if command.is_empty() {
            return Err("command must not be empty".to_owned());
        }
        if should_use_pane_backend(cwd, workspace_id, self.inner.client.as_ref()) {
            return self.start_pane(cwd, command, workspace_id.expect("checked above"));
        }
        self.start_native(cwd, command)
    }

    fn start_native(&self, cwd: &Path, command: &str) -> Result<Value, String> {
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
            pid: Some(pid),
            backend: SessionBackend::Native {
                child: Mutex::new(child),
                output_readers: AtomicUsize::new(output_readers),
            },
            buffers: Mutex::new(Buffers::default()),
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
            "backend": "native",
            "phase": "started",
            "progress": {
                "bytes_read": 0,
                "bytes_total": 0,
                "elapsed_ms": 0,
            },
        }))
    }

    fn start_pane(&self, cwd: &Path, command: &str, workspace_id: &str) -> Result<Value, String> {
        let client = self
            .inner
            .client
            .clone()
            .ok_or_else(|| "Herdr pane backend is unavailable".to_owned())?;
        let id = new_session_id();
        let pane = client
            .call_with_timeout(
                "pane.split",
                json!({
                    "workspace_id": workspace_id,
                    "direction": "right",
                    "cwd": cwd.to_string_lossy(),
                    "focus": false,
                }),
                PANE_RPC_TIMEOUT,
            )
            .map_err(|error| format!("cannot create Herdr exec pane: {error}"))?;
        let pane_id =
            extract_pane_id(&pane).ok_or_else(|| "pane.split returned no pane id".to_owned())?;
        let _ = client.call_with_timeout(
            "pane.rename",
            json!({
                "pane_id": pane_id,
                "label": format!("herdr-mcp:exec:{}", short_session_id(&id)),
            }),
            PANE_RPC_TIMEOUT,
        );
        let script_path = pane_script_path(&id);
        let spool = pane_spool_paths(&id);
        write_pane_script(&script_path, cwd, command, &spool)?;
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
            cleanup_pane_files(&script_path, &spool);
            let _ = client.call_with_timeout(
                "pane.close",
                json!({"pane_id": pane_id}),
                PANE_RPC_TIMEOUT,
            );
            return Err(format!("cannot start Herdr pane command: {error}"));
        }
        let started_at_ms = now_ms();
        let session = Arc::new(Session {
            id: id.clone(),
            cwd: cwd.to_path_buf(),
            command: command.to_owned(),
            started_at_ms,
            pid: None,
            backend: SessionBackend::Pane {
                client,
                pane_id: pane_id.clone(),
                script_path,
                spool,
                stdout_offset: Mutex::new(0),
                stderr_offset: Mutex::new(0),
            },
            buffers: Mutex::new(Buffers::default()),
            status: Mutex::new(SessionStatus::default()),
        });
        if let Err(error) = self
            .inner
            .state_store
            .lock()
            .map_err(|_| "exec state store lock poisoned".to_owned())
            .and_then(|store| store.record_pane_exec_running(&id, started_at_ms))
        {
            terminate_session(&session, true, None);
            return Err(format!(
                "cannot durably register pane exec session; pane closed before return: {error}"
            ));
        }
        self.inner
            .sessions
            .lock()
            .map_err(|_| "exec registry lock poisoned".to_owned())?
            .insert(id.clone(), Arc::clone(&session));
        spawn_monitor(Arc::clone(&session), Arc::downgrade(&self.inner));
        Ok(json!({
            "ok": true,
            "session_id": id,
            "cwd": cwd.to_string_lossy(),
            "command": command,
            "started_at": iso_from_ms(started_at_ms),
            "pid": Value::Null,
            "backend": "herdr_pane",
            "pane_id": pane_id,
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
        terminate_session(&session, false, Some(&self.inner));
        let weak = Arc::downgrade(&session);
        thread::spawn(move || {
            thread::sleep(KILL_GRACE);
            if let Some(session) = weak.upgrade()
                && !session_status(&session).closed
            {
                terminate_session(&session, true, None);
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
        let record = self
            .inner
            .state_store
            .lock()
            .ok()
            .and_then(|store| store.get_closed_exec_session(id).ok())
            .flatten()?;
        let session = load_session_from_spool_or_record(&self.inner.state_dir, &record);
        if let Ok(mut sessions) = self.inner.sessions.lock() {
            sessions.insert(id.to_owned(), Arc::clone(&session));
        }
        Some(session)
    }

    fn prune(&self) {
        let now = now_ms();
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

fn should_use_pane_backend(
    cwd: &Path,
    workspace_id: Option<&str>,
    client: Option<&HerdrClient>,
) -> bool {
    #[cfg(target_os = "macos")]
    {
        if workspace_id.is_none() || client.is_none() {
            return false;
        }
        let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
            return false;
        };
        is_protected_root_for_home(cwd, &home)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (cwd, workspace_id, client);
        false
    }
}

#[cfg(any(target_os = "macos", test))]
fn is_protected_root_for_home(cwd: &Path, home: &Path) -> bool {
    ["Documents", "Desktop", "Downloads"]
        .into_iter()
        .map(|name| home.join(name))
        .any(|protected| cwd.starts_with(protected))
}

fn extract_pane_id(value: &Value) -> Option<String> {
    let pane = value.get("pane").unwrap_or(value);
    pane.get("pane_id")
        .and_then(Value::as_str)
        .or_else(|| pane.get("id").and_then(Value::as_str))
        .map(str::to_owned)
}

fn marker_safe_id(id: &str) -> String {
    id.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn short_session_id(id: &str) -> String {
    id.chars()
        .rev()
        .take(10)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn pane_script_path(id: &str) -> PathBuf {
    env::temp_dir().join(format!("herdr-mcp-pane-exec-{}.sh", marker_safe_id(id)))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
    } = &session.backend
    else {
        return false;
    };
    drain_spool_file(session, &spool.stdout, StreamKind::Stdout, stdout_offset);
    drain_spool_file(session, &spool.stderr, StreamKind::Stderr, stderr_offset);
    let Some(exit_code) = read_pane_exit_code(&spool.status) else {
        return false;
    };
    let transitioned = complete_session(session, registry, Some(exit_code), None);
    if transitioned {
        cleanup_pane_files(script_path, spool);
        let _ =
            client.call_with_timeout("pane.close", json!({"pane_id": pane_id}), PANE_RPC_TIMEOUT);
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

fn terminate_session(session: &Arc<Session>, force: bool, registry: Option<&RegistryInner>) {
    match &session.backend {
        SessionBackend::Completed => {}
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
        }
        SessionBackend::Pane {
            client,
            pane_id,
            script_path,
            spool,
            ..
        } => {
            let _ = client.call_with_timeout(
                "pane.close",
                json!({"pane_id": pane_id}),
                PANE_RPC_TIMEOUT,
            );
            cleanup_pane_files(script_path, spool);
            complete_session(
                session,
                registry,
                None,
                Some(if force { "SIGKILL" } else { "SIGTERM" }),
            );
        }
    }
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

fn persist_closed_evidence(
    inner: &RegistryInner,
    session: &Arc<Session>,
    ended_at: u64,
    exit_code: Option<i32>,
    signal: Option<&str>,
) {
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
    if result.is_err() {
        inner.persistence_failures.fetch_add(1, Ordering::Relaxed);
    }
    if let Err(_error) = write_session_spool(&inner.state_dir, session) {
        inner.persistence_failures.fetch_add(1, Ordering::Relaxed);
    }
}

fn complete_session(
    session: &Arc<Session>,
    registry: Option<&RegistryInner>,
    exit_code: Option<i32>,
    signal: Option<&str>,
) -> bool {
    let Ok(mut status) = session.status.lock() else {
        return false;
    };
    if status.closed {
        return false;
    }
    let ended_at = now_ms();
    if let Some(registry) = registry {
        persist_closed_evidence(registry, session, ended_at, exit_code, signal);
    }
    status.closed = true;
    status.exit_code = exit_code;
    status.signal = signal.map(str::to_owned);
    status.ended_at_ms = Some(ended_at);
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

    #[cfg(unix)]
    fn pane_session(id: &str) -> (Arc<Session>, PathBuf) {
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
            },
            buffers: Mutex::new(Buffers::default()),
            status: Mutex::new(SessionStatus::default()),
        });
        (session, socket)
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
            pid: Some(pid),
            backend: SessionBackend::Native {
                child: Mutex::new(child),
                output_readers: AtomicUsize::new(output_readers),
            },
            buffers: Mutex::new(Buffers::default()),
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

    #[test]
    fn protected_root_detection_is_scoped_to_user_privacy_folders() {
        let home = Path::new("/Users/example");
        assert!(is_protected_root_for_home(
            Path::new("/Users/example/Documents/repo"),
            home
        ));
        assert!(is_protected_root_for_home(
            Path::new("/Users/example/Desktop/repo"),
            home
        ));
        assert!(is_protected_root_for_home(
            Path::new("/Users/example/Downloads/repo"),
            home
        ));
        assert!(!is_protected_root_for_home(
            Path::new("/Users/example/src/repo"),
            home
        ));
        assert!(!is_protected_root_for_home(
            Path::new("/Users/other/Documents/repo"),
            home
        ));
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
                }),
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
}
