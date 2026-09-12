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
// Local Git metadata is normally sub-millisecond, but a one-second budget can
// false-timeout under release-link/LTO scheduler pressure and force the planner
// into a second status request. Keep it bounded without making CPU contention a
// remote-call amplifier.
const GIT_TIMEOUT: Duration = Duration::from_secs(3);
const GH_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_COMMAND_BYTES: usize = 256 * 1024;
const MAX_STDERR_BYTES: usize = 2048;
const MAX_CHECK_PAGES: usize = 32;

const PR_STATUS_META_QUERY: &str = r#"
query($owner:String!,$name:String!,$number:Int!){
  repository(owner:$owner,name:$name){
    autoMergeAllowed
    defaultBranchRef{name}
    pullRequest(number:$number){
      id number state isDraft mergeable mergeStateStatus
      headRefOid baseRefOid headRefName baseRefName
      autoMergeRequest{mergeMethod}
      url
    }
  }
}
"#;

const PR_STATUS_CHECKS_QUERY: &str = r#"
query($id:ID!,$endCursor:String){
  node(id:$id){
    ...on PullRequest{
      commits(last:1){
        nodes{
          commit{
            statusCheckRollup{
              contexts(first:100,after:$endCursor){
                nodes{
                  __typename
                  ...on StatusContext{
                    context state targetUrl createdAt description
                    isRequired(pullRequestId:$id)
                  }
                  ...on CheckRun{
                    name
                    checkSuite{workflowRun{event workflow{name}}}
                    status conclusion startedAt completedAt detailsUrl
                    isRequired(pullRequestId:$id)
                  }
                }
                pageInfo{hasNextPage endCursor}
              }
            }
          }
        }
      }
    }
  }
}
"#;

pub fn status(params: &Value, snapshot: &Value) -> Value {
    let Some(object) = params.as_object() else {
        return invalid_params("params must be an object");
    };
    let allowed = [
        "project_root",
        "repository",
        "pr_number",
        "previous_fingerprint",
    ]
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
    let root = match managed_project_root(snapshot, root_raw) {
        Ok(root) => root,
        Err(error) => return error,
    };

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
    let explicit_repository = match object.get("repository") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_str().and_then(parse_repository_name) {
            Some(repository) => Some(repository),
            None => {
                return invalid_params("repository must be an owner/repo GitHub repository name");
            }
        },
    };

    let Some(gh) = find_gh() else {
        return json!({
            "ok": false,
            "code": "gh_unavailable",
            "message": "GitHub CLI (gh) is not available on the runtime PATH or standard install locations",
        });
    };
    let repository = match explicit_repository {
        Some(repository) => repository,
        None => match github_repository(&root) {
            Ok(repository) => repository,
            Err(error) => return error,
        },
    };
    // `--repo owner/repo` makes GitHub CLI independent from a Git working tree.
    // When the repository is explicit, run from a safe temp directory so a
    // rotating macOS runtime never re-enters a TCC-protected project merely to
    // inspect GitHub state. The managed project_root gate above still scopes
    // who may invoke this private status helper.
    let gh_root = if object
        .get("repository")
        .is_some_and(|value| !value.is_null())
    {
        env::temp_dir()
    } else {
        root.clone()
    };
    let (repository_json, pr_json, all_checks, required_checks) = if let Some(number) = pr_number {
        let (repository_json, pr_json, pr_id) =
            match fetch_pr_metadata(&gh, &gh_root, &repository, number) {
                Ok(value) => value,
                Err(error) => return error,
            };
        let (all_checks, required_checks) = match fetch_pr_checks(&gh, &gh_root, &pr_id, &pr_json) {
            Ok(value) => value,
            Err(error) => return error,
        };
        (
            repository_json,
            Some(pr_json),
            Some(all_checks),
            Some(required_checks),
        )
    } else {
        let repo_api_path = format!("repos/{repository}");
        let repository_json = match run_gh_json(&gh, &gh_root, &["api", &repo_api_path]) {
            Ok(value) => value,
            Err(error) => return error,
        };
        (repository_json, None, None, None)
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

fn managed_project_root(snapshot: &Value, root_raw: &str) -> Result<PathBuf, Value> {
    let topology = crate::projects::derive_routing(snapshot);
    crate::fs_security::validate_exact_project_root_with_topology(&topology, root_raw)
        .map(|managed| managed.root)
        .map_err(|_| {
            json!({
                "ok": false,
                "code": "project_root_not_managed",
                "message": "project_root must match a project/worktree in the live Herdr snapshot",
                "project_root": root_raw,
            })
        })
}

pub(crate) fn github_repository(root: &Path) -> Result<String, Value> {
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

fn parse_repository_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 200 || value.chars().any(char::is_whitespace) {
        return None;
    }
    let mut parts = value.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if parts.next().is_some() || owner.is_empty() || repo.is_empty() {
        return None;
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    if !owner.chars().all(allowed) || !repo.chars().all(allowed) {
        return None;
    }
    if matches!(owner, "." | "..") || matches!(repo, "." | "..") {
        return None;
    }
    Some(format!("{owner}/{repo}"))
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

fn fetch_pr_metadata(
    gh: &Path,
    root: &Path,
    repository: &str,
    number: u64,
) -> Result<(Value, Value, String), Value> {
    let Some((owner, name)) = repository.split_once('/') else {
        return Err(command_error(
            "unsupported_git_remote",
            "GitHub repository identity must be owner/name".to_owned(),
        ));
    };
    let query = format!("query={PR_STATUS_META_QUERY}");
    let owner = format!("owner={owner}");
    let name = format!("name={name}");
    let number = format!("number={number}");
    let response = run_gh_json(
        gh,
        root,
        &[
            "api", "graphql", "-f", &query, "-F", &owner, "-F", &name, "-F", &number,
        ],
    )?;
    reject_graphql_errors(&response)?;
    parse_pr_metadata_response(&response)
}

fn parse_pr_metadata_response(response: &Value) -> Result<(Value, Value, String), Value> {
    let Some(repository_json) = response.pointer("/data/repository") else {
        return Err(graphql_shape_error("repository metadata is missing"));
    };
    let Some(pr_json) = repository_json
        .get("pullRequest")
        .filter(|value| !value.is_null())
    else {
        return Err(command_error(
            "gh_command_failed",
            "pull request was not found in the GitHub repository".to_owned(),
        ));
    };
    let Some(pr_id) = pr_json.get("id").and_then(Value::as_str) else {
        return Err(graphql_shape_error("pull request node id is missing"));
    };
    let repository_summary = json!({
        "allow_auto_merge": repository_json.get("autoMergeAllowed").cloned().unwrap_or(Value::Null),
        "default_branch": repository_json.pointer("/defaultBranchRef/name").cloned().unwrap_or(Value::Null),
    });
    Ok((repository_summary, pr_json.clone(), pr_id.to_owned()))
}

fn fetch_pr_checks(
    gh: &Path,
    root: &Path,
    pr_id: &str,
    pr_json: &Value,
) -> Result<(Value, Value), Value> {
    let mut contexts = Vec::new();
    let mut cursor: Option<String> = None;
    let mut completed = false;
    for _ in 0..MAX_CHECK_PAGES {
        let response = fetch_pr_check_page(gh, root, pr_id, cursor.as_deref())?;
        reject_graphql_errors(&response)?;
        let Some(page) =
            response.pointer("/data/node/commits/nodes/0/commit/statusCheckRollup/contexts")
        else {
            return Err(graphql_shape_error(
                "pull request check contexts are missing",
            ));
        };
        let Some(nodes) = page.get("nodes").and_then(Value::as_array) else {
            return Err(graphql_shape_error("pull request check nodes are invalid"));
        };
        contexts.extend(nodes.iter().cloned());
        let has_next = page
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .ok_or_else(|| graphql_shape_error("check pagination flag is missing"))?;
        if !has_next {
            completed = true;
            break;
        }
        let Some(next) = page
            .pointer("/pageInfo/endCursor")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            return Err(graphql_shape_error("check pagination cursor is missing"));
        };
        cursor = Some(next.to_owned());
    }
    if !completed {
        return Err(command_error(
            "gh_command_failed",
            format!("pull request check pagination exceeded {MAX_CHECK_PAGES} pages"),
        ));
    }
    let head = pr_json
        .get("headRefName")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    normalize_check_contexts(contexts, head)
}

fn fetch_pr_check_page(
    gh: &Path,
    root: &Path,
    pr_id: &str,
    cursor: Option<&str>,
) -> Result<Value, Value> {
    let query = format!("query={PR_STATUS_CHECKS_QUERY}");
    let id = format!("id={pr_id}");
    if let Some(cursor) = cursor {
        let cursor = format!("endCursor={cursor}");
        run_gh_json(
            gh,
            root,
            &["api", "graphql", "-f", &query, "-F", &id, "-F", &cursor],
        )
    } else {
        run_gh_json(gh, root, &["api", "graphql", "-f", &query, "-F", &id])
    }
}

fn normalize_check_contexts(mut contexts: Vec<Value>, head: &str) -> Result<(Value, Value), Value> {
    contexts.sort_by(|left, right| check_started_at(right).cmp(check_started_at(left)));
    let mut seen = BTreeSet::new();
    let mut all = Vec::new();
    let mut required = Vec::new();
    for context in contexts {
        let key = check_identity(&context)?;
        if !seen.insert(key) {
            continue;
        }
        let is_required = context
            .get("isRequired")
            .and_then(Value::as_bool)
            .ok_or_else(|| graphql_shape_error("check required-state is missing"))?;
        let row = normalize_check_context(&context)?;
        if is_required {
            required.push(row.clone());
        }
        all.push(row);
    }
    if all.is_empty() {
        return Err(command_error(
            "gh_command_failed",
            format!("no checks reported on the '{head}' branch"),
        ));
    }
    if required.is_empty() {
        return Err(command_error(
            "gh_command_failed",
            format!("no required checks reported on the '{head}' branch"),
        ));
    }
    Ok((Value::Array(all), Value::Array(required)))
}

fn check_started_at(context: &Value) -> &str {
    context
        .get("startedAt")
        .or_else(|| context.get("createdAt"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn check_identity(context: &Value) -> Result<String, Value> {
    match context
        .get("__typename")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "StatusContext" => context
            .get("context")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| format!("status:{value}"))
            .ok_or_else(|| graphql_shape_error("status context name is missing")),
        "CheckRun" => {
            let name = context
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| graphql_shape_error("check run name is missing"))?;
            let workflow = context
                .pointer("/checkSuite/workflowRun/workflow/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let event = context
                .pointer("/checkSuite/workflowRun/event")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(format!("check:{name}/{workflow}/{event}"))
        }
        _ => Err(graphql_shape_error("unknown status check context type")),
    }
}

fn normalize_check_context(context: &Value) -> Result<Value, Value> {
    match context
        .get("__typename")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "StatusContext" => {
            let name = context
                .get("context")
                .and_then(Value::as_str)
                .ok_or_else(|| graphql_shape_error("status context name is missing"))?;
            let state = context
                .get("state")
                .and_then(Value::as_str)
                .ok_or_else(|| graphql_shape_error("status context state is missing"))?;
            Ok(json!({
                "name": name,
                "state": state,
                "bucket": check_bucket(state),
                "link": context.get("targetUrl").cloned().unwrap_or(Value::Null),
                "workflow": "",
            }))
        }
        "CheckRun" => {
            let name = context
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| graphql_shape_error("check run name is missing"))?;
            let status = context
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(|| graphql_shape_error("check run status is missing"))?;
            let state = if status == "COMPLETED" {
                context
                    .get("conclusion")
                    .and_then(Value::as_str)
                    .unwrap_or(status)
            } else {
                status
            };
            Ok(json!({
                "name": name,
                "state": state,
                "bucket": check_bucket(state),
                "link": context.get("detailsUrl").cloned().unwrap_or(Value::Null),
                "workflow": context.pointer("/checkSuite/workflowRun/workflow/name").cloned().unwrap_or(Value::String(String::new())),
            }))
        }
        _ => Err(graphql_shape_error("unknown status check context type")),
    }
}

fn check_bucket(state: &str) -> &'static str {
    match state {
        "SUCCESS" => "pass",
        "SKIPPED" | "NEUTRAL" => "skipping",
        "ERROR" | "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" => "fail",
        "CANCELLED" => "cancel",
        _ => "pending",
    }
}

fn reject_graphql_errors(response: &Value) -> Result<(), Value> {
    if response
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(command_error(
            "gh_command_failed",
            "GitHub GraphQL returned errors".to_owned(),
        ));
    }
    Ok(())
}

fn graphql_shape_error(message: &str) -> Value {
    command_error("github_status_shape_invalid", message.to_owned())
}

pub(crate) fn find_gh() -> Option<PathBuf> {
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

pub(crate) fn run_gh_json(gh: &Path, root: &Path, args: &[&str]) -> Result<Value, Value> {
    run_gh_json_inner(gh, root, args, &[])
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
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

    fn temp_repo() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_REPO.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "herdr-github-status-{}-{unique}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        root
    }

    #[test]
    fn parses_explicit_repository_names() {
        for value in [
            "whshang/herdr-mcp",
            " whshang/herdr-mcp ",
            "owner.with-dots/repo_name-1",
        ] {
            assert_eq!(parse_repository_name(value).as_deref(), Some(value.trim()));
        }
        for value in [
            "",
            "owner",
            "owner/repo/extra",
            "https://github.com/owner/repo",
            "owner/repo with space",
            "../repo",
            "owner/..",
        ] {
            assert_eq!(parse_repository_name(value), None, "{value}");
        }
    }

    #[test]
    fn git_remote_probe_budget_absorbs_scheduler_jitter_without_unbounded_wait() {
        assert_eq!(GIT_TIMEOUT, Duration::from_secs(3));
        assert_eq!(GIT_TIMEOUT, GH_TIMEOUT);
    }

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

    #[cfg(unix)]
    #[test]
    fn single_page_pr_status_uses_exactly_two_gh_commands() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_repo();
        let gh = root.join("fake-gh");
        let script = r#"#!/bin/sh
+LOG="$(dirname "$0")/gh-calls.log"
+printf 'call\n' >> "$LOG"
+COUNT=$(wc -l < "$LOG" | tr -d ' ')
+if [ "$COUNT" = "1" ]; then
+  printf '%s\n' '{"data":{"repository":{"autoMergeAllowed":true,"defaultBranchRef":{"name":"main"},"pullRequest":{"id":"PR_1","number":401,"state":"OPEN","isDraft":false,"mergeable":"MERGEABLE","mergeStateStatus":"BLOCKED","headRefOid":"abc","baseRefOid":"def","headRefName":"feature","baseRefName":"main","autoMergeRequest":null,"url":"https://github.com/o/r/pull/401"}}}}'
+elif [ "$COUNT" = "2" ]; then
+  printf '%s\n' '{"data":{"node":{"commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[{"__typename":"CheckRun","name":"rust","checkSuite":{"workflowRun":{"event":"pull_request","workflow":{"name":"CI"}}},"status":"COMPLETED","conclusion":"SUCCESS","startedAt":"2026-09-12T07:02:00Z","completedAt":"2026-09-12T07:03:00Z","detailsUrl":"rust","isRequired":true},{"__typename":"StatusContext","context":"deploy/relay","state":"SUCCESS","targetUrl":"relay","createdAt":"2026-09-12T07:01:00Z","description":"ok","isRequired":false}],"pageInfo":{"hasNextPage":false,"endCursor":"2"}}}}}]}}}}'
+else
+  exit 99
+fi
+"#;
        fs::write(&gh, script.replace("\n+", "\n")).unwrap();
        let mut permissions = fs::metadata(&gh).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&gh, permissions).unwrap();

        let (repo, pr, pr_id) = fetch_pr_metadata(&gh, &root, "o/r", 401).unwrap();
        let (all, required) = fetch_pr_checks(&gh, &root, &pr_id, &pr).unwrap();
        let calls = fs::read_to_string(root.join("gh-calls.log")).unwrap();
        assert_eq!(calls.lines().count(), 2);
        assert_eq!(repo["default_branch"], "main");
        assert_eq!(all.as_array().unwrap().len(), 2);
        assert_eq!(required.as_array().unwrap().len(), 1);
        assert_eq!(required[0]["name"], "rust");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn graphql_metadata_preserves_repository_and_pr_shape() {
        let response = json!({
            "data": {
                "repository": {
                    "autoMergeAllowed": true,
                    "defaultBranchRef": {"name": "main"},
                    "pullRequest": {
                        "id": "PR_1",
                        "number": 401,
                        "state": "OPEN",
                        "isDraft": false,
                        "mergeable": "MERGEABLE",
                        "mergeStateStatus": "BLOCKED",
                        "headRefOid": "abc",
                        "baseRefOid": "def",
                        "headRefName": "feature",
                        "baseRefName": "main",
                        "autoMergeRequest": null,
                        "url": "https://github.com/o/r/pull/401"
                    }
                }
            }
        });
        let (repo, pr, pr_id) = parse_pr_metadata_response(&response).unwrap();
        assert_eq!(repo["allow_auto_merge"], true);
        assert_eq!(repo["default_branch"], "main");
        assert_eq!(pr["number"], 401);
        assert_eq!(pr["headRefName"], "feature");
        assert_eq!(pr_id, "PR_1");
    }

    #[test]
    fn graphql_checks_preserve_event_identity_required_state_and_latest_duplicate() {
        let contexts = vec![
            json!({
                "__typename": "CheckRun",
                "name": "rust",
                "checkSuite": {"workflowRun": {"event": "pull_request", "workflow": {"name": "CI"}}},
                "status": "COMPLETED",
                "conclusion": "FAILURE",
                "startedAt": "2026-09-12T07:00:00Z",
                "detailsUrl": "old",
                "isRequired": true
            }),
            json!({
                "__typename": "CheckRun",
                "name": "rust",
                "checkSuite": {"workflowRun": {"event": "pull_request", "workflow": {"name": "CI"}}},
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "startedAt": "2026-09-12T07:02:00Z",
                "detailsUrl": "new",
                "isRequired": true
            }),
            json!({
                "__typename": "CheckRun",
                "name": "rust",
                "checkSuite": {"workflowRun": {"event": "push", "workflow": {"name": "CI"}}},
                "status": "COMPLETED",
                "conclusion": "FAILURE",
                "startedAt": "2026-09-12T07:01:00Z",
                "detailsUrl": "push",
                "isRequired": false
            }),
            json!({
                "__typename": "StatusContext",
                "context": "deploy/relay",
                "state": "SUCCESS",
                "targetUrl": "relay",
                "createdAt": "2026-09-12T07:01:00Z",
                "isRequired": false
            }),
        ];
        let (all, required) = normalize_check_contexts(contexts, "feature").unwrap();
        let all = all.as_array().unwrap();
        let required = required.as_array().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(required.len(), 1);
        assert_eq!(required[0]["name"], "rust");
        assert_eq!(required[0]["state"], "SUCCESS");
        assert_eq!(required[0]["link"], "new");
        assert_eq!(all.iter().filter(|row| row["name"] == "rust").count(), 2);
        assert!(
            all.iter()
                .any(|row| row["link"] == "push" && row["bucket"] == "fail")
        );
        assert!(
            all.iter()
                .any(|row| row["name"] == "deploy/relay" && row["bucket"] == "pass")
        );
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
    fn project_root_uses_the_shared_managed_project_topology() {
        let root = temp_repo();
        let snapshot = json!({
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": root.to_string_lossy(),
            }],
            "agents": []
        });

        assert_eq!(
            managed_project_root(&snapshot, root.to_str().unwrap()).unwrap(),
            root
        );
        let other = root.parent().unwrap().join("not-the-project");
        let error = managed_project_root(&snapshot, other.to_str().unwrap()).unwrap_err();
        assert_eq!(error["code"], "project_root_not_managed");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_documented_pending_exit_is_accepted_for_check_json() {
        assert!(accept_json_exit(true, Some(0), &[8]));
        assert!(accept_json_exit(false, Some(8), &[8]));
        assert!(!accept_json_exit(false, Some(1), &[8]));
        assert!(!accept_json_exit(false, None, &[8]));
    }
}
