//! Local Codex conversation history for one project directory.
//!
//! [`LIST_METHOD`], [`READ_METHOD`], and [`RESUME_METHOD`] read only
//! `$CODEX_HOME/sessions/**` and `$CODEX_HOME/archived_sessions/*` (default
//! `~/.codex`), so rollout files stay the single source of truth. Session
//! identity comes from a parsed `session_meta` payload and never a file name,
//! lists are scoped to one exactly validated project root. Read/resume may
//! omit `session_id`; selection then prefers a live session in that exact
//! directory and chooses the most recently modified candidate. Every read is
//! bounded and returns only real user/assistant conversation. Resume validates
//! selection, passes the global mutation gate, then starts `kind="codex"` with
//! `args=["resume", "<session id>"]`.

use crate::codex_paths::same_directory;
use crate::herdr::HerdrClient;
use serde_json::{Map, Value, json};
use std::env;
use std::fs::File;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const LIST_METHOD: &str = "herdr_mcp.codex_session.list";
pub const READ_METHOD: &str = "herdr_mcp.codex_session.read";
pub const RESUME_METHOD: &str = "herdr_mcp.codex_session.resume";

/// A single JSONL record (`session_meta` carries `base_instructions`).
const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_SCANNED_FILES: usize = 4000;
const SESSIONS_DEPTH: usize = 6;
const MAX_META_RECORDS: usize = 200;
const MAX_META_BYTES: usize = 4 * 1024 * 1024;
const MAX_SESSION_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CONVERSATION_RECORDS: usize = 20_000;
const MAX_CONVERSATION_BYTES: usize = 8 * 1024 * 1024;
const MAX_SESSION_ID_CHARS: usize = 128;
const MAX_PROJECT_ROOT_CHARS: usize = 4096;
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;
const TITLE_CHARS: usize = 160;
const DEFAULT_MESSAGE_CHARS: usize = 2000;
const MAX_MESSAGE_CHARS: usize = 20_000;
const DEFAULT_STARTUP_MS: u64 = 30_000;
const MIN_STARTUP_MS: u64 = 5_000;
const MAX_STARTUP_MS: u64 = 55_000;

/// Injected user-role internal context: excluded from previews and reads.
const INTERNAL_PREFIXES: &str = concat!(
    "<environment_context|<user_instructions|<recommended_plugins|<user_action",
    "|<codex_internal_context|<codex_delegation|<multi_agent_mode|<INSTRUCTIONS>",
    "|# AGENTS.md instructions",
);

type Obj = Map<String, Value>;
type Session = (Value, PathBuf, PathBuf);

/// Routes the Codex history methods; `None` keeps the caller's dispatch chain.
pub fn call(client: &HerdrClient, method: &str, params: &Value, snapshot: &Value) -> Option<Value> {
    let result = match method {
        LIST_METHOD => list(params, snapshot),
        READ_METHOD => read(params, snapshot),
        RESUME_METHOD => resume(client, params, snapshot),
        _ => return None,
    };
    Some(match result {
        Ok(value) => value,
        Err(error) => error,
    })
}

pub fn list(params: &Value, snapshot: &Value) -> Result<Value, Value> {
    let object = parse(params)?;
    let root_raw = required_string(object, "project_root", MAX_PROJECT_ROOT_CHARS)?;
    let limit = bounded_usize(object, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?;
    let root = managed_root(snapshot, &root_raw)?;
    let files = collect(&codex_home()?);
    let mut sessions = Vec::new();
    for candidate in &files {
        if sessions.len() == limit {
            break;
        }
        if let Some((summary, cwd, _)) = inspect(candidate)
            && same_directory(&root, &cwd)
        {
            sessions.push(summary);
        }
    }
    let preferred_session_id = sessions
        .first()
        .and_then(|session| session["session_id"].as_str())
        .map(str::to_owned);
    Ok(json!({
        "ok": true,
        "source": "codex_local_history",
        "read_only": true,
        "project_root": root.to_string_lossy(),
        "selection_strategy": "project_root_exact_latest",
        "preferred_session_id": preferred_session_id,
        "count": sessions.len(),
        "limit": limit,
        "sessions": sessions,
    }))
}

pub fn read(params: &Value, snapshot: &Value) -> Result<Value, Value> {
    let object = parse(params)?;
    let root_raw = required_string(object, "project_root", MAX_PROJECT_ROOT_CHARS)?;
    let session_id = optional_string(object, "session_id", MAX_SESSION_ID_CHARS)?;
    let max_messages = bounded_usize(object, "max_messages", 50, 1, 200)?;
    let max_chars = bounded_usize(
        object,
        "max_chars",
        DEFAULT_MESSAGE_CHARS,
        1,
        MAX_MESSAGE_CHARS,
    )?;
    let root = managed_root(snapshot, &root_raw)?;
    let (session, path, strategy) = select_session(&codex_home()?, session_id.as_deref(), &root)?;
    let selection = selection_evidence(strategy, &session);
    let (messages, truncated) = conversation(&path, max_messages, max_chars);
    Ok(json!({
        "ok": true,
        "source": "codex_local_history",
        "read_only": true,
        "project_root": root.to_string_lossy(),
        "selection": selection,
        "session": session,
        "messages_truncated": truncated,
        "messages": messages,
    }))
}

pub fn resume(client: &HerdrClient, params: &Value, snapshot: &Value) -> Result<Value, Value> {
    let object = parse(params)?;
    let root_raw = required_string(object, "project_root", MAX_PROJECT_ROOT_CHARS)?;
    let session_id = optional_string(object, "session_id", MAX_SESSION_ID_CHARS)?;
    let pane_id = required_string(object, "pane_id", 256)?;
    let name = required_string(object, "name", 64)?;
    let timeout_ms = optional_u64(object, "timeout_ms", MIN_STARTUP_MS, MAX_STARTUP_MS)?;
    if !is_safe_agent_name(&name) {
        return Err(invalid_params("name must match [a-z][a-z0-9_-]{0,31}"));
    }
    let root = managed_root(snapshot, &root_raw)?;
    // Selection is project-scoped before the mutation gate, so a foreign
    // session id can never reach agent.start.
    let (session, _, strategy) = select_session(&codex_home()?, session_id.as_deref(), &root)?;
    let resolved_session_id = session["session_id"]
        .as_str()
        .ok_or_else(|| invalid_params("selected session is missing session_id"))?
        .to_owned();
    let selection = selection_evidence(strategy, &session);
    crate::mutation::check_global(RESUME_METHOD)?;

    let (start, budget) = start_request(&name, &pane_id, &resolved_session_id, timeout_ms);
    let outcome = client.call_with_timeout("agent.start", start.clone(), budget);
    let mut payload = json!({
        "ok": outcome.is_ok(),
        "source": "codex_local_history",
        "mutation": true,
        "project_root": root.to_string_lossy(),
        "selection": selection,
        "session": session,
        "start": start,
    });
    match outcome {
        Ok(result) => payload["result"] = result,
        Err(error) => {
            payload["method"] = json!(RESUME_METHOD);
            payload["code"] = json!(error.code);
            payload["message"] = json!(error.message);
            payload["hint"] =
                json!("verify with herdr_inspect before retrying: the start may already have run");
        }
    }
    Ok(payload)
}

/// Native `agent.start` request plus a socket budget above its startup timeout.
fn start_request(
    name: &str,
    pane_id: &str,
    session_id: &str,
    timeout_ms: Option<u64>,
) -> (Value, Duration) {
    let ms = timeout_ms.unwrap_or(DEFAULT_STARTUP_MS);
    let mut params = json!({"name": name, "kind": "codex", "pane_id": pane_id});
    params["args"] = json!(["resume", session_id]);
    params["timeout_ms"] = json!(ms);
    let budget = Duration::from_millis(ms).saturating_add(Duration::from_secs(4));
    (params, budget)
}

fn codex_home() -> Result<PathBuf, Value> {
    crate::codex_paths::resolve_codex_home(
        env::var_os("CODEX_HOME"),
        env::var_os("HOME"),
        env::var_os("USERPROFILE"),
    )
    .ok_or_else(|| invalid_params("cannot resolve CODEX_HOME or the platform home directory"))
}

struct Candidate {
    path: PathBuf,
    archived: bool,
    modified: SystemTime,
}

/// Newest-first rollout candidates from both roots. `DirEntry::file_type` never
/// follows symlinks, so a link cannot pull an unrelated file into scope.
fn collect(codex_home: &Path) -> Vec<Candidate> {
    let roots = [
        (codex_home.join("sessions"), SESSIONS_DEPTH, false),
        (codex_home.join("archived_sessions"), 0, true),
    ];
    let mut files = Vec::new();
    for (root, max_depth, archived) in roots {
        let mut queue = vec![(root, 0usize)];
        while let Some((directory, depth)) = queue.pop() {
            if files.len() >= MAX_SCANNED_FILES {
                break;
            }
            let entries = std::fs::read_dir(&directory)
                .into_iter()
                .flatten()
                .flatten();
            for entry in entries {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if kind.is_dir() {
                    if depth < max_depth {
                        queue.push((path, depth + 1));
                    }
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                let jsonl = path.extension().is_some_and(|ext| ext == "jsonl");
                if kind.is_file() && jsonl && metadata.len() <= MAX_SESSION_FILE_BYTES {
                    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
                    files.push(Candidate {
                        path,
                        archived,
                        modified,
                    });
                }
            }
        }
    }
    files.sort_by(|left, right| {
        left.archived
            .cmp(&right.archived)
            .then_with(|| right.modified.cmp(&left.modified))
            .then_with(|| left.path.cmp(&right.path))
    });
    files
}

/// Session identity, cwd, and first-user-message preview from one capped scan.
fn inspect(candidate: &Candidate) -> Option<Session> {
    let mut head: Option<(String, PathBuf, Option<String>)> = None;
    let mut title = None;
    scan(&candidate.path, MAX_META_RECORDS, MAX_META_BYTES, |value| {
        if head.is_none() {
            if value["type"] != "session_meta" {
                return true;
            }
            let payload = &value["payload"];
            let id = text_field(payload, "session_id").or_else(|| text_field(payload, "id"));
            let id = id.filter(|id| id.chars().count() <= MAX_SESSION_ID_CHARS);
            if let (Some(id), Some(cwd)) = (id, text_field(payload, "cwd")) {
                head = Some((id, PathBuf::from(cwd), text_field(payload, "timestamp")));
            }
            return true;
        }
        if title.is_none()
            && let Some((_, text)) = conversational_message(value)
        {
            title = Some(truncate_chars(&collapse(&text), TITLE_CHARS).0);
            return false;
        }
        true
    });
    let (session_id, cwd, timestamp) = head?;
    let elapsed = candidate.modified.duration_since(UNIX_EPOCH).ok();
    let summary = json!({
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "timestamp": timestamp,
        "title": title,
        "archived": candidate.archived,
        "modified_at_ms": elapsed.map_or(0, |value| value.as_millis() as u64),
    });
    Some((summary, cwd, candidate.path.clone()))
}

/// Locates one session by payload id; a session from another root is refused.
fn find_session(codex_home: &Path, session_id: &str, root: &Path) -> Result<Session, Value> {
    let mut mismatched = false;
    for candidate in &collect(codex_home) {
        let Some(session) = inspect(candidate) else {
            continue;
        };
        if session.0["session_id"] != session_id {
            continue;
        }
        if same_directory(root, &session.1) {
            return Ok(session);
        }
        mismatched = true;
    }
    if mismatched {
        return Err(json!({
            "ok": false,
            "code": "session_project_mismatch",
            "message": "the session belongs to a different project directory and was not used",
            "session_id": session_id,
            "project_root": root.to_string_lossy(),
        }));
    }
    Err(json!({
        "ok": false,
        "code": "session_not_found",
        "message": "no local Codex session with that id was found in the Codex history directories",
        "session_id": session_id,
        "project_root": root.to_string_lossy(),
    }))
}

fn select_session(
    codex_home: &Path,
    session_id: Option<&str>,
    root: &Path,
) -> Result<(Value, PathBuf, &'static str), Value> {
    if let Some(session_id) = session_id {
        let (session, _, path) = find_session(codex_home, session_id, root)?;
        return Ok((session, path, "explicit_session_id"));
    }
    for candidate in &collect(codex_home) {
        let Some((session, cwd, path)) = inspect(candidate) else {
            continue;
        };
        if same_directory(root, &cwd) {
            return Ok((session, path, "project_root_exact_latest"));
        }
    }
    Err(json!({
        "ok": false,
        "code": "project_session_not_found",
        "message": "no local Codex session belongs to the exact project directory",
        "project_root": root.to_string_lossy(),
    }))
}

fn selection_evidence(strategy: &str, session: &Value) -> Value {
    json!({
        "strategy": strategy,
        "project_root_match": "exact_canonical",
        "session_id": session["session_id"],
        "archived": session["archived"],
        "modified_at_ms": session["modified_at_ms"],
    })
}

fn conversation(path: &Path, max_messages: usize, max_chars: usize) -> (Vec<Value>, bool) {
    let mut messages = Vec::new();
    let mut capped = false;
    let stopped = scan(
        path,
        MAX_CONVERSATION_RECORDS,
        MAX_CONVERSATION_BYTES,
        |value| {
            let Some((role, text)) = conversational_message(value) else {
                return true;
            };
            if messages.len() == max_messages {
                capped = true;
                return false;
            }
            let (text, truncated) = truncate_chars(text.trim(), max_chars);
            messages.push(json!({"role": role, "text": text, "truncated": truncated}));
            true
        },
    );
    (messages, capped || stopped)
}

/// Streams JSONL records under a per-record, per-scan, and total byte cap;
/// an oversized record is skipped, and `true` means the scan stopped early.
fn scan(
    path: &Path,
    max_records: usize,
    max_bytes: usize,
    mut visit: impl FnMut(&Value) -> bool,
) -> bool {
    let Ok(file) = File::open(path) else {
        return true;
    };
    let mut reader = std::io::BufReader::new(file);
    let cap = MAX_RECORD_BYTES as u64 + 1;
    let mut line = Vec::new();
    let mut bytes = 0usize;
    for _ in 0..max_records {
        line.clear();
        let Ok(read) = reader.by_ref().take(cap).read_until(b'\n', &mut line) else {
            return true;
        };
        if read == 0 {
            return false;
        }
        bytes += read;
        if bytes > max_bytes || !line.ends_with(b"\n") {
            continue;
        }
        if let Ok(value) = serde_json::from_slice::<Value>(&line)
            && !visit(&value)
        {
            return true;
        }
    }
    true
}

/// A real conversational turn: user/assistant `response_item` messages only,
/// with injected user-role context wrappers and every other payload dropped.
fn conversational_message(value: &Value) -> Option<(&'static str, String)> {
    if value["type"] != "response_item" || value["payload"]["type"] != "message" {
        return None;
    }
    let payload = &value["payload"];
    let role = match payload["role"].as_str()? {
        "user" => "user",
        "assistant" => "assistant",
        _ => return None,
    };
    let parts = payload["content"]
        .as_array()?
        .iter()
        .filter(|item| item["type"] == "input_text" || item["type"] == "output_text")
        .filter_map(|item| item["text"].as_str())
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    let text = parts.join("\n");
    let injected = role == "user"
        && INTERNAL_PREFIXES
            .split('|')
            .any(|prefix| text.trim_start().starts_with(prefix));
    if injected {
        return None;
    }
    Some((role, text))
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    let text = value.get(key).and_then(Value::as_str).map(str::trim)?;
    (!text.is_empty()).then(|| text.to_owned())
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_owned(), false);
    }
    (text.chars().take(max_chars).collect(), true)
}

/// The read/exec validated-root surface: `project_root` must be an exact root.
fn managed_root(snapshot: &Value, raw: &str) -> Result<PathBuf, Value> {
    let topology = crate::projects::derive_routing(snapshot);
    crate::fs_security::validate_exact_validated_root_with_topology(&topology, raw)
        .map(|managed| managed.root)
        .map_err(|_| {
            json!({
                "ok": false,
                "code": "project_root_not_managed",
                "message": "project_root must exactly match a project root in the live Herdr snapshot",
                "project_root": raw,
            })
        })
}

fn is_safe_agent_name(name: &str) -> bool {
    let tail = |value: char| {
        value.is_ascii_lowercase() || value.is_ascii_digit() || value == '_' || value == '-'
    };
    name.len() <= 32 && name.starts_with(|c: char| c.is_ascii_lowercase()) && name.chars().all(tail)
}

fn parse(params: &Value) -> Result<&Obj, Value> {
    params
        .as_object()
        .ok_or_else(|| invalid_params("params must be an object"))
}

fn required_string(o: &Obj, key: &str, max: usize) -> Result<String, Value> {
    match o.get(key).and_then(Value::as_str).map(str::trim) {
        Some(text) if !text.is_empty() && text.chars().count() <= max => Ok(text.to_owned()),
        _ => Err(invalid_params(format!(
            "{key} must be a non-empty string of at most {max} chars"
        ))),
    }
}

fn optional_string(o: &Obj, key: &str, max: usize) -> Result<Option<String>, Value> {
    let Some(value) = o.get(key).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    match value.as_str().map(str::trim) {
        Some(text) if !text.is_empty() && text.chars().count() <= max => Ok(Some(text.to_owned())),
        _ => Err(invalid_params(format!(
            "{key} must be a non-empty string of at most {max} chars"
        ))),
    }
}

fn bounded_usize(o: &Obj, key: &str, dflt: usize, min: usize, max: usize) -> Result<usize, Value> {
    let Some(value) = o.get(key).filter(|value| !value.is_null()) else {
        return Ok(dflt);
    };
    match value.as_u64() {
        Some(number) if number >= min as u64 && number <= max as u64 => Ok(number as usize),
        _ => Err(invalid_params(format!(
            "{key} must be an integer between {min} and {max}"
        ))),
    }
}

fn optional_u64(o: &Obj, key: &str, min: u64, max: u64) -> Result<Option<u64>, Value> {
    let Some(value) = o.get(key).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let bounded = value.as_u64().filter(|number| (min..=max).contains(number));
    let error = invalid_params(format!("{key} must be an integer between {min} and {max}"));
    bounded.map(Some).ok_or(error)
}

fn invalid_params(message: impl Into<String>) -> Value {
    json!({"ok": false, "code": "invalid_params", "message": message.into()})
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// One rollout body with `{id}`, `{cwd}`, and `{title}` placeholders.
    const ROLLOUT: &str = concat!(
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"{id}\",\"cwd\":{cwd},\"base_instructions\":\"SECRET\"}}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"<environment_context>ctx\"}]}}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"{title}\"}]}}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"assistant answer\"}]}}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"SECRET developer\"}]}}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"tool\",\"output\":\"SECRET tool\"}}\n",
    );

    /// A temp Codex home plus the managed project directory inside it.
    fn history() -> (PathBuf, PathBuf) {
        let unique = UNIX_EPOCH.elapsed().unwrap().as_nanos();
        let home = env::temp_dir().join(format!("herdr-codex-{}-{unique}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let project = home.join("project");
        fs::create_dir_all(&project).unwrap();
        (home, project)
    }

    fn write_session(dir: &Path, file: &str, id: &str, cwd: &Path, title: &str) -> PathBuf {
        let quoted = serde_json::to_string(&cwd.to_string_lossy()).unwrap();
        let body = ROLLOUT.replace("{id}", id).replace("{cwd}", &quoted);
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(file);
        fs::write(&path, body.replace("{title}", title)).unwrap();
        path
    }

    fn with_codex_home<T>(home: &Path, body: impl FnOnce() -> T) -> T {
        let previous = env::var_os("CODEX_HOME");
        unsafe { env::set_var("CODEX_HOME", home) };
        let value = body();
        match previous {
            Some(previous) => unsafe { env::set_var("CODEX_HOME", previous) },
            None => unsafe { env::remove_var("CODEX_HOME") },
        }
        let _ = fs::remove_dir_all(home);
        value
    }

    fn snapshot(root: &Path) -> Value {
        let cwd = root.to_string_lossy();
        json!({"panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": cwd}]})
    }

    fn set_modified(path: &Path, millis: u64) {
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        let times = fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_millis(millis));
        file.set_times(times).unwrap();
    }

    #[test]
    fn codex_home_follows_platform_home_precedence() {
        let home = Some(std::ffi::OsString::from("home-value"));
        let profile = Some(std::ffi::OsString::from("profile-value"));
        let resolved = crate::codex_paths::resolve_codex_home(None, home, profile).unwrap();
        #[cfg(windows)]
        assert_eq!(resolved, PathBuf::from("profile-value").join(".codex"));
        #[cfg(not(windows))]
        assert_eq!(resolved, PathBuf::from("home-value").join(".codex"));
    }

    /// Main path: project-scoped newest-first list with preview, conversation
    /// only read, and the native `codex resume <session id>` start request.
    #[test]
    fn codex_history_lists_reads_and_resumes_one_project() {
        let _guard = crate::test_env::lock();
        let (home, project) = history();
        let live = home.join("sessions/2026/01");
        let arch = home.join("archived_sessions");
        let older = "019f0000-0000-7000-8000-000000000001";
        let newer = "019f0000-0000-7000-8000-000000000002";
        let kept = "019f0000-0000-7000-8000-000000000004";
        let foreign = "019f0000-0000-7000-8000-000000000003";
        let a = write_session(&live, "a.jsonl", older, &project, "Fix the parser");
        let b = write_session(&arch, "b.jsonl", kept, &project, "Archived work");
        // The file name is not the session id: identity comes from the payload.
        let c = write_session(&live, "c.jsonl", newer, &project, "Explain the router");
        write_session(&arch, "d.jsonl", foreign, &home.join("other"), "Other work");
        set_modified(&a, 1_000_000_000_000);
        set_modified(&b, 2_100_000_000_000);
        set_modified(&c, 1_900_000_000_000);
        let root = json!({"project_root": project.to_string_lossy()});
        with_codex_home(&home, || {
            let snapshot = snapshot(&project);
            let listed = list(&root, &snapshot).unwrap();
            let sessions = listed["sessions"].as_array().unwrap();
            assert_eq!(listed["count"], 3);
            assert_eq!(listed["selection_strategy"], "project_root_exact_latest");
            assert_eq!(listed["preferred_session_id"], newer);
            assert_eq!(sessions[0]["session_id"], newer);
            assert_eq!(sessions[0]["title"], "Explain the router");
            assert_eq!(sessions[1]["session_id"], older);
            assert_eq!(sessions[2]["session_id"], kept);
            assert_eq!(sessions[2]["archived"], true);
            assert!(!listed.to_string().contains("SECRET"));

            let automatic = read(&root, &snapshot).unwrap();
            assert_eq!(
                automatic["selection"]["strategy"],
                "project_root_exact_latest"
            );
            assert_eq!(automatic["selection"]["session_id"], newer);
            assert_eq!(automatic["messages"][0]["text"], "Explain the router");

            let mut params = root.clone();
            params["session_id"] = json!(older);
            params["max_messages"] = json!(10);
            let explicit = read(&params, &snapshot).unwrap();
            let messages = explicit["messages"].as_array().unwrap();
            assert_eq!(explicit["selection"]["strategy"], "explicit_session_id");
            assert_eq!(explicit["selection"]["session_id"], older);
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0]["role"], "user");
            assert_eq!(messages[0]["text"], "Fix the parser");
            assert_eq!(messages[1]["text"], "assistant answer");
            assert!(!explicit.to_string().contains("SECRET"));
            let (start, _) = start_request("codex-resume", "w1:p1", newer, None);
            assert_eq!(start["kind"], "codex");
            assert_eq!(start["args"], json!(["resume", newer]));
        });
    }

    /// Another project's session is never used, an unknown id is missing, an
    /// unmanaged root is rejected, and readonly mode blocks resume.
    #[test]
    fn codex_history_refuses_foreign_sessions_and_readonly_resume() {
        let _guard = crate::test_env::lock();
        let (home, project) = history();
        let live = home.join("sessions/2026/01");
        let arch = home.join("archived_sessions");
        let own = "019f0000-0000-7000-8000-00000000000e";
        let foreign = "019f0000-0000-7000-8000-00000000000f";
        let other = home.join("other");
        write_session(&live, "own.jsonl", own, &project, "Own work");
        write_session(&arch, "foreign.jsonl", foreign, &other, "Other");
        let client = HerdrClient::new(home.join("absent.sock"));
        let resume_params = |session_id: &str| {
            json!({
                "project_root": project.to_string_lossy(),
                "session_id": session_id,
                "pane_id": "w1:p1",
                "name": "codex-resume",
            })
        };
        let previous = env::var_os("HERDR_MCP_READONLY");
        with_codex_home(&home, || {
            let snapshot = snapshot(&project);
            let root = json!({"project_root": project.to_string_lossy()});
            let denied = resume(&client, &resume_params(foreign), &snapshot);
            assert_eq!(denied.unwrap_err()["code"], "session_project_mismatch");
            let listed = list(&root, &snapshot).unwrap();
            assert_eq!(listed["count"], 1);
            assert_eq!(listed["sessions"][0]["session_id"], own);
            let mut params = root.clone();
            params["session_id"] = json!("019f0000-0000-7000-8000-0000000000ff");
            let missing = read(&params, &snapshot);
            assert_eq!(missing.unwrap_err()["code"], "session_not_found");
            let unmanaged = list(&json!({"project_root": "/not/managed"}), &snapshot);
            assert_eq!(unmanaged.unwrap_err()["code"], "project_root_not_managed");
            // Path-only selection resolves the same-project session before the
            // global gate, and readonly mode still blocks the socket mutation.
            unsafe { env::set_var("HERDR_MCP_READONLY", "1") };
            let blocked = resume(
                &client,
                &json!({
                    "project_root": project.to_string_lossy(),
                    "pane_id": "w1:p1",
                    "name": "codex-resume",
                }),
                &snapshot,
            );
            assert_eq!(blocked.unwrap_err()["reason"], "readonly_mode");
        });
        match previous {
            Some(previous) => unsafe { env::set_var("HERDR_MCP_READONLY", previous) },
            None => unsafe { env::remove_var("HERDR_MCP_READONLY") },
        }
    }
}
