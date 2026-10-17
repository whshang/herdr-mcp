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
    droid_sessions: Option<PathBuf>,
    grok_sessions: Option<PathBuf>,
    cursor_projects: Option<PathBuf>,
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
            omp_home: home.as_ref().map(|path| path.join(".omp")),
            droid_sessions: home.as_ref().map(|path| path.join(".factory/sessions")),
            grok_sessions: home.as_ref().map(|path| path.join(".grok/sessions")),
            cursor_projects: home.map(|path| path.join(".cursor/projects")),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Provider {
    Codex,
    Claude,
    Pi,
    Omp,
    Droid,
    Grok,
    CursorAgent,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude-code",
            Self::Pi => "pi",
            Self::Omp => "omp",
            Self::Droid => "droid",
            Self::Grok => "grok",
            Self::CursorAgent => "cursor-agent",
        }
    }
}

#[derive(Debug, Clone)]
struct LogFile {
    provider: Provider,
    path: PathBuf,
    mtime: Option<SystemTime>,
    /// Session cwd recoverable from the file's location rather than its
    /// content (grok percent-encodes it in a directory name; cursor-agent
    /// lossily encodes it into the project directory name).
    cwd_hint: Option<CwdHint>,
}

#[derive(Debug, Clone)]
enum CwdHint {
    /// A decoded absolute cwd: matched with the usual prefix rule.
    Path(PathBuf),
    /// cursor-agent's lossy `Users-x-Documents-proj` encoding: matched by
    /// encoding each managed root the same way and comparing for equality.
    EncodedName(String),
}

impl CwdHint {
    fn matching<'a>(&self, scopes: &'a [Scope]) -> Option<&'a Scope> {
        match self {
            Self::Path(cwd) => matching_scope(cwd, scopes),
            // `scopes` is sorted deepest-first, so the deepest managed root
            // wins. A session in a subdirectory encodes as `<root>-<sub>`.
            Self::EncodedName(name) => scopes.iter().find(|scope| {
                let root = cursor_project_name(&scope.match_root);
                *name == root || name.starts_with(&format!("{root}-"))
            }),
        }
    }
}

/// cursor-agent's project directory encoding: absolute path without the
/// leading slash, every remaining `/` replaced by `-`.
fn cursor_project_name(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches('/')
        .replace('/', "-")
}

/// Minimal percent-decoder for grok's URL-encoded cwd directory names.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = bytes.get(index + 1..index + 3)?;
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
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
    project_refs: &BTreeMap<PathBuf, String>,
    now_ms: i64,
) -> Result<usize, String> {
    ingest_from_dirs_with_refs(
        store,
        managed_roots,
        project_refs,
        now_ms,
        &LogDirs::discover(),
    )
}

#[cfg(test)]
fn ingest_from_dirs(
    store: &StateStore,
    managed_roots: &[PathBuf],
    now_ms: i64,
    dirs: &LogDirs,
) -> Result<usize, String> {
    ingest_from_dirs_with_refs(store, managed_roots, &BTreeMap::new(), now_ms, dirs)
}

/// `project_refs` maps a managed root to the same project_ref the tool_call
/// capture uses, so native-log events and MCP events share one work_state row.
fn ingest_from_dirs_with_refs(
    store: &StateStore,
    managed_roots: &[PathBuf],
    project_refs: &BTreeMap<PathBuf, String>,
    now_ms: i64,
    dirs: &LogDirs,
) -> Result<usize, String> {
    let scopes = scopes(managed_roots, project_refs);
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

fn scopes(roots: &[PathBuf], project_refs: &BTreeMap<PathBuf, String>) -> Vec<Scope> {
    let mut result = Vec::new();
    for root in roots {
        let display = root.to_string_lossy().into_owned();
        if result.iter().any(|scope: &Scope| scope.worktree == display) {
            continue;
        }
        result.push(Scope {
            project_ref: project_refs
                .get(root)
                .cloned()
                .unwrap_or_else(|| display.clone()),
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
    if let Some(root) = &dirs.droid_sessions {
        // Sessions live exactly one directory deep: sessions/<cwd>/<uuid>.jsonl.
        collect_jsonl(
            root,
            1,
            Provider::Droid,
            |relative| relative.components().count() == 2,
            &mut logs,
        );
    }
    if let Some(root) = &dirs.grok_sessions {
        collect_jsonl(
            root,
            2,
            Provider::Grok,
            |relative| {
                relative
                    .file_name()
                    .is_some_and(|name| name == "chat_history.jsonl")
            },
            &mut logs,
        );
        for log in logs
            .iter_mut()
            .filter(|log| matches!(log.provider, Provider::Grok))
        {
            if let Some(encoded) = log
                .path
                .strip_prefix(root)
                .ok()
                .and_then(|relative| relative.components().next())
                .and_then(|part| percent_decode(&part.as_os_str().to_string_lossy()))
            {
                log.cwd_hint = Some(CwdHint::Path(PathBuf::from(encoded)));
            }
        }
    }
    if let Some(root) = &dirs.cursor_projects {
        // Only transcripts: projects/<p>/agent-transcripts/[<uuid>/]<uuid>.jsonl.
        collect_jsonl(
            root,
            3,
            Provider::CursorAgent,
            |relative| {
                let mut parts = relative.components();
                parts.next().is_some()
                    && parts
                        .next()
                        .is_some_and(|part| part.as_os_str() == "agent-transcripts")
            },
            &mut logs,
        );
        for log in logs
            .iter_mut()
            .filter(|log| matches!(log.provider, Provider::CursorAgent))
        {
            if let Some(name) = log
                .path
                .strip_prefix(root)
                .ok()
                .and_then(|relative| relative.components().next())
            {
                log.cwd_hint = Some(CwdHint::EncodedName(
                    name.as_os_str().to_string_lossy().into_owned(),
                ));
            }
        }
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
                    cwd_hint: None,
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
            && log.cwd_hint.is_none()
            && let Some(cwd) = record_cwd(&value)
        {
            scope_key = matching_scope(&cwd, scopes)
                .map(|scope| (scope.project_ref.clone(), scope.worktree.clone()));
            if scope_key.is_none() {
                return Ok(FileOutcome::Unmanaged);
            }
        }
        if reset
            && scope_key.is_none()
            && let Some(hint) = &log.cwd_hint
        {
            // Providers whose logs carry no in-band cwd: resolve the path-
            // derived hint instead. A hint that matches no managed root
            // leaves the file unmanaged once the scan completes.
            scope_key = hint
                .matching(scopes)
                .map(|scope| (scope.project_ref.clone(), scope.worktree.clone()));
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
    match provider {
        Provider::Droid => parse_droid_events(value),
        Provider::Grok => parse_grok_events(value),
        Provider::CursorAgent => parse_cursor_events(value),
        Provider::Codex | Provider::Claude | Provider::Pi | Provider::Omp => {
            parse_generic_events(value)
        }
    }
}

fn parse_generic_events(value: &Value) -> Vec<ParsedEvent> {
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
            if let Some(text) = content
                .and_then(content_text)
                .and_then(|text| crate::work_capture::strip_handoff_context(&text))
            {
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
                let text = payload
                    .get("content")
                    .and_then(content_text)
                    .and_then(|text| {
                        if role == Some("user") {
                            crate::work_capture::strip_handoff_context(&text)
                        } else {
                            Some(text)
                        }
                    });
                if let Some(text) = text {
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

    events
}

/// Factory droid: `{"type":"message","message":{"role":..,"content":[..]}}`
/// plus a `session_start` first line whose `cwd` `record_cwd` already finds.
fn parse_droid_events(value: &Value) -> Vec<ParsedEvent> {
    let mut events = Vec::new();
    if value.get("type").and_then(Value::as_str) != Some("message") {
        return events;
    }
    let Some(message) = value.get("message") else {
        return events;
    };
    let Some(content) = message.get("content") else {
        return events;
    };
    let account = find_account(value);
    let ts = find_timestamp(value);
    match message.get("role").and_then(Value::as_str) {
        Some("user") => {
            // System reminders are injected user messages marked llm_only.
            let llm_only = [value, message]
                .into_iter()
                .any(|owner| owner.get("visibility").and_then(Value::as_str) == Some("llm_only"));
            // Empty content or tool_result-only content yields no text to log.
            if !llm_only
                && let Some(text) = content_text(content)
                    .and_then(|text| crate::work_capture::strip_handoff_context(&text))
            {
                events.push(parsed("user_prompt", text, &account, ts));
            }
        }
        Some("assistant") => {
            if let Some(text) = content_text(content) {
                events.push(parsed("assistant_summary", text, &account, ts));
            }
            collect_tool_events(content, &account, ts, &mut events);
        }
        _ => {}
    }
    events
}

/// grok CLI: flat `{"type":"user"|"assistant",...}` lines from
/// `<encoded-cwd>/<session-id>/chat_history.jsonl`.
fn parse_grok_events(value: &Value) -> Vec<ParsedEvent> {
    let mut events = Vec::new();
    let account = find_account(value);
    let ts = find_timestamp(value);
    match value.get("type").and_then(Value::as_str) {
        Some("user") => {
            // Only real typed turns carry prompt_index; synthetic reminders
            // and <user_info> bookkeeping lines never do.
            if value.get("prompt_index").is_none() || value.get("synthetic_reason").is_some() {
                return events;
            }
            let Some(text) = value.get("content").and_then(content_text) else {
                return events;
            };
            let text = extract_user_query(&text).unwrap_or(&text);
            if let Some(text) = crate::work_capture::strip_handoff_context(text) {
                events.push(parsed("user_prompt", text, &account, ts));
            }
        }
        Some("assistant") => {
            if let Some(text) = value.get("content").and_then(content_text) {
                events.push(parsed("assistant_summary", text, &account, ts));
            }
            if let Some(calls) = value.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let Some(name) = call.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    // `arguments` is a JSON string, not an object; an
                    // unparseable argument string still yields a named event.
                    let arguments = call
                        .get("arguments")
                        .and_then(Value::as_str)
                        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
                    let short = arguments
                        .as_ref()
                        .map(short_argument)
                        .filter(|text| !text.is_empty());
                    let summary =
                        short.map_or_else(|| name.to_owned(), |arg| format!("{name}: {arg}"));
                    events.push(parsed("tool_call", summary, &account, ts));
                }
            }
        }
        _ => {}
    }
    events
}

/// cursor-agent: `{"role":..,"message":{"content":[..]}}` lines; the cwd
/// comes from the project directory name, never from file content.
fn parse_cursor_events(value: &Value) -> Vec<ParsedEvent> {
    let mut events = Vec::new();
    let account = find_account(value);
    let ts = find_timestamp(value);
    let Some(message) = value.get("message") else {
        return events;
    };
    let Some(content) = message.get("content") else {
        return events;
    };
    match value.get("role").and_then(Value::as_str) {
        Some("user") => {
            let Some(text) = content_text(content) else {
                return events;
            };
            let text = extract_user_query(&text).unwrap_or(&text);
            if let Some(text) = crate::work_capture::strip_handoff_context(text) {
                events.push(parsed("user_prompt", text, &account, ts));
            }
        }
        Some("assistant") => {
            if let Some(text) = content_text(content) {
                events.push(parsed("assistant_summary", text, &account, ts));
            }
            collect_tool_events(content, &account, ts, &mut events);
        }
        _ => {}
    }
    events
}

/// Inner text of a `<user_query>...</user_query>` block when one exists.
fn extract_user_query(text: &str) -> Option<&str> {
    let after = text.find("<user_query>")? + "<user_query>".len();
    let before = text[after..].find("</user_query>")?;
    let inner = text[after..after + before].trim();
    (!inner.is_empty()).then_some(inner)
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
                    droid_sessions: Some(base.join("droid/sessions")),
                    grok_sessions: Some(base.join("grok/sessions")),
                    cursor_projects: Some(base.join("cursor/projects")),
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

    /// helper for grok directory names: percent-encode every byte.
    fn percent_encode(text: &str) -> String {
        text.bytes().map(|byte| format!("%{byte:02X}")).collect()
    }

    /// user droid transcript tail | Given a factory session log | When the
    /// store ingests it twice | Then scoped events appear once, reminders and
    /// non-transcript files are skipped, and handoff context is stripped.
    #[test]
    fn droid_logs_are_tailed_incrementally_with_scope_and_cleanup_rules() {
        let fixture = Fixture::new();
        let cwd = fixture.project.to_string_lossy();
        fixture.write(
            "droid/sessions/encoded-cwd/11111111-1111-1111-1111-111111111111.jsonl",
            &[
                serde_json::json!({"type":"session_start","id":"s1","title":"t","cwd":cwd}),
                serde_json::json!({"type":"message","id":"m1","timestamp":"2026-09-14T01:13:35.738Z","message":{"role":"user","content":[{"type":"text","text":"## Handoff context\n\nh\n\n---\n\ndroid real ask"}]}}),
                serde_json::json!({"type":"message","message":{"role":"user","visibility":"llm_only","content":[{"type":"text","text":"system reminder text"}]}}),
                serde_json::json!({"type":"message","visibility":"llm_only","message":{"role":"user","content":[{"type":"text","text":"top level reminder"}]}}),
                serde_json::json!({"type":"message","message":{"role":"user","content":[{"type":"tool_result","content":"tool output"}]}}),
                serde_json::json!({"type":"todo_state","items":["noise"]}),
                serde_json::json!({"type":"message","message":{"role":"assistant","content":[{"type":"thinking","thinking":"droid hidden"},{"type":"text","text":"droid answer"},{"type":"tool_use","id":"t1","name":"Execute","input":{"command":"git diff"}}]}}),
            ],
        );
        // Not a transcript: files outside <cwd>/<uuid>.jsonl or without the
        // .jsonl extension are never discovered.
        fixture.write(
            "droid/sessions/stray.jsonl",
            &[serde_json::json!({"type":"session_start","cwd":cwd})],
        );
        fixture.write(
            "droid/sessions/encoded-cwd/session.settings.jsonl-notes.txt",
            &[],
        );
        fixture.write(
            "droid/sessions/other-cwd/22222222-2222-2222-2222-222222222222.jsonl",
            &[
                serde_json::json!({"type":"session_start","id":"s2","cwd":fixture.base.join("other")}),
                serde_json::json!({"type":"message","message":{"role":"user","content":"droid foreign"}}),
            ],
        );
        // A garbage line is skipped without losing later lines in the file.
        let garbage = fixture
            .base
            .join("droid/sessions/encoded-cwd/33333333-3333-3333-3333-333333333333.jsonl");
        fs::write(
            &garbage,
            format!("not json\n{{\"type\":\"session_start\",\"cwd\":{cwd:?}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\",\"content\":\"droid after garbage\"}}}}\n"),
        )
        .unwrap();

        let store = StateStore::open(":memory:").unwrap();
        let roots = std::slice::from_ref(&fixture.project);
        assert_eq!(
            ingest_from_dirs(&store, roots, 1, &fixture.dirs).unwrap(),
            4
        );
        let rows = store.latest_work_events(&cwd, &cwd, 20).unwrap();
        assert!(rows.iter().all(|row| row.provider == "droid"));
        assert!(
            rows.iter()
                .any(|row| row.kind == "user_prompt" && row.summary == "droid real ask")
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == "assistant_summary" && row.summary == "droid answer")
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == "tool_call" && row.summary == "Execute: git diff")
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == "user_prompt" && row.summary == "droid after garbage")
        );
        for skipped in [
            "system reminder text",
            "top level reminder",
            "tool output",
            "droid hidden",
            "droid foreign",
        ] {
            assert!(
                rows.iter().all(|row| !row.summary.contains(skipped)),
                "unexpected event: {skipped}"
            );
        }
        // Incremental tail: no new bytes means no new rows.
        assert_eq!(
            ingest_from_dirs(&store, roots, 2, &fixture.dirs).unwrap(),
            0
        );
    }

    /// user grok transcript tail | Given a grok chat_history.jsonl | When the
    /// store ingests it twice | Then typed turns appear once, synthetic and
    /// non-chat files are skipped, and the cwd decodes from the directory.
    #[test]
    fn grok_logs_are_tailed_incrementally_with_scope_and_cleanup_rules() {
        let fixture = Fixture::new();
        let cwd = fixture.project.to_string_lossy();
        fixture.write(
            &format!(
                "grok/sessions/{}/sess-1/chat_history.jsonl",
                percent_encode(&cwd)
            ),
            &[
                serde_json::json!({"type":"system","content":"system banner"}),
                serde_json::json!({"type":"user","content":[{"type":"text","text":"<user_query>\n## Handoff context\n\nh\n\n---\n\ngrok real ask\n</user_query>"}],"prompt_index":0}),
                serde_json::json!({"type":"user","content":[{"type":"text","text":"<user_query>\nplain grok ask\n</user_query>"}],"prompt_index":1}),
                serde_json::json!({"type":"user","content":[{"type":"text","text":"<user_info>bookkeeping</user_info>"}]}),
                serde_json::json!({"type":"user","content":"synthetic reminder","prompt_index":2,"synthetic_reason":"system_reminder"}),
                serde_json::json!({"type":"reasoning","content":"grok hidden"}),
                serde_json::json!({"type":"assistant","content":"grok answer","tool_calls":[{"id":"c1","name":"run_terminal_command","arguments":"{\"command\":\"git status\"}"},{"id":"c2","name":"broken_args","arguments":"{not json"}]}),
                serde_json::json!({"type":"tool_result","content":"tool output"}),
            ],
        );
        // Sibling files in the session directory are not transcripts.
        fixture.write(
            &format!(
                "grok/sessions/{}/sess-1/updates.jsonl",
                percent_encode(&cwd)
            ),
            &[serde_json::json!({"type":"user","content":"updates noise","prompt_index":0})],
        );
        fixture.write(
            &format!(
                "grok/sessions/{}/sess-2/chat_history.jsonl",
                percent_encode(&fixture.base.join("other").to_string_lossy())
            ),
            &[serde_json::json!({"type":"user","content":"grok foreign","prompt_index":0})],
        );
        let garbage = fixture.base.join(format!(
            "grok/sessions/{}/sess-3/chat_history.jsonl",
            percent_encode(&cwd)
        ));
        fs::create_dir_all(garbage.parent().unwrap()).unwrap();
        fs::write(
            &garbage,
            "not json\n{\"type\":\"user\",\"content\":\"grok after garbage\",\"prompt_index\":0}\n",
        )
        .unwrap();

        let store = StateStore::open(":memory:").unwrap();
        let roots = std::slice::from_ref(&fixture.project);
        assert_eq!(
            ingest_from_dirs(&store, roots, 1, &fixture.dirs).unwrap(),
            6
        );
        let rows = store.latest_work_events(&cwd, &cwd, 20).unwrap();
        assert!(rows.iter().all(|row| row.provider == "grok"));
        assert!(
            rows.iter()
                .any(|row| row.kind == "user_prompt" && row.summary == "grok real ask")
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == "user_prompt" && row.summary == "plain grok ask")
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == "assistant_summary" && row.summary == "grok answer")
        );
        assert!(rows.iter().any(
            |row| row.kind == "tool_call" && row.summary == "run_terminal_command: git status"
        ));
        assert!(
            rows.iter()
                .any(|row| row.kind == "tool_call" && row.summary == "broken_args")
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == "user_prompt" && row.summary == "grok after garbage")
        );
        for skipped in [
            "system banner",
            "bookkeeping",
            "synthetic reminder",
            "grok hidden",
            "tool output",
            "updates noise",
            "grok foreign",
        ] {
            assert!(
                rows.iter().all(|row| !row.summary.contains(skipped)),
                "unexpected event: {skipped}"
            );
        }
        assert_eq!(
            ingest_from_dirs(&store, roots, 2, &fixture.dirs).unwrap(),
            0
        );
    }

    /// user cursor-agent transcript tail | Given agent-transcripts logs | When
    /// the store ingests them twice | Then events resolve scope by the encoded
    /// project name, prefer the deepest root, and non-transcript dirs and
    /// unmanaged projects are skipped.
    #[test]
    fn cursor_agent_logs_are_tailed_incrementally_with_scope_and_cleanup_rules() {
        let fixture = Fixture::new();
        let cwd = fixture.project.to_string_lossy();
        let nested = fixture.project.join("sub");
        fs::create_dir_all(&nested).unwrap();
        let nested_cwd = nested.to_string_lossy().into_owned();
        let encoded_project = cursor_project_name(&fs::canonicalize(&fixture.project).unwrap());
        let encoded_nested = cursor_project_name(&fs::canonicalize(&nested).unwrap());
        fixture.write(
            &format!("cursor/projects/{encoded_project}/agent-transcripts/uuid-1/uuid-1.jsonl"),
            &[
                serde_json::json!({"role":"user","message":{"content":[{"type":"text","text":"<timestamp>2026-09-14</timestamp>\n<user_query>\n## Handoff context\n\nh\n\n---\n\ncursor real ask\n</user_query>"}]}}),
                serde_json::json!({"role":"assistant","message":{"content":[{"type":"text","text":"cursor answer"},{"type":"tool_use","name":"Shell","input":{"command":"gh pr view 52","description":"show pr"}}]}}),
                serde_json::json!({"role":"user","message":{"content":[{"type":"text","text":"plain cursor ask"}]}}),
            ],
        );
        // A transcript directly under agent-transcripts is also accepted, and
        // the deepest matching managed root wins the encoded-name comparison.
        fixture.write(
            &format!("cursor/projects/{encoded_nested}/agent-transcripts/direct.jsonl"),
            &[serde_json::json!({"role":"user","message":{"content":"nested cursor ask"}})],
        );
        fixture.write(
            &format!("cursor/projects/{encoded_project}/other-dir/ignored.jsonl"),
            &[serde_json::json!({"role":"user","message":{"content":"non transcript noise"}})],
        );
        fixture.write(
            "cursor/projects/unmanaged-project/agent-transcripts/x.jsonl",
            &[serde_json::json!({"role":"user","message":{"content":"cursor foreign"}})],
        );
        let garbage = fixture.base.join(format!(
            "cursor/projects/{encoded_project}/agent-transcripts/uuid-2/uuid-2.jsonl"
        ));
        fs::create_dir_all(garbage.parent().unwrap()).unwrap();
        fs::write(
            &garbage,
            "not json\n{\"role\":\"user\",\"message\":{\"content\":\"cursor after garbage\"}}\n",
        )
        .unwrap();

        let store = StateStore::open(":memory:").unwrap();
        let roots = [fixture.project.clone(), nested];
        assert_eq!(
            ingest_from_dirs(&store, &roots, 1, &fixture.dirs).unwrap(),
            6
        );
        let rows = store.latest_work_events(&cwd, &cwd, 20).unwrap();
        assert!(rows.iter().all(|row| row.provider == "cursor-agent"));
        assert!(
            rows.iter()
                .any(|row| row.kind == "user_prompt" && row.summary == "cursor real ask")
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == "user_prompt" && row.summary == "plain cursor ask")
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == "assistant_summary" && row.summary == "cursor answer")
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == "tool_call" && row.summary == "Shell: gh pr view 52")
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == "user_prompt" && row.summary == "cursor after garbage")
        );
        assert!(
            rows.iter()
                .all(|row| !row.summary.contains("non transcript noise")
                    && !row.summary.contains("cursor foreign"))
        );
        let nested_rows = store
            .latest_work_events(&nested_cwd, &nested_cwd, 10)
            .unwrap();
        assert!(nested_rows.iter().any(|row| row.provider == "cursor-agent"
            && row.kind == "user_prompt"
            && row.summary == "nested cursor ask"));
        assert_eq!(
            store
                .latest_work_events(&cwd, &cwd, 30)
                .unwrap()
                .iter()
                .chain(&nested_rows)
                .filter(|row| row.provider == "cursor-agent")
                .count(),
            6
        );
        assert_eq!(
            ingest_from_dirs(&store, &roots, 2, &fixture.dirs).unwrap(),
            0
        );
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

#[cfg(test)]
mod strip_handoff_tailer_tests {
    use super::*;

    #[test]
    fn native_user_prompt_with_injected_handoff_is_stripped_or_skipped() {
        let with_task = serde_json::json!({"message": {"role": "user", "content": "## Handoff context\n\nh\n\n---\n\nreal ask"}});
        let events = parse_events(Provider::Pi, &with_task);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary, "real ask");
        let only = serde_json::json!({"message": {"role": "user", "content": "## Handoff context\n\nh\n\n---\n\n"}});
        assert!(parse_events(Provider::Pi, &only).is_empty());
    }
}
