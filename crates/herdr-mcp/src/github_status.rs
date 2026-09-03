use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const SOURCE: &str = "local_gh_api";
const GIT_TIMEOUT: Duration = Duration::from_secs(1);
const GH_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_COMMAND_BYTES: usize = 256 * 1024;
const MAX_STDERR_BYTES: usize = 2048;

pub fn status(params: &Value, snapshot: &Value) -> Value {
    let Some(object) = params.as_object() else {
        return invalid_params("params must be an object");
    };
    let allowed = ["project_root", "pr_number", "previous_fingerprint"]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let unknown = object
        .keys()
        .filter(|key| !allowed.contains(key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return json!({
            "ok": false,
            "code": "invalid_params",
            "message": "unknown github status params",
            "unknown": unknown,
        });
    }

    let Some(root_raw) = object.get("project_root").and_then(Value::as_str) else {
        return invalid_params("project_root must be a non-empty string");
    };
    if root_raw.trim().is_empty() {
        return invalid_params("project_root must be a non-empty string");
    }
    let root = PathBuf::from(root_raw);
    if !snapshot_contains_project_root(snapshot, &root) {
        return json!({
            "ok": false,
            "code": "project_root_not_managed",
            "message": "project_root must match a project/worktree in the live Herdr snapshot",
            "project_root": root_raw,
        });
    }

    let pr_number = match object.get("pr_number") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_u64() {
            Some(number) if number > 0 => Some(number),
            _ => return invalid_params("pr_number must be a positive integer"),
        },
    };
    let previous_fingerprint = match object.get("previous_fingerprint") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_str() {
            Some(text) if !text.trim().is_empty() => Some(text.trim()),
            _ => return invalid_params("previous_fingerprint must be a non-empty string"),
        },
    };

    let Some(gh) = find_gh() else {
        return json!({
            "ok": false,
            "code": "gh_unavailable",
            "message": "GitHub CLI (gh) is not available on the runtime PATH or standard install locations",
        });
    };
    let repository = match github_repository(&root) {
        Ok(repository) => repository,
        Err(error) => return error,
    };
    let repo_api_path = format!("repos/{repository}");
    let repository_json = match run_gh_json(&gh, &root, &["api", &repo_api_path]) {
        Ok(value) => value,
        Err(error) => return error,
    };

    let pr_json = if let Some(number) = pr_number {
        let number = number.to_string();
        match run_gh_json(
            &gh,
            &root,
            &[
                "pr",
                "view",
                &number,
                "--repo",
                &repository,
                "--json",
                "number,state,isDraft,mergeable,mergeStateStatus,headRefOid,baseRefOid,headRefName,baseRefName,autoMergeRequest,url",
            ],
        ) {
            Ok(value) => Some(value),
            Err(error) => return error,
        }
    } else {
        None
    };

    let (all_checks, required_checks) = if let Some(number) = pr_number {
        let number = number.to_string();
        let fields = "name,state,bucket,link,workflow";
        let all = match run_gh_checks_json(
            &gh,
            &root,
            &[
                "pr",
                "checks",
                &number,
                "--repo",
                &repository,
                "--json",
                fields,
            ],
        ) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let required = match run_gh_checks_json(
            &gh,
            &root,
            &[
                "pr",
                "checks",
                &number,
                "--repo",
                &repository,
                "--required",
                "--json",
                fields,
            ],
        ) {
            Ok(value) => value,
            Err(error) => return error,
        };
        (Some(all), Some(required))
    } else {
        (None, None)
    };

    render_status(
        &repository,
        &repository_json,
        pr_json.as_ref(),
        all_checks.as_ref(),
        required_checks.as_ref(),
        previous_fingerprint,
    )
}

fn snapshot_contains_project_root(snapshot: &Value, root: &Path) -> bool {
    let root = root.to_string_lossy();
    snapshot
        .get("workspaces")
        .and_then(Value::as_array)
        .is_some_and(|workspaces| {
            workspaces.iter().any(|workspace| {
                workspace
                    .get("worktree")
                    .and_then(|worktree| worktree.get("checkout_path"))
                    .and_then(Value::as_str)
                    .is_some_and(|path| path == root)
                    || workspace
                        .get("cwd")
                        .and_then(Value::as_str)
                        .is_some_and(|path| path == root)
            })
        })
}

fn github_repository(root: &Path) -> Result<String, Value> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["remote", "get-url", "origin"]);
    let output =
        crate::child_process::run_bounded_output(&mut command, GIT_TIMEOUT, MAX_COMMAND_BYTES)
            .map_err(|error| command_error("git_remote_failed", error.to_string()))?
            .ok_or_else(|| {
                command_error(
                    "git_remote_timeout",
                    "git remote probe timed out".to_owned(),
                )
            })?;
    if output.truncated {
        return Err(command_error(
            "git_remote_failed",
            "git remote probe output exceeded the bounded capture budget".to_owned(),
        ));
    }
    if !output.status.success() {
        return Err(command_error(
            "git_remote_failed",
            bounded_stderr(&output.stderr),
        ));
    }
    let remote = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    parse_github_repository(&remote).ok_or_else(|| {
        json!({
            "ok": false,
            "code": "unsupported_git_remote",
            "message": "origin must be a github.com repository remote",
        })
    })
}

fn parse_github_repository(remote: &str) -> Option<String> {
    let remote = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    let path = if let Some(rest) = remote.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = remote.strip_prefix("ssh://git@github.com/") {
        rest
    } else if let Some(rest) = remote.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = remote.strip_prefix("http://github.com/") {
        rest
    } else {
        remote.strip_prefix("git://github.com/")?
    };
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    if parts.next().is_some() || owner == "." || repo == "." {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn find_gh() -> Option<PathBuf> {
    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path) {
            let candidate = directory.join(if cfg!(windows) { "gh.exe" } else { "gh" });
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    if !cfg!(windows) {
        for path in ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"] {
            let candidate = PathBuf::from(path);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn run_gh_json(gh: &Path, root: &Path, args: &[&str]) -> Result<Value, Value> {
    run_gh_json_inner(gh, root, args, &[])
}

fn run_gh_checks_json(gh: &Path, root: &Path, args: &[&str]) -> Result<Value, Value> {
    run_gh_json_inner(gh, root, args, &[8])
}

fn run_gh_json_inner(
    gh: &Path,
    root: &Path,
    args: &[&str],
    accepted_exit_codes: &[i32],
) -> Result<Value, Value> {
    let mut command = Command::new(gh);
    command
        .args(args)
        .current_dir(root)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env("PAGER", "cat");
    let output =
        crate::child_process::run_bounded_output(&mut command, GH_TIMEOUT, MAX_COMMAND_BYTES)
            .map_err(|error| command_error("gh_exec_failed", error.to_string()))?
            .ok_or_else(|| {
                command_error("gh_timeout", "GitHub status probe timed out".to_owned())
            })?;
    if output.truncated {
        return Err(command_error(
            "gh_output_too_large",
            "GitHub status output exceeded the bounded capture budget".to_owned(),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let exit_code = output.status.code();
    if accept_json_exit(output.status.success(), exit_code, accepted_exit_codes)
        && let Ok(value) = serde_json::from_str::<Value>(stdout.trim())
    {
        return Ok(value);
    }
    Err(json!({
        "ok": false,
        "code": "gh_command_failed",
        "message": bounded_stderr(&output.stderr),
        "exit_code": exit_code,
    }))
}

fn accept_json_exit(success: bool, exit_code: Option<i32>, accepted_exit_codes: &[i32]) -> bool {
    success || exit_code.is_some_and(|code| accepted_exit_codes.contains(&code))
}

fn render_status(
    repository: &str,
    repository_json: &Value,
    pr_json: Option<&Value>,
    all_checks: Option<&Value>,
    required_checks: Option<&Value>,
    previous_fingerprint: Option<&str>,
) -> Value {
    let repo_summary = json!({
        "allow_auto_merge": repository_json.get("allow_auto_merge").cloned().unwrap_or(Value::Null),
        "default_branch": repository_json.get("default_branch").cloned().unwrap_or(Value::Null),
    });
    let pr_summary = pr_json.map(|pr| {
        json!({
            "number": pr.get("number").cloned().unwrap_or(Value::Null),
            "state": pr.get("state").cloned().unwrap_or(Value::Null),
            "draft": pr.get("isDraft").cloned().unwrap_or(Value::Null),
            "mergeable": pr.get("mergeable").cloned().unwrap_or(Value::Null),
            "merge_state": pr.get("mergeStateStatus").cloned().unwrap_or(Value::Null),
            "head_sha": pr.get("headRefOid").cloned().unwrap_or(Value::Null),
            "base_sha": pr.get("baseRefOid").cloned().unwrap_or(Value::Null),
            "head": pr.get("headRefName").cloned().unwrap_or(Value::Null),
            "base": pr.get("baseRefName").cloned().unwrap_or(Value::Null),
            "auto_merge": pr.get("autoMergeRequest").is_some_and(|value| !value.is_null()),
            "url": pr.get("url").cloned().unwrap_or(Value::Null),
        })
    });
    let checks = checks_summary(all_checks, required_checks);
    let canonical = json!({
        "repository": repository,
        "repo": repo_summary,
        "pr": pr_summary,
        "checks": checks.get("state").cloned().unwrap_or(Value::Null),
    });
    let fingerprint = fingerprint(&canonical);
    let observed_at = now_rfc3339();
    let compact_summary = json!({
        "repository": repository,
        "allow_auto_merge": repo_summary.get("allow_auto_merge").cloned().unwrap_or(Value::Null),
        "pr_state": pr_summary.as_ref().and_then(|value| value.get("state")).cloned().unwrap_or(Value::Null),
        "merge_state": pr_summary.as_ref().and_then(|value| value.get("merge_state")).cloned().unwrap_or(Value::Null),
        "required": checks.get("required_counts").cloned().unwrap_or(Value::Null),
        "all": checks.get("all_counts").cloned().unwrap_or(Value::Null),
    });
    if previous_fingerprint == Some(fingerprint.as_str()) {
        return json!({
            "ok": true,
            "source": SOURCE,
            "fresh": true,
            "cache_policy": "bypass_connector_cache",
            "observed_at": observed_at,
            "changed": false,
            "fingerprint": fingerprint,
            "summary": compact_summary,
        });
    }
    json!({
        "ok": true,
        "source": SOURCE,
        "fresh": true,
        "cache_policy": "bypass_connector_cache",
        "observed_at": observed_at,
        "changed": true,
        "fingerprint": fingerprint,
        "repository": {
            "full_name": repository,
            "allow_auto_merge": repo_summary.get("allow_auto_merge").cloned().unwrap_or(Value::Null),
            "default_branch": repo_summary.get("default_branch").cloned().unwrap_or(Value::Null),
        },
        "pr": pr_summary,
        "checks": checks.get("details").cloned().unwrap_or(Value::Null),
        "summary": compact_summary,
    })
}

fn checks_summary(all_checks: Option<&Value>, required_checks: Option<&Value>) -> Value {
    let all = check_rows(all_checks);
    let required = check_rows(required_checks);
    let required_names = required
        .iter()
        .filter_map(|row| row.get("name").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    let supplemental = all
        .iter()
        .filter(|row| {
            row.get("name")
                .and_then(Value::as_str)
                .is_none_or(|name| !required_names.contains(name))
        })
        .cloned()
        .collect::<Vec<_>>();
    let state = json!({
        "required": required.iter().map(minimal_check).collect::<Vec<_>>(),
        "supplemental": supplemental.iter().map(minimal_check).collect::<Vec<_>>(),
    });
    json!({
        "state": state,
        "required_counts": bucket_counts(&required),
        "all_counts": bucket_counts(&all),
        "details": {
            "required": required,
            "supplemental": supplemental,
            "required_counts": bucket_counts(&check_rows(required_checks)),
            "all_counts": bucket_counts(&check_rows(all_checks)),
        }
    })
}

fn check_rows(value: Option<&Value>) -> Vec<Value> {
    value.and_then(Value::as_array).cloned().unwrap_or_default()
}

fn minimal_check(row: &Value) -> Value {
    json!({
        "name": row.get("name").cloned().unwrap_or(Value::Null),
        "state": row.get("state").cloned().unwrap_or(Value::Null),
        "bucket": row.get("bucket").cloned().unwrap_or(Value::Null),
    })
}

fn bucket_counts(rows: &[Value]) -> Value {
    let mut counts = BTreeMap::<String, u64>::new();
    for row in rows {
        let bucket = row
            .get("bucket")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        *counts.entry(bucket).or_default() += 1;
    }
    serde_json::to_value(counts).unwrap_or_else(|_| json!({}))
}

fn fingerprint(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

fn invalid_params(message: &str) -> Value {
    json!({"ok": false, "code": "invalid_params", "message": message})
}

fn command_error(code: &str, message: String) -> Value {
    json!({"ok": false, "code": code, "message": message})
}

fn bounded_stderr(stderr: &[u8]) -> String {
    let bytes = if stderr.len() > MAX_STDERR_BYTES {
        &stderr[..MAX_STDERR_BYTES]
    } else {
        stderr
    };
    String::from_utf8_lossy(bytes).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_github_remotes() {
        for remote in [
            "git@github.com:whshang/herdr-mcp.git",
            "https://github.com/whshang/herdr-mcp.git",
            "ssh://git@github.com/whshang/herdr-mcp.git",
        ] {
            assert_eq!(
                parse_github_repository(remote).as_deref(),
                Some("whshang/herdr-mcp")
            );
        }
        assert_eq!(parse_github_repository("https://gitlab.com/x/y.git"), None);
    }

    #[test]
    fn unchanged_fingerprint_returns_compact_result() {
        let repo = json!({"allow_auto_merge": true, "default_branch": "main"});
        let pr = json!({
            "number": 284,
            "state": "OPEN",
            "isDraft": false,
            "mergeable": "MERGEABLE",
            "mergeStateStatus": "BLOCKED",
            "headRefOid": "abc",
            "baseRefOid": "def",
            "headRefName": "feature",
            "baseRefName": "main",
            "autoMergeRequest": {"mergeMethod": "MERGE"},
            "url": "https://github.com/o/r/pull/284"
        });
        let all = json!([
            {"name":"rust","state":"SUCCESS","bucket":"pass","link":"x","workflow":"CI"},
            {"name":"deploy","state":"PENDING","bucket":"pending","link":"y","workflow":""}
        ]);
        let required = json!([
            {"name":"rust","state":"SUCCESS","bucket":"pass","link":"x","workflow":"CI"}
        ]);
        let first = render_status("o/r", &repo, Some(&pr), Some(&all), Some(&required), None);
        let fingerprint = first["fingerprint"].as_str().unwrap();
        let second = render_status(
            "o/r",
            &repo,
            Some(&pr),
            Some(&all),
            Some(&required),
            Some(fingerprint),
        );
        assert_eq!(second["changed"], false);
        assert!(second.get("checks").is_none());
        assert_eq!(second["summary"]["required"]["pass"], 1);
        assert_eq!(second["summary"]["all"]["pending"], 1);
    }

    #[test]
    fn project_root_must_be_live_managed_root() {
        let snapshot = json!({
            "workspaces": [{"worktree": {"checkout_path": "/repo"}}]
        });
        assert!(snapshot_contains_project_root(
            &snapshot,
            Path::new("/repo")
        ));
        assert!(!snapshot_contains_project_root(
            &snapshot,
            Path::new("/other")
        ));
    }

    #[test]
    fn only_documented_pending_exit_is_accepted_for_check_json() {
        assert!(accept_json_exit(true, Some(0), &[8]));
        assert!(accept_json_exit(false, Some(8), &[8]));
        assert!(!accept_json_exit(false, Some(1), &[8]));
        assert!(!accept_json_exit(false, None, &[8]));
    }
}
