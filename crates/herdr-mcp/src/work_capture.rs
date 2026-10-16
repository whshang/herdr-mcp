use crate::projects;
use crate::state_store::work_journal::{NewWorkEvent, WORK_SUMMARY_MAX_CHARS, clip_chars};
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const REDACTED: &str = "[REDACTED]";
const SALIENT_KEYS: [&str; 14] = [
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
];

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

    let scope = resolve_scope(arguments, snapshot)?;
    let kind = classify(tool_name, arguments);
    let summary = summarize(tool_name, arguments);
    Some(NewWorkEvent {
        ts: now_ms(),
        project_ref: redact(&scope.project_ref),
        worktree: redact(&scope.worktree.to_string_lossy()),
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

pub(crate) fn redact(input: &str) -> String {
    let mut value = private_key_regex()
        .replace_all(input, REDACTED)
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
    let mut candidates = BTreeSet::new();
    collect_path_candidates(arguments, &mut candidates);

    let project = candidates
        .iter()
        .flat_map(|candidate| {
            let candidate = Path::new(candidate);
            topology
                .projects
                .values()
                .filter(move |project| project.managed && candidate.starts_with(&project.root))
        })
        .max_by_key(|project| project.root.components().count())
        .or_else(|| {
            workspace_argument(arguments, &topology)
                .and_then(|workspace| {
                    projects::projects_for_workspace(&topology, &workspace)
                        .into_iter()
                        .find(|project| project.managed)
                })
                .and_then(|project| topology.projects.get(&project.root))
        })?;

    let project_ref = project_ref_for_root(snapshot, &project.root)
        .unwrap_or_else(|| format!("dir:{}", project.root.to_string_lossy()));
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

fn assigned_secret_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r#"(?i)\b(api[_-]?key|password|passwd|token|secret)\b\s*[:=]\s*(?:"[^"]*"|'[^']*'|[^\s,;}\]]+)"#)
            .expect("assigned secret regex")
    })
}

fn high_entropy_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| Regex::new(r"[A-Za-z0-9+/=_-]{32,}").expect("high entropy regex"))
}

fn looks_high_entropy(value: &str) -> bool {
    let mut counts = [0_u16; 128];
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
            let probability = f64::from(count) / length;
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
            "Authorization: Bearer abc.def password=hunter2 token='hidden' sk-abcdefghijklmnopqrstuvwxyz ghp_abcdefghijklmnopqrstuvwxyz github_pat_abcdefghijklmnopqrstuvwxyz123 xoxb-1234567890-secret AKIAIOSFODNN7EXAMPLE {private_key} opaque={random} hex={random_hex}"
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
        ] {
            assert!(!output.contains(secret), "secret remained: {secret}");
        }
        assert!(output.contains(REDACTED));
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
        assert!(
            event_for_tool_call(
                "herdr_call",
                &json!({"method": "work.pickup", "params": "{\"cwd\":\"/repo\"}"}),
                &snapshot,
            )
            .is_none()
        );
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
}
