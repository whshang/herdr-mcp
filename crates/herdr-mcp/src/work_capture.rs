use crate::projects;
use crate::state_store::work_journal::{NewWorkEvent, WORK_SUMMARY_MAX_CHARS, clip_chars};
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const REDACTED: &str = "[REDACTED]";

/// Summaries are clipped to WORK_SUMMARY_MAX_CHARS, so nothing past this
/// bound can survive into the stored event. Clip before running the regexes
/// to bound the redaction work applied to any single tool-call value.
const REDACT_INPUT_MAX_CHARS: usize = 8 * 1024;
const SALIENT_KEYS: [&str; 16] = [
    "method",
    "command",
    "project_root",
    "root",
    "cwd",
    "path",
    "paths",
    "pattern",
    "query",
    "workspace",
    "workspace_id",
    "pane",
    "pane_id",
    "prompt",
    "target",
    "text",
];

const HANDOFF_CONTEXT_HEADING: &str = "## Handoff context";

/// Remove a herdr-injected `## Handoff context ... ---` prefix so injected
/// handoffs are never re-journaled (feedback loop). Returns the user's own
/// text, or `None` when nothing remains after the injected block.
pub(crate) fn strip_handoff_context(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix(HANDOFF_CONTEXT_HEADING) else {
        return (!text.trim().is_empty()).then(|| text.to_owned());
    };
    let rest = rest.replace("\r\n", "\n");
    let user = rest
        .find("\n---\n")
        .map(|index| rest[index + 5..].trim().to_owned())
        .unwrap_or_default();
    (!user.is_empty()).then_some(user)
}

pub(crate) fn event_for_tool_call(
    tool_name: &str,
    arguments: &Value,
    snapshot: &Value,
) -> Option<NewWorkEvent> {
    if tool_name == "herdr_call"
        && arguments.get("method").and_then(Value::as_str) == Some("work.pickup")
    {
        return None;
    }

    // Structural redaction first: sensitive-named JSON fields are replaced
    // outright and the remaining strings are text-redacted, so the summary
    // (and scope fields) are built from already-sanitized values.
    let mut sanitized = sanitize_json(arguments);
    if let Some(object) = sanitized.as_object_mut() {
        for key in ["prompt", "text"] {
            if let Some(Value::String(text)) = object.get(key)
                && let stripped = strip_handoff_context(text).unwrap_or_default()
                && stripped != *text
            {
                object.insert(key.to_owned(), Value::String(stripped));
            }
        }
    }
    // Scope comes from the RAW arguments matched against topology roots:
    // high-entropy redaction can mangle long path tokens, which would drop
    // the event. project_ref/worktree are topology-derived, not user text.
    let scope = resolve_scope(arguments, snapshot)?;
    let arguments = &sanitized;
    let kind = classify(tool_name, arguments);
    let summary = summarize(tool_name, arguments);
    Some(NewWorkEvent {
        ts: now_ms(),
        project_ref: scope.project_ref.clone(),
        worktree: scope.worktree.to_string_lossy().into_owned(),
        provider: "herdr-mcp".to_owned(),
        account: None,
        kind: redact(&kind),
        summary,
        source_ref: redact(&format!("mcp:{tool_name}")),
        dedupe_key: None,
    })
}

pub(crate) fn classify(tool_name: &str, arguments: &Value) -> String {
    match tool_name {
        "exec" | "herdr_exec" | "herdr_exec_start" | "herdr_exec_read" | "herdr_exec_kill" => {
            "command".to_owned()
        }
        "herdr_fs_write" | "herdr_fs_edit" | "herdr_fs_patch" | "fs_patch" => "edit".to_owned(),
        "herdr_fs_read" | "herdr_fs_grep" | "herdr_fs_list" | "herdr_fs_image" | "fs_read"
        | "grep" | "list" => "read".to_owned(),
        "herdr_git" => "git".to_owned(),
        "herdr_prompt" => "dispatch".to_owned(),
        "herdr_call" => {
            let prefix = arguments
                .get("method")
                .and_then(Value::as_str)
                .and_then(|method| method.split('.').next())
                .filter(|prefix| !prefix.is_empty())
                .unwrap_or("unknown");
            format!("call:{prefix}")
        }
        _ if tool_name.starts_with("herdr_exec") => "command".to_owned(),
        _ => "tool".to_owned(),
    }
}

/// Walk JSON argument values, replacing the value of any sensitive-named
/// key (password/passwd/secret/token/api key/authorization/cookie/
/// credential/private key, case-insensitive) with a redaction marker. String
/// leaves under non-sensitive keys are still text-redacted.
fn sanitize_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sanitized = serde_json::Map::with_capacity(map.len());
            for (key, item) in map {
                if sensitive_key_regex().is_match(key) {
                    sanitized.insert(key.clone(), Value::String(REDACTED.to_owned()));
                } else {
                    sanitized.insert(key.clone(), sanitize_json(item));
                }
            }
            Value::Object(sanitized)
        }
        Value::Array(items) => Value::Array(items.iter().map(sanitize_json).collect()),
        Value::String(text) => Value::String(redact(text)),
        scalar => scalar.clone(),
    }
}

pub(crate) fn redact(input: &str) -> String {
    let input = clip_chars(input, REDACT_INPUT_MAX_CHARS);
    let mut value = private_key_regex()
        .replace_all(&input, REDACTED)
        .into_owned();
    value = url_userinfo_regex()
        .replace_all(&value, |captures: &regex::Captures<'_>| {
            format!("{}{REDACTED}@", &captures[1])
        })
        .into_owned();
    value = bearer_regex().replace_all(&value, REDACTED).into_owned();
    value = known_token_regex()
        .replace_all(&value, REDACTED)
        .into_owned();
    value = assigned_secret_regex()
        .replace_all(&value, |captures: &regex::Captures<'_>| {
            format!("{}={REDACTED}", &captures[1])
        })
        .into_owned();
    high_entropy_regex()
        .replace_all(&value, |captures: &regex::Captures<'_>| {
            let candidate = &captures[0];
            if looks_high_entropy(candidate) {
                REDACTED.to_owned()
            } else {
                candidate.to_owned()
            }
        })
        .into_owned()
}

fn summarize(tool_name: &str, arguments: &Value) -> String {
    let mut parts = vec![tool_name.to_owned()];
    let Some(object) = arguments.as_object() else {
        return tool_name.to_owned();
    };
    for key in SALIENT_KEYS {
        let Some(value) = object.get(key) else {
            continue;
        };
        let rendered = compact_value(value);
        if !rendered.is_empty() {
            parts.push(format!("{key}={rendered}"));
        }
    }
    clip_chars(&redact(&parts.join(" ")), WORK_SUMMARY_MAX_CHARS)
}

fn compact_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.split_whitespace().collect::<Vec<_>>().join(" "),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

#[derive(Debug)]
struct WorkScope {
    project_ref: String,
    worktree: PathBuf,
}

fn resolve_scope(arguments: &Value, snapshot: &Value) -> Option<WorkScope> {
    let topology = projects::derive_routing(snapshot);
    // Tool_call capture scopes to the unified validated-root surface: managed
    // Git roots plus vcs-less operational roots proven by the live topology
    // (same surface the tailers tail). Project key = the git root when any,
    // else the operational folder itself.
    let validated = crate::fs_security::validated_roots_from(&topology);
    let mut candidates = BTreeSet::new();
    collect_path_candidates(arguments, &mut candidates);

    let is_validated = |root: &Path| validated.iter().any(|valid| valid == root);
    let project = candidates
        .iter()
        .flat_map(|candidate| {
            let candidate = Path::new(candidate);
            topology.projects.values().filter(|project| {
                is_validated(&project.root) && candidate.starts_with(&project.root)
            })
        })
        .max_by_key(|project| project.root.components().count())
        .or_else(|| {
            workspace_argument(arguments, &topology)
                .and_then(|workspace| {
                    projects::projects_for_workspace(&topology, &workspace)
                        .into_iter()
                        .find(|project| is_validated(&project.root))
                })
                .and_then(|project| topology.projects.get(&project.root))
        })?;

    let project_ref = project_ref_for(snapshot, &project.root);
    Some(WorkScope {
        project_ref,
        worktree: project.root.clone(),
    })
}

fn collect_path_candidates(arguments: &Value, candidates: &mut BTreeSet<String>) {
    let Some(object) = arguments.as_object() else {
        return;
    };
    for key in ["project_root", "root", "cwd", "path"] {
        if let Some(path) = object.get(key).and_then(Value::as_str)
            && Path::new(path).is_absolute()
        {
            candidates.insert(path.to_owned());
        }
    }
    if let Some(paths) = object.get("paths").and_then(Value::as_array) {
        for path in paths.iter().filter_map(Value::as_str) {
            if Path::new(path).is_absolute() {
                candidates.insert(path.to_owned());
            }
        }
    }
    if let Some(params) = object.get("params").and_then(Value::as_str)
        && let Ok(params) = serde_json::from_str::<Value>(params)
    {
        collect_path_candidates(&params, candidates);
    }
}

fn workspace_argument(arguments: &Value, topology: &projects::ProjectTopology) -> Option<String> {
    arguments
        .get("workspace")
        .and_then(Value::as_str)
        .or_else(|| arguments.get("workspace_id").and_then(Value::as_str))
        .map(str::to_owned)
        .or_else(|| {
            arguments
                .get("pane")
                .and_then(Value::as_str)
                .or_else(|| arguments.get("pane_id").and_then(Value::as_str))
                // herdr_prompt addresses its pane as `target`.
                .or_else(|| arguments.get("target").and_then(Value::as_str))
                .and_then(|pane| topology.pane_to_workspace.get(pane).cloned())
        })
        .or_else(|| {
            arguments
                .get("params")
                .and_then(Value::as_str)
                .and_then(|params| serde_json::from_str::<Value>(params).ok())
                .and_then(|params| workspace_argument(&params, topology))
        })
}

/// The single project_ref derivation shared by tool_call capture and the
/// native-log tailers.
pub(crate) fn project_ref_for(snapshot: &Value, root: &Path) -> String {
    project_ref_for_root(snapshot, root)
        .unwrap_or_else(|| format!("dir:{}", root.to_string_lossy()))
}

fn project_ref_for_root(snapshot: &Value, root: &Path) -> Option<String> {
    snapshot
        .get("workspaces")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|workspace| workspace.get("worktree").and_then(Value::as_object))
        .find_map(|worktree| {
            let matches_root = ["checkout_path", "repo_root"]
                .into_iter()
                .filter_map(|key| worktree.get(key).and_then(Value::as_str))
                .any(|path| Path::new(path) == root);
            matches_root
                .then(|| worktree.get("repo_key").and_then(Value::as_str))
                .flatten()
                .filter(|repo_key| !repo_key.is_empty())
                .map(|repo_key| format!("git:{repo_key}"))
        })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn private_key_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r"(?s)-----BEGIN [^-\r\n]*PRIVATE KEY-----.*?-----END [^-\r\n]*PRIVATE KEY-----")
            .expect("private key regex")
    })
}

/// user:password@ credentials embedded in an otherwise-public URL: the
/// whole userinfo pair is dropped, keeping only the scheme and the host.
fn url_userinfo_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)[^\s/@:]+:[^\s/@]+@").expect("url userinfo regex")
    })
}

fn bearer_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| Regex::new(r"(?i)\bbearer\s+[^\s,;]+").expect("bearer regex"))
}

fn known_token_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r"\b(?:sk-[A-Za-z0-9_-]{16,}|ghp_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|xox[a-zA-Z]-[A-Za-z0-9-]{10,}|AKIA[A-Z0-9]{16})\b")
            .expect("known token regex")
    })
}

/// Sensitive JSON key names: substring match so names like
/// `github_api_key` are covered as well as exact forms.
fn sensitive_key_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(
            r"(?i)password|passwd|secret|token|api[_-]?key|authorization|cookie|credential|private[_-]?key",
        )
        .expect("sensitive key regex")
    })
}

fn assigned_secret_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        // The key itself may be bare (`password=`), double-quoted
        // (`"password":"x"`) or single-quoted (`'token': 'y'`), with optional
        // whitespace around the separator.
        Regex::new(r#"(?i)(["']?(?:password|passwd|secret|token|api[_-]?key|authorization|cookie|credential|private[_-]?key)["']?)\s*[:=]\s*(?:\[REDACTED\]|"[^"]*"|'[^']*'|[^\s,;}\]]+)"#)
            .expect("assigned secret regex")
    })
}

fn high_entropy_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| Regex::new(r"[A-Za-z0-9+/=_-]{32,}").expect("high entropy regex"))
}

fn looks_high_entropy(value: &str) -> bool {
    // usize counters: a single repeated byte in an unbounded token must not
    // wrap (debug builds panic on integer overflow).
    let mut counts = [0_usize; 128];
    let mut length = 0_f64;
    for byte in value.bytes() {
        if !byte.is_ascii() {
            return false;
        }
        counts[byte as usize] += 1;
        length += 1.0;
    }
    let entropy = counts
        .into_iter()
        .filter(|count| *count > 0)
        .map(|count| {
            let probability = count as f64 / length;
            -probability * probability.log2()
        })
        .sum::<f64>();
    entropy >= 3.5
}

#[cfg(test)]
mod tests {

    use super::*;
    use serde_json::json;

    #[test]
    fn user_sees_tool_calls_classified_by_public_name_and_call_prefix() {
        assert_eq!(classify("herdr_exec_start", &json!({})), "command");
        assert_eq!(classify("herdr_fs_patch", &json!({})), "edit");
        assert_eq!(classify("herdr_fs_grep", &json!({})), "read");
        assert_eq!(classify("herdr_git", &json!({})), "git");
        assert_eq!(classify("herdr_prompt", &json!({})), "dispatch");
        assert_eq!(
            classify("herdr_call", &json!({"method": "continuity.resume"})),
            "call:continuity"
        );
        assert_eq!(classify("herdr_inspect", &json!({})), "tool");
    }

    #[test]
    fn user_secrets_are_removed_before_journal_storage() {
        let private_key = "-----BEGIN PRIVATE KEY-----\nabc123\n-----END PRIVATE KEY-----";
        let random = "xP9mQ2vL7sN4kR8tY1wC6dF3hJ5uB0zA";
        let random_hex = "c81e728d9d4c2f636f067f89cc14862c9a1b2c3d4e5f6071";
        let input = format!(
            "Authorization: Bearer abc.def password=hunter2 token='hidden' sk-abcdefghijklmnopqrstuvwxyz ghp_abcdefghijklmnopqrstuvwxyz github_pat_abcdefghijklmnopqrstuvwxyz123 xoxb-1234567890-secret AKIAIOSFODNN7EXAMPLE {private_key} opaque={random} hex={random_hex} fetch=https://ci-user:pw-hunter@example.invalid/repo"
        );
        let output = redact(&input);
        for secret in [
            "abc.def",
            "hunter2",
            "hidden",
            "sk-abcdefghijklmnopqrstuvwxyz",
            "ghp_abcdefghijklmnopqrstuvwxyz",
            "github_pat_abcdefghijklmnopqrstuvwxyz123",
            "xoxb-1234567890-secret",
            "AKIAIOSFODNN7EXAMPLE",
            "abc123",
            random,
            random_hex,
            "pw-hunter",
            "ci-user:pw-hunter",
        ] {
            assert!(!output.contains(secret), "secret remained: {secret}");
        }
        assert!(output.contains(REDACTED));
        assert!(
            output.contains("https://[REDACTED]@example.invalid/repo"),
            "{output}"
        );
    }

    fn test_snapshot() -> Value {
        json!({
            "workspaces": [{
                "workspace_id": "w1",
                "worktree": {
                    "checkout_path": "/repo",
                    "repo_root": "/repo",
                    "repo_key": "github.com/example/repo"
                }
            }],
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": "/repo/src"}],
            "agents": []
        })
    }

    #[test]
    fn user_tool_call_is_scoped_to_a_managed_worktree_and_pickup_is_excluded() {
        let snapshot = json!({
            "workspaces": [{
                "workspace_id": "w1",
                "worktree": {
                    "checkout_path": "/repo",
                    "repo_root": "/repo",
                    "repo_key": "github.com/example/repo"
                }
            }],
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": "/repo/src"}],
            "agents": []
        });
        let event = event_for_tool_call(
            "herdr_fs_write",
            &json!({"path": "/repo/src/main.rs", "content": "secret=hidden"}),
            &snapshot,
        )
        .expect("managed path should resolve");
        assert_eq!(event.project_ref, "git:github.com/example/repo");
        assert_eq!(event.worktree, "/repo");
        assert_eq!(event.kind, "edit");
        assert_eq!(event.source_ref, "mcp:herdr_fs_write");
        assert!(!event.summary.contains("hidden"));
        assert!(event_for_tool_call("herdr_inspect", &json!({}), &snapshot).is_none());
        let prompt = event_for_tool_call(
            "herdr_prompt",
            &json!({"target": "w1:p1", "text": "## Handoff context\n\nold\n\n---\n\nreal task"}),
            &snapshot,
        )
        .expect("herdr_prompt target pane should resolve its worktree");
        assert_eq!(prompt.worktree, "/repo");
        assert!(prompt.summary.contains("real task"));
        assert!(!prompt.summary.contains("Handoff context"));
        assert!(
            event_for_tool_call(
                "herdr_call",
                &json!({"method": "work.pickup", "params": "{\"cwd\":\"/repo\"}"}),
                &snapshot,
            )
            .is_none()
        );
    }

    /// user tool call in a vcs-less folder | Given a live pane cwd that is
    /// not a Git repo (an operational root proven by the topology) | When a
    /// tool call under it is captured | Then the event scopes to the folder
    /// itself with a dir:-prefixed project key.
    #[test]
    fn user_tool_call_in_a_vcs_less_operational_folder_is_captured() {
        // Short path on purpose: / and - are in the high-entropy charset, so
        // a >=32-char path token would be redacted before scope resolution.
        let root = Path::new("/tmp/herdr-oproot");
        std::fs::create_dir_all(root).unwrap();
        let snapshot = json!({
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": root.to_string_lossy()
            }],
            "agents": []
        });
        let event = event_for_tool_call(
            "herdr_fs_write",
            &json!({"path": root.join("notes.md").to_string_lossy()}),
            &snapshot,
        )
        .expect("operational root should resolve");
        assert_eq!(event.worktree, root.to_string_lossy());
        assert_eq!(event.project_ref, format!("dir:{}", root.to_string_lossy()));
        // The pane-only path resolves through the workspace as well.
        let via_pane = event_for_tool_call("herdr_exec", &json!({"pane_id": "w1:p1"}), &snapshot)
            .expect("operational workspace fallback should resolve");
        assert_eq!(via_pane.worktree, root.to_string_lossy());
        // Outside the folder stays uncaptured.
        assert!(
            event_for_tool_call(
                "herdr_fs_write",
                &json!({"path": "/tmp/herdr-elsewhere/x.md"}),
                &snapshot,
            )
            .is_none()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn user_json_embedded_secrets_never_reach_the_stored_summary() {
        // Quoted-key JSON forms embedded in salient string arguments.
        let embedded = event_for_tool_call(
            "herdr_prompt",
            &json!({
                "prompt": "apply config {\"password\":\"hunter2\", \"api_key\": \"sq-topsecret-\", 'token': 'zz-hidden-token'} done",
                "path": "/repo/src/main.rs"
            }),
            &test_snapshot(),
        )
        .expect("managed path should resolve");
        for secret in ["hunter2", "sq-topsecret-", "zz-hidden-token"] {
            assert!(
                !embedded.summary.contains(secret),
                "secret leaked into summary: {secret}"
            );
        }
        assert!(embedded.summary.contains(REDACTED));

        // Sensitive-named keys in nested argument objects are redacted
        // structurally, whatever the value format.
        let structured = event_for_tool_call(
            "tool",
            &json!({
                "path": "/repo/src/main.rs",
                "query": {
                    "password": "hunter2-nested",
                    "api_key": "sq-nested-secret",
                    "nested": [1, {"private_key": "key-material-zzz"}]
                }
            }),
            &test_snapshot(),
        )
        .expect("managed path should resolve");
        for secret in ["hunter2-nested", "sq-nested-secret", "key-material-zzz"] {
            assert!(
                !structured.summary.contains(secret),
                "secret leaked into summary: {secret}"
            );
        }
        assert!(structured.summary.contains(REDACTED));
    }

    #[test]
    fn user_summary_is_bounded_and_redacted() {
        let summary = summarize(
            "herdr_exec",
            &json!({"command": format!("echo token=hidden {}", "x".repeat(500))}),
        );
        assert!(summary.chars().count() <= WORK_SUMMARY_MAX_CHARS);
        assert!(!summary.contains("hidden"));
    }

    #[test]
    fn user_oversized_arguments_stay_bounded_without_panicking() {
        // >70k chars of one repeated byte: previously overflowed the u16
        // entropy counter and panicked in debug builds.
        let big = "a".repeat(140_000);
        let event = event_for_tool_call(
            "herdr_exec",
            &json!({"path": "/repo", "command": big}),
            &test_snapshot(),
        )
        .expect("managed path should resolve");
        assert!(event.summary.chars().count() <= WORK_SUMMARY_MAX_CHARS);
        let redacted = redact(&"b".repeat(140_000));
        assert!(redacted.chars().count() <= REDACT_INPUT_MAX_CHARS + 1);
    }
}

#[cfg(test)]
mod redact_idempotence_tests {
    #[test]
    fn known_token_after_assignment_is_not_double_bracketed() {
        let out = super::redact("echo token=ghp_FAKEe2eTOKEN1234567890abcdefABCDEF12 && ls");
        assert_eq!(out, "echo token=[REDACTED] && ls");
    }
}

#[cfg(test)]
mod strip_handoff_tests {
    use super::strip_handoff_context;

    #[test]
    fn injected_handoff_block_is_stripped() {
        let injected = "## Handoff context\n\n## Recent\n- [read] x\n\n---\n\ndo the next thing";
        assert_eq!(
            strip_handoff_context(injected).as_deref(),
            Some("do the next thing")
        );
        assert_eq!(
            strip_handoff_context("## Handoff context\n\nonly handoff\n\n---\n\n  "),
            None
        );
        assert_eq!(
            strip_handoff_context("## Handoff context\n\nno separator"),
            None
        );
        assert_eq!(
            strip_handoff_context("plain prompt").as_deref(),
            Some("plain prompt")
        );
        assert_eq!(strip_handoff_context("   "), None);
    }

    #[test]
    fn herdr_prompt_audit_drops_injected_block() {
        let event = super::event_for_tool_call(
            "herdr_prompt",
            &serde_json::json!({"prompt": "## Handoff context\n\nold\n\n---\n\nreal task", "cwd": "/repo"}),
            &serde_json::json!({}),
        );
        if let Some(event) = event {
            assert!(!event.summary.contains("Handoff context"));
        }
    }
}
