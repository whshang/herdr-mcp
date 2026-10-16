//! Read-only incremental ingestion of native coding-agent session logs.

use crate::state_store::StateStore;
use crate::state_store::work_journal::{NewWorkEvent, WorkState, clip_chars};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::fs::{self, File, Metadata};
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const MAX_FILES: usize = 4_000;
const MAX_DEPTH: usize = 12;
const MAX_LINE_BYTES: u64 = 1024 * 1024;
const MAX_CONTEXT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CONTEXT_LINES: usize = 200;
const EVENT_TEXT_CHARS: usize = 200;

#[derive(Debug, Clone)]
struct LogDirs {
    codex_sessions: Option<PathBuf>,
    claude_projects: Option<PathBuf>,
    pi_sessions: Option<PathBuf>,
    omp_home: Option<PathBuf>,
}

impl LogDirs {
    fn discover() -> Self {
        let home = env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        let codex_home = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".codex")));
        Self {
            codex_sessions: codex_home.map(|path| path.join("sessions")),
            claude_projects: home.as_ref().map(|path| path.join(".claude/projects")),
            pi_sessions: home.as_ref().map(|path| path.join(".pi/agent/sessions")),
            omp_home: home.map(|path| path.join(".omp")),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Provider {
    Codex,
    Claude,
    Pi,
    Omp,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude-code",
            Self::Pi => "pi",
            Self::Omp => "omp",
        }
    }
}

#[derive(Debug, Clone)]
struct LogFile {
    provider: Provider,
    path: PathBuf,
    mtime: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    inode: u64,
    offset: u64,
    /// Hash of the file's first line: catches same-inode, non-shrinking
    /// rewrites that an offset-only cursor would never re-read. Absent from
    /// pre-existing cursor JSON, so old cursors decode to 0 and rescan once.
    #[serde(default)]
    first_line_hash: u64,
}

type ScopeKey = (String, String);

#[derive(Debug, Clone)]
struct Scope {
    project_ref: String,
    worktree: String,
    match_root: PathBuf,
}

#[derive(Debug)]
struct IngestedFile {
    scope: ScopeKey,
    cursor: Cursor,
    appended: usize,
}

#[derive(Debug)]
struct ParsedEvent {
    kind: &'static str,
    summary: String,
    account: Option<String>,
    ts: Option<i64>,
}

#[derive(Debug)]
enum FileOutcome {
    /// The file was scanned and its cursor advanced.
    Ingested(IngestedFile),
    /// The session cwd is outside every managed root: drop any stale cursor.
    Unmanaged,
    /// open/stat/seek/read failed (already logged): keep the previous cursor
    /// so the next scan retries the file.
    Unreadable,
}

/// Incrementally ingest supported native session logs below the current user's
/// agent homes. This function only reads those logs and writes journal rows and
/// cursors to `store`; scheduling belongs to the compaction lane.
pub(crate) fn ingest_native_logs(
    store: &StateStore,
    managed_roots: &[PathBuf],
    now_ms: i64,
) -> Result<usize, String> {
    ingest_from_dirs(store, managed_roots, now_ms, &LogDirs::discover())
}

fn ingest_from_dirs(
    store: &StateStore,
    managed_roots: &[PathBuf],
    now_ms: i64,
    dirs: &LogDirs,
) -> Result<usize, String> {
    let scopes = scopes(managed_roots);
    if scopes.is_empty() {
        return Ok(0);
    }

    let mut states = HashMap::<ScopeKey, WorkState>::new();
    for state in store.work_states()? {
        let key = (state.project_ref.clone(), state.worktree.clone());
        if scopes
            .iter()
            .any(|scope| key == (scope.project_ref.clone(), scope.worktree.clone()))
        {
            states.insert(key, state);
        }
    }

    let mut cursor_index = HashMap::<String, (ScopeKey, Cursor)>::new();
    for (key, state) in &states {
        for (path, cursor) in decode_cursors(&state.log_cursors_json) {
            cursor_index.insert(path, (key.clone(), cursor));
        }
    }

    let mut dirty = BTreeSet::<ScopeKey>::new();
    let mut appended = 0usize;
    for log in discover_logs(dirs) {
        let path_key = log.path.to_string_lossy().into_owned();
        let previous = cursor_index.get(&path_key).cloned();
        match ingest_file(store, &log, &path_key, previous.as_ref(), &scopes, now_ms) {
            Ok(FileOutcome::Ingested(result)) => {
                appended += result.appended;
                if let Some((old_scope, _)) = previous.as_ref()
                    && old_scope != &result.scope
                {
                    remove_cursor(&mut states, &mut dirty, old_scope, &path_key, now_ms);
                }
                put_cursor(
                    &mut states,
                    &mut dirty,
                    &result.scope,
                    &path_key,
                    result.cursor,
                    now_ms,
                );
            }
            Ok(FileOutcome::Unmanaged) => {
                // A replaced file may now belong to an unmanaged cwd. Forget
                // its old managed cursor so a later rewrite is inspected anew.
                if let Some((old_scope, _)) = previous.as_ref() {
                    remove_cursor(&mut states, &mut dirty, old_scope, &path_key, now_ms);
                }
            }
            Ok(FileOutcome::Unreadable) => {
                // Keep the stored cursor so the next scan retries this file.
            }
            Err(error) => {
                return Err(format!(
                    "cannot store events from {} log {}: {error}",
                    log.provider.name(),
                    log.path.display()
                ));
            }
        }
    }

    // Persist only scopes whose cursors changed, and only their cursor field,
    // so a concurrent compaction update of handoff_md/through_event_id is not
    // clobbered by the stale snapshot loaded above.
    for key in &dirty {
        let Some(incoming) = states.get(key) else {
            continue;
        };
        let mut row = store
            .work_state(&key.0, &key.1)?
            .unwrap_or_else(|| WorkState {
                project_ref: key.0.clone(),
                worktree: key.1.clone(),
                ..WorkState::default()
            });
        row.log_cursors_json = incoming.log_cursors_json.clone();
        row.updated_at = incoming.updated_at;
        store.put_work_state(&row)?;
    }
    Ok(appended)
}

fn scopes(roots: &[PathBuf]) -> Vec<Scope> {
    let mut result = Vec::new();
    for root in roots {
        let display = root.to_string_lossy().into_owned();
        if result
            .iter()
            .any(|scope: &Scope| scope.project_ref == display)
        {
            continue;
        }
        result.push(Scope {
            project_ref: display.clone(),
            worktree: display,
            match_root: fs::canonicalize(root).unwrap_or_else(|_| root.clone()),
        });
    }
    result.sort_by_key(|scope| std::cmp::Reverse(scope.match_root.components().count()));
    result
}

fn discover_logs(dirs: &LogDirs) -> Vec<LogFile> {
    let mut logs = Vec::new();
    if let Some(root) = &dirs.codex_sessions {
        collect_jsonl(root, MAX_DEPTH, Provider::Codex, |_| true, &mut logs);
    }
    if let Some(root) = &dirs.claude_projects {
        collect_jsonl(root, 1, Provider::Claude, |_| true, &mut logs);
    }
    if let Some(root) = &dirs.pi_sessions {
        collect_jsonl(root, MAX_DEPTH, Provider::Pi, |_| true, &mut logs);
    }
    if let Some(root) = &dirs.omp_home {
        collect_jsonl(
            root,
            MAX_DEPTH,
            Provider::Omp,
            |relative| {
                relative
                    .components()
                    .any(|part| part.as_os_str() == "sessions")
            },
            &mut logs,
        );
    }
    // Cap across ALL providers, keeping the newest by mtime, so one chatty
    // provider discovered earlier cannot starve the others.
    let discovered = logs.len();
    logs.sort_by(|left, right| {
        right
            .mtime
            .cmp(&left.mtime)
            .then_with(|| left.path.cmp(&right.path))
    });
    logs.truncate(MAX_FILES);
    if discovered > logs.len() {
        eprintln!(
            "work log discovery capped at {MAX_FILES} files; skipped {} older logs",
            discovered - logs.len()
        );
    }
    logs.sort_by(|left, right| left.path.cmp(&right.path));
    logs
}

fn collect_jsonl(
    root: &Path,
    max_depth: usize,
    provider: Provider,
    accept: impl Fn(&Path) -> bool,
    output: &mut Vec<LogFile>,
) {
    let mut queue = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = queue.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                eprintln!(
                    "work log discovery skipped {}: {error}",
                    directory.display()
                );
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    eprintln!(
                        "work log discovery skipped an entry under {}: {error}",
                        directory.display()
                    );
                    continue;
                }
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() && depth < max_depth {
                queue.push((path, depth + 1));
            } else if kind.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
                && path.strip_prefix(root).is_ok_and(&accept)
            {
                let mtime = entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok());
                output.push(LogFile {
                    provider,
                    path,
                    mtime,
                });
            }
        }
    }
}

/// Scan one log file from its stored cursor forward. `Err` is reserved for
/// journal-store failures; per-file I/O problems are logged and reported as
/// [`FileOutcome::Unreadable`], so one bad file never aborts the scan or
/// discards the cursors of files that already succeeded.
fn ingest_file(
    store: &StateStore,
    log: &LogFile,
    path_key: &str,
    previous: Option<&(ScopeKey, Cursor)>,
    scopes: &[Scope],
    now_ms: i64,
) -> Result<FileOutcome, String> {
    let unreadable = |error: String| {
        eprintln!(
            "work log skipped {} {}: {error}",
            log.provider.name(),
            log.path.display()
        );
        FileOutcome::Unreadable
    };
    let mut file = match File::open(&log.path) {
        Ok(file) => file,
        Err(error) => return Ok(unreadable(format!("cannot open log: {error}"))),
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return Ok(unreadable(format!("cannot inspect log: {error}"))),
    };
    let inode = file_inode(&metadata);
    let first_line_hash = match first_line_hash(&mut file) {
        Ok(hash) => hash,
        Err(error) => return Ok(unreadable(error)),
    };
    let previous_cursor = previous.map(|(_, cursor)| *cursor).unwrap_or_default();
    // Resume only when the file identity still matches the cursor: a changed
    // inode, a shrink, or a rewritten first line all mean earlier bytes may
    // have changed underneath the stored offset, so restart from the top.
    let reset = previous.is_none()
        || previous_cursor.inode != inode
        || metadata.len() < previous_cursor.offset
        || previous_cursor.first_line_hash != first_line_hash;
    let start = if reset { 0 } else { previous_cursor.offset };
    let session_account = if start == 0 {
        None
    } else {
        match scan_session_account(&mut file, start) {
            Ok(account) => account,
            Err(error) => return Ok(unreadable(error)),
        }
    };
    if let Err(error) = file.seek(SeekFrom::Start(start)) {
        return Ok(unreadable(format!("cannot seek log: {error}")));
    }

    let mut scope_key = if reset {
        None
    } else {
        previous.map(|(scope, _)| scope.clone())
    };
    let mut reader = std::io::BufReader::new(file);
    let mut offset = start;
    let mut appended = 0usize;
    let mut session_account = session_account;

    loop {
        let line_offset = offset;
        let Some((line, consumed)) = (match read_complete_line(&mut reader) {
            Ok(line) => line,
            Err(error) => return Ok(unreadable(error)),
        }) else {
            break;
        };
        offset += consumed;
        let value = match serde_json::from_slice::<Value>(&line) {
            Ok(value) => value,
            Err(error) => {
                eprintln!(
                    "work log line skipped {} {}@{line_offset}: {error}",
                    log.provider.name(),
                    log.path.display()
                );
                continue;
            }
        };

        if reset
            && scope_key.is_none()
            && let Some(cwd) = record_cwd(&value)
        {
            scope_key = matching_scope(&cwd, scopes)
                .map(|scope| (scope.project_ref.clone(), scope.worktree.clone()));
            if scope_key.is_none() {
                return Ok(FileOutcome::Unmanaged);
            }
        }
        let Some((project_ref, worktree)) = scope_key.as_ref() else {
            continue;
        };
        if let Some(account) = find_account(&value) {
            session_account = Some(account);
        }

        let line_hash = hash_bytes(&line);
        for (index, mut event) in parse_events(log.provider, &value).into_iter().enumerate() {
            if event.account.is_none() {
                event.account.clone_from(&session_account);
            }
            let summary = redact(&collapse(&event.summary));
            if summary.is_empty() {
                continue;
            }
            // The raw-line hash (and inode) keep the key honest when a line
            // is rewritten at a byte offset the journal has already seen.
            let dedupe = format!(
                "{}:{}:{inode}:{line_offset}:{index}:{line_hash:016x}",
                log.provider.name(),
                path_key
            );
            let event = NewWorkEvent {
                ts: event.ts.unwrap_or(now_ms),
                project_ref: project_ref.clone(),
                worktree: worktree.clone(),
                provider: log.provider.name().to_owned(),
                account: event.account.map(|account| redact(&account)),
                kind: event.kind.to_owned(),
                summary: clip_chars(&summary, EVENT_TEXT_CHARS),
                source_ref: format!("native-log:{path_key}"),
                dedupe_key: Some(dedupe),
            };
            if store.append_work_event(&event)?.is_some() {
                appended += 1;
            }
        }
    }

    Ok(match scope_key {
        Some(scope) => FileOutcome::Ingested(IngestedFile {
            scope,
            cursor: Cursor {
                inode,
                offset,
                first_line_hash,
            },
            appended,
        }),
        None => FileOutcome::Unmanaged,
    })
}

/// Hash of the first line of the file (bounded prefix when the first line is
/// oversized), the cheapest stable fingerprint of a file's identity.
fn first_line_hash(file: &mut File) -> Result<u64, String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("cannot seek log start: {error}"))?;
    let mut line = Vec::new();
    std::io::BufReader::new((&mut *file).take(MAX_LINE_BYTES + 1))
        .read_until(b'\n', &mut line)
        .map_err(|error| format!("cannot read first log line: {error}"))?;
    Ok(hash_bytes(&line))
}

/// FNV-1a 64-bit: stable across runs and platforms, no extra dependency.
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn scan_session_account(file: &mut File, before: u64) -> Result<Option<String>, String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("cannot seek log metadata: {error}"))?;
    let mut reader = std::io::BufReader::new(file);
    let mut consumed = 0u64;
    let mut account = None;
    for _ in 0..MAX_CONTEXT_LINES {
        if consumed >= before || consumed >= MAX_CONTEXT_BYTES {
            break;
        }
        let Some((line, read)) = read_complete_line(&mut reader)? else {
            break;
        };
        consumed += read;
        if let Ok(value) = serde_json::from_slice::<Value>(&line)
            && let Some(found) = find_account(&value)
        {
            account = Some(found);
        }
    }
    Ok(account)
}

/// Returns one newline-terminated line and its consumed byte count. A partial
/// final line remains behind the cursor. Oversized complete lines are consumed
/// and represented as an empty (therefore unparseable) line.
fn read_complete_line(reader: &mut impl BufRead) -> Result<Option<(Vec<u8>, u64)>, String> {
    let mut line = Vec::new();
    let read = reader
        .by_ref()
        .take(MAX_LINE_BYTES + 1)
        .read_until(b'\n', &mut line)
        .map_err(|error| format!("cannot read log: {error}"))?;
    if read == 0 {
        return Ok(None);
    }
    if line.ends_with(b"\n") {
        return Ok(Some((line, read as u64)));
    }
    if read as u64 <= MAX_LINE_BYTES {
        return Ok(None);
    }

    let mut consumed = read as u64;
    loop {
        let buffer = reader
            .fill_buf()
            .map_err(|error| format!("cannot drain oversized log line: {error}"))?;
        if buffer.is_empty() {
            return Ok(None);
        }
        if let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
            reader.consume(index + 1);
            consumed += (index + 1) as u64;
            return Ok(Some((Vec::new(), consumed)));
        }
        let length = buffer.len();
        reader.consume(length);
        consumed += length as u64;
    }
}

fn matching_scope<'a>(cwd: &Path, scopes: &'a [Scope]) -> Option<&'a Scope> {
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    scopes
        .iter()
        .find(|scope| cwd.starts_with(&scope.match_root))
}

fn record_cwd(value: &Value) -> Option<PathBuf> {
    string_at(value, &["cwd"])
        .or_else(|| string_at(value, &["payload", "cwd"]))
        .or_else(|| string_at(value, &["session", "cwd"]))
        .map(PathBuf::from)
}

fn parse_events(provider: Provider, value: &Value) -> Vec<ParsedEvent> {
    let account = find_account(value);
    let ts = find_timestamp(value);
    let message = value.get("message").unwrap_or(value);
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| value.get("type").and_then(Value::as_str));
    let content = message.get("content").or_else(|| value.get("content"));
    let mut events = Vec::new();

    match role {
        Some("user") | Some("human") => {
            if let Some(text) = content.and_then(content_text) {
                events.push(parsed("user_prompt", text, &account, ts));
            }
        }
        Some("assistant") => {
            if let Some(text) = content.and_then(content_text) {
                events.push(parsed("assistant_summary", text, &account, ts));
            }
            if let Some(content) = content {
                collect_tool_events(content, &account, ts, &mut events);
            }
        }
        _ => {}
    }

    // Codex rollout records keep role and content under payload.
    if value.get("type").and_then(Value::as_str) == Some("response_item") {
        let payload = &value["payload"];
        match payload.get("type").and_then(Value::as_str) {
            Some("message") if events.is_empty() => {
                let role = payload.get("role").and_then(Value::as_str);
                if let Some(text) = payload.get("content").and_then(content_text) {
                    let kind = if role == Some("user") {
                        "user_prompt"
                    } else if role == Some("assistant") {
                        "assistant_summary"
                    } else {
                        ""
                    };
                    if !kind.is_empty() {
                        events.push(parsed(kind, text, &account, ts));
                    }
                }
            }
            Some("function_call") | Some("custom_tool_call") | Some("tool_call") => {
                if let Some(event) = tool_event(payload, &account, ts) {
                    events.push(event);
                }
            }
            _ => {}
        }
    }

    if events.is_empty()
        && matches!(
            value.get("type").and_then(Value::as_str),
            Some("tool_use" | "tool_call" | "function_call")
        )
        && let Some(event) = tool_event(value, &account, ts)
    {
        events.push(event);
    }

    if let Some(error) = explicit_error(value, message) {
        events.push(parsed("error", error, &account, ts));
    }

    // OMP currently follows pi's session shape; keeping provider-specific
    // parsing selected here makes a future format divergence local.
    let _ = provider;
    events
}

fn parsed(
    kind: &'static str,
    summary: String,
    account: &Option<String>,
    ts: Option<i64>,
) -> ParsedEvent {
    ParsedEvent {
        kind,
        summary,
        account: account.clone(),
        ts,
    }
}

fn collect_tool_events(
    content: &Value,
    account: &Option<String>,
    ts: Option<i64>,
    output: &mut Vec<ParsedEvent>,
) {
    let Some(items) = content.as_array() else {
        return;
    };
    for item in items {
        if matches!(
            item.get("type").and_then(Value::as_str),
            Some("tool_use" | "toolCall" | "tool_call" | "function_call")
        ) && let Some(event) = tool_event(item, account, ts)
        {
            output.push(event);
        }
    }
}

fn tool_event(value: &Value, account: &Option<String>, ts: Option<i64>) -> Option<ParsedEvent> {
    let name = value
        .get("name")
        .or_else(|| value.get("tool_name"))
        .or_else(|| value.get("toolName"))
        .and_then(Value::as_str)?;
    let arguments = value
        .get("input")
        .or_else(|| value.get("arguments"))
        .or_else(|| value.get("args"));
    let short_arg = arguments
        .map(short_argument)
        .filter(|text| !text.is_empty());
    let summary = short_arg.map_or_else(|| name.to_owned(), |arg| format!("{name}: {arg}"));
    Some(parsed("tool_call", summary, account, ts))
}

fn short_argument(value: &Value) -> String {
    for key in [
        "command",
        "path",
        "query",
        "pattern",
        "url",
        "prompt",
        "file_path",
    ] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            return clip_chars(text, 120);
        }
    }
    clip_chars(&value.to_string(), 120)
}

fn explicit_error(value: &Value, message: &Value) -> Option<String> {
    for candidate in [
        message.get("errorMessage"),
        message.get("error"),
        value.get("errorMessage"),
        value.get("error"),
        value.pointer("/payload/error"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(text) = candidate.as_str().filter(|text| !text.trim().is_empty()) {
            return Some(text.to_owned());
        }
        if candidate.is_object() {
            return Some(candidate.to_string());
        }
    }
    // Only an explicit error marker qualifies: payloads such as Codex
    // event_msg agent_message rows carry a `message` field that is plain
    // user-visible text, never an error by itself.
    if value.pointer("/payload/type").and_then(Value::as_str) == Some("error") {
        return Some(
            value
                .pointer("/payload/message")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .unwrap_or("agent reported an error")
                .to_owned(),
        );
    }
    let failed = message.get("stopReason").and_then(Value::as_str) == Some("error")
        || value.get("is_error").and_then(Value::as_bool) == Some(true)
        || value.get("isError").and_then(Value::as_bool) == Some(true);
    failed.then(|| "agent reported an error".to_owned())
}

fn content_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return (!text.trim().is_empty()).then(|| text.to_owned());
    }
    let texts = content
        .as_array()?
        .iter()
        .filter(|item| {
            matches!(
                item.get("type").and_then(Value::as_str),
                Some("text" | "input_text" | "output_text")
            )
        })
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>();
    (!texts.is_empty()).then(|| texts.join(" "))
}

fn find_account(value: &Value) -> Option<String> {
    for container in [Some(value), value.get("payload"), value.get("message")]
        .into_iter()
        .flatten()
    {
        for key in ["account", "account_id", "accountId", "email"] {
            if let Some(text) = container.get(key).and_then(Value::as_str) {
                return Some(text.to_owned());
            }
        }
    }
    None
}

fn find_timestamp(value: &Value) -> Option<i64> {
    for candidate in [
        value.get("timestamp"),
        value.get("ts"),
        value.pointer("/payload/timestamp"),
        value.pointer("/message/timestamp"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(number) = candidate.as_i64() {
            return Some(if number < 10_000_000_000 {
                number * 1000
            } else {
                number
            });
        }
        if let Some(text) = candidate.as_str()
            && let Ok(parsed) = OffsetDateTime::parse(text, &Rfc3339)
        {
            return Some(parsed.unix_timestamp_nanos() as i64 / 1_000_000);
        }
    }
    None
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(value, |current, key| current.get(key))?
        .as_str()
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One redaction implementation for every stored journal field: lane A's
/// `work_capture` redactor covers quoted assignments, bearer/provider
/// tokens, high-entropy strings, and URL userinfo.
fn redact(text: &str) -> String {
    crate::work_capture::redact(text)
}

fn decode_cursors(json: &str) -> BTreeMap<String, Cursor> {
    match serde_json::from_str(json) {
        Ok(cursors) => cursors,
        Err(error) => {
            eprintln!("work log cursors reset after invalid JSON: {error}");
            BTreeMap::new()
        }
    }
}

fn put_cursor(
    states: &mut HashMap<ScopeKey, WorkState>,
    dirty: &mut BTreeSet<ScopeKey>,
    key: &ScopeKey,
    path: &str,
    cursor: Cursor,
    now_ms: i64,
) {
    let state = states.entry(key.clone()).or_insert_with(|| WorkState {
        project_ref: key.0.clone(),
        worktree: key.1.clone(),
        log_cursors_json: "{}".to_owned(),
        updated_at: now_ms,
        ..WorkState::default()
    });
    let mut cursors = decode_cursors(&state.log_cursors_json);
    if cursors.insert(path.to_owned(), cursor) == Some(cursor) {
        return;
    }
    let Some(json) = encode_cursors(key, &cursors) else {
        return;
    };
    state.log_cursors_json = json;
    state.updated_at = now_ms;
    dirty.insert(key.clone());
}

fn remove_cursor(
    states: &mut HashMap<ScopeKey, WorkState>,
    dirty: &mut BTreeSet<ScopeKey>,
    key: &ScopeKey,
    path: &str,
    now_ms: i64,
) {
    let Some(state) = states.get_mut(key) else {
        return;
    };
    let mut cursors = decode_cursors(&state.log_cursors_json);
    if cursors.remove(path).is_none() {
        return;
    }
    let Some(json) = encode_cursors(key, &cursors) else {
        return;
    };
    state.log_cursors_json = json;
    state.updated_at = now_ms;
    dirty.insert(key.clone());
}

/// Serialize cursors without panicking in the ingest loop: on failure log
/// and keep the prior cursor row so the next scan simply retries.
fn encode_cursors(key: &ScopeKey, cursors: &BTreeMap<String, Cursor>) -> Option<String> {
    match serde_json::to_string(cursors) {
        Ok(json) => Some(json),
        Err(error) => {
            eprintln!(
                "work log cursors not persisted for {}:{}: {error}",
                key.0, key.1
            );
            None
        }
    }
}

#[cfg(unix)]
fn file_inode(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn file_inode(_metadata: &Metadata) -> u64 {
    // No stable file identity from stable std here (Windows file_index is
    // unstable): the len and first-line-hash
    // resets still catch truncation and rewrites.
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::UNIX_EPOCH;

    struct Fixture {
        base: PathBuf,
        project: PathBuf,
        dirs: LogDirs,
    }

    impl Fixture {
        fn new() -> Self {
            let unique = UNIX_EPOCH.elapsed().unwrap().as_nanos();
            let base = env::temp_dir().join(format!(
                "herdr-work-tailers-{}-{unique}",
                std::process::id()
            ));
            let project = base.join("project");
            fs::create_dir_all(&project).unwrap();
            Self {
                project,
                dirs: LogDirs {
                    codex_sessions: Some(base.join("codex/sessions")),
                    claude_projects: Some(base.join("claude/projects")),
                    pi_sessions: Some(base.join("pi/sessions")),
                    omp_home: Some(base.join("omp")),
                },
                base,
            }
        }

        fn write(&self, relative: &str, lines: &[Value]) -> PathBuf {
            let path = self.base.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let body = lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            fs::write(&path, body).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn user_events_are_ingested_from_each_native_provider_and_incrementally_tailed() {
        let fixture = Fixture::new();
        let cwd = fixture.project.to_string_lossy();
        let codex = fixture.write(
            "codex/sessions/2026/a.jsonl",
            &[
                serde_json::json!({"type":"session_meta","payload":{"cwd":cwd,"account":"codex@example.test"}}),
                serde_json::json!({"type":"response_item","timestamp":"2026-01-01T00:00:00Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fix token=top-secret"}]}}),
                serde_json::json!({"type":"response_item","payload":{"type":"function_call","name":"bash","arguments":"{\"command\":\"cargo test\"}"}}),
            ],
        );
        fixture.write(
            "claude/projects/p/a.jsonl",
            &[
                serde_json::json!({"type":"user","cwd":cwd,"message":{"role":"user","content":"Claude prompt"}}),
                serde_json::json!({"type":"assistant","cwd":cwd,"message":{"role":"assistant","content":[{"type":"text","text":"Claude answer"},{"type":"tool_use","name":"Read","input":{"path":"src/main.rs"}}]}}),
            ],
        );
        fixture.write(
            "pi/sessions/p/a.jsonl",
            &[
                serde_json::json!({"type":"session","version":3,"id":"s1","timestamp":"2026-01-01T00:00:00Z","cwd":cwd}),
                serde_json::json!({"type":"turn_start"}),
                serde_json::json!({"type":"message_start","message":{"role":"user","content":[{"type":"text","text":"pi prompt"}]}}),
                serde_json::json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"streaming noise"}}),
                serde_json::json!({"type":"message_end","message":{"role":"assistant","timestamp":1767225600000_u64,"content":[{"type":"thinking","thinking":"hidden"},{"type":"text","text":"pi answer"},{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"src/lib.rs"}}],"stopReason":"toolUse"}}),
                serde_json::json!({"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"pi failed"}}),
            ],
        );
        fixture.write(
            "omp/profile/sessions/p/a.jsonl",
            &[
                serde_json::json!({"type":"session","cwd":cwd}),
                serde_json::json!({"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"omp answer"}]}}),
            ],
        );

        let store = StateStore::open(":memory:").unwrap();
        assert_eq!(
            ingest_from_dirs(
                &store,
                std::slice::from_ref(&fixture.project),
                10,
                &fixture.dirs
            )
            .unwrap(),
            10
        );
        let rows = store.latest_work_events(&cwd, &cwd, 30).unwrap();
        assert_eq!(rows.len(), 10);
        assert!(rows.iter().any(|row| row.provider == "codex"));
        assert!(
            rows.iter()
                .any(|row| row.kind == "tool_call" && row.summary.contains("cargo test"))
        );
        assert!(rows.iter().any(|row| row.provider == "claude-code"));
        assert!(rows.iter().any(|row| row.provider == "pi"
            && row.kind == "tool_call"
            && row.summary.contains("read: src/lib.rs")));
        assert!(
            rows.iter()
                .any(|row| row.provider == "pi" && row.kind == "error")
        );
        assert!(!rows.iter().any(|row| row.summary.contains("hidden")));
        assert!(rows.iter().any(|row| row.provider == "omp"));
        assert!(!rows.iter().any(|row| row.summary.contains("top-secret")));
        assert_eq!(
            redact("use sk-proj-abcdefghijklmnop and bearer abc.def"),
            "use [REDACTED] and [REDACTED]"
        );
        assert!(
            rows.iter()
                .all(|row| row.summary.chars().count() <= EVENT_TEXT_CHARS)
        );

        assert_eq!(
            ingest_from_dirs(
                &store,
                std::slice::from_ref(&fixture.project),
                20,
                &fixture.dirs
            )
            .unwrap(),
            0
        );
        let mut file = fs::OpenOptions::new().append(true).open(codex).unwrap();
        write!(file, "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"new answer\"}}]}}}}\n{{\"type\":\"message_start\",\"message\":{{\"role\":\"user\",\"content\":\"deferred part").unwrap();
        // The complete line lands; the partial line stays behind the cursor.
        assert_eq!(
            ingest_from_dirs(
                &store,
                std::slice::from_ref(&fixture.project),
                30,
                &fixture.dirs
            )
            .unwrap(),
            1
        );
        assert_eq!(
            store.latest_work_events(&cwd, &cwd, 1).unwrap()[0]
                .account
                .as_deref(),
            Some("codex@example.test")
        );
        // Completing the partial line turns it into a real ingested event.
        file.write_all(b"ial line\"}}\n").unwrap();
        assert_eq!(
            ingest_from_dirs(
                &store,
                std::slice::from_ref(&fixture.project),
                40,
                &fixture.dirs
            )
            .unwrap(),
            1
        );
        assert_eq!(
            store.latest_work_events(&cwd, &cwd, 1).unwrap()[0].summary,
            "deferred partial line"
        );
    }

    #[test]
    fn cursor_resets_for_truncation_or_replacement_and_skips_unmanaged_and_bad_lines() {
        let fixture = Fixture::new();
        let cwd = fixture.project.to_string_lossy();
        let path = fixture.write(
            "pi/sessions/p/reset.jsonl",
            &[
                serde_json::json!({"type":"session","cwd":cwd}),
                serde_json::json!({"type":"message","message":{"role":"user","content":format!("first {}", "padding ".repeat(40))}}),
            ],
        );
        fixture.write(
            "claude/projects/p/foreign.jsonl",
            &[serde_json::json!({"type":"user","cwd":fixture.base.join("other"),"message":{"role":"user","content":"foreign"}})],
        );
        let garbage = fixture.base.join("omp/x/sessions/p/bad.jsonl");
        fs::create_dir_all(garbage.parent().unwrap()).unwrap();
        fs::write(
            &garbage,
            format!("not json\n{{\"type\":\"session\",\"cwd\":{cwd:?}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"after bad line\"}}}}\n"),
        )
        .unwrap();

        let store = StateStore::open(":memory:").unwrap();
        // The unparseable line is skipped and the valid event after it in the
        // same file is still ingested; the foreign-cwd file is ignored.
        assert_eq!(
            ingest_from_dirs(
                &store,
                std::slice::from_ref(&fixture.project),
                1,
                &fixture.dirs
            )
            .unwrap(),
            2
        );

        // Shrink below the cursor: the new header is rescanned and its event is
        // accepted at the reused byte offset with a new inode/offset generation.
        fs::write(
            &path,
            format!(
                "{{\"type\":\"session\",\"cwd\":{cwd:?}}}\n\n{{\"type\":\"message_start\",\"message\":{{\"role\":\"user\",\"content\":\"after truncate\"}}}}\n"
            ),
        )
        .unwrap();
        assert_eq!(
            ingest_from_dirs(
                &store,
                std::slice::from_ref(&fixture.project),
                2,
                &fixture.dirs
            )
            .unwrap(),
            1
        );

        #[cfg(unix)]
        {
            let replacement = path.with_extension("new");
            fs::write(
                &replacement,
                format!(
                    "{{\"type\":\"session\",\"cwd\":{cwd:?}}}\n\n\n{{\"type\":\"message_start\",\"message\":{{\"role\":\"user\",\"content\":\"after replace\"}}}}\n"
                ),
            )
            .unwrap();
            fs::rename(replacement, &path).unwrap();
            assert_eq!(
                ingest_from_dirs(
                    &store,
                    std::slice::from_ref(&fixture.project),
                    3,
                    &fixture.dirs
                )
                .unwrap(),
                1
            );
        }

        let rows = store.latest_work_events(&cwd, &cwd, 20).unwrap();
        assert!(rows.iter().any(|row| row.summary == "after truncate"));
        #[cfg(unix)]
        assert!(rows.iter().any(|row| row.summary == "after replace"));
        assert!(rows.iter().any(|row| row.summary == "after bad line"));
        assert!(!rows.iter().any(|row| row.summary == "foreign"));
    }

    /// user rewritten line at a seen offset | Given a file whose cursor
    /// already advanced | When one line is replaced with same-length content
    /// (same inode, no shrink) | Then the new summary is stored.
    #[test]
    fn rewritten_line_at_a_seen_offset_is_reingested_instead_of_deduped_forever() {
        let fixture = Fixture::new();
        let cwd = fixture.project.to_string_lossy();
        let line = |text: String| serde_json::json!({"type":"user","cwd":cwd,"message":{"role":"user","content":text}});
        let path = fixture.write(
            "claude/projects/p/rewritten.jsonl",
            &[
                line(format!("alpha {}", "x".repeat(50))),
                line("stable second line".to_owned()),
            ],
        );
        let store = StateStore::open(":memory:").unwrap();
        let roots = std::slice::from_ref(&fixture.project);
        assert_eq!(
            ingest_from_dirs(&store, roots, 1, &fixture.dirs).unwrap(),
            2
        );

        // Replace line 1 in place: identical length and inode. An offset-only
        // cursor would never re-read it; the first-line hash reset must.
        let replacement = format!(
            "{}\n{}\n",
            line(format!("beta  {}", "y".repeat(50))),
            line("stable second line".to_owned())
        );
        assert_eq!(replacement.len() as u64, fs::metadata(&path).unwrap().len());
        fs::write(&path, replacement).unwrap();
        assert_eq!(
            ingest_from_dirs(&store, roots, 2, &fixture.dirs).unwrap(),
            1
        );
        let rows = store.latest_work_events(&cwd, &cwd, 10).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|row| row.summary.starts_with("beta")));
    }

    /// user chat rows are not errors | Given a Codex event_msg agent_message
    /// line | When the file is ingested | Then no error event is stored.
    #[test]
    fn codex_agent_and_user_event_messages_are_not_errors() {
        let fixture = Fixture::new();
        let cwd = fixture.project.to_string_lossy();
        fixture.write(
            "codex/sessions/2026/reply.jsonl",
            &[
                serde_json::json!({"type":"session_meta","payload":{"cwd":cwd}}),
                serde_json::json!({"type":"event_msg","payload":{"type":"agent_message","message":"ok"}}),
                serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":"hi"}}),
            ],
        );
        let store = StateStore::open(":memory:").unwrap();
        ingest_from_dirs(
            &store,
            std::slice::from_ref(&fixture.project),
            1,
            &fixture.dirs,
        )
        .unwrap();
        let rows = store.latest_work_events(&cwd, &cwd, 10).unwrap();
        assert!(!rows.iter().any(|row| row.kind == "error"), "{rows:?}");
    }

    /// user secrets in native logs | Given a log line carrying quoted,
    /// hyphenated-key and URL-userinfo secrets | When the event is stored |
    /// Then none of the four inputs survive in summary or account.
    #[test]
    fn user_native_log_secrets_never_reach_the_stored_summary_or_account() {
        let fixture = Fixture::new();
        let cwd = fixture.project.to_string_lossy();
        fixture.write(
            "pi/sessions/p/secrets.jsonl",
            &[
                serde_json::json!({"type":"session","cwd":cwd,"account":"worker sk-ant-api03-AbCdEfGhIjKlMnOpQr"}),
                serde_json::json!({"type":"message_start","message":{"role":"user","content":"set password: \"hello world\" then sk-proj-AbCdEfGhIjKlMnOpQrSt via https://ci-user:pw-hunter@example.invalid/repo"}}),
            ],
        );
        let store = StateStore::open(":memory:").unwrap();
        assert_eq!(
            ingest_from_dirs(
                &store,
                std::slice::from_ref(&fixture.project),
                1,
                &fixture.dirs
            )
            .unwrap(),
            1
        );
        let rows = store.latest_work_events(&cwd, &cwd, 10).unwrap();
        for secret in [
            "hello world",
            "sk-proj-AbCdEfGhIjKlMnOpQrSt",
            "sk-ant-api03-AbCdEfGhIjKlMnOpQr",
            "pw-hunter",
        ] {
            assert!(
                rows.iter().all(|row| !row.summary.contains(secret)),
                "secret leaked into summary: {secret}"
            );
            assert!(
                rows.iter().all(|row| {
                    row.account
                        .as_deref()
                        .is_none_or(|account| !account.contains(secret))
                }),
                "secret leaked into account: {secret}"
            );
        }
        assert_eq!(
            rows[0].account.as_deref(),
            Some("worker [REDACTED]"),
            "{rows:?}"
        );
    }
}
