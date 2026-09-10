use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const GIT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_COMMAND_BYTES: usize = 512 * 1024;
const MAX_STDERR_BYTES: usize = 2048;
const DEFAULT_TARGET_REF: &str = "origin/main";

#[derive(Clone, Debug)]
struct OpenPr {
    number: u64,
    head: String,
    base: String,
    url: Option<String>,
    draft: bool,
}

impl OpenPr {
    fn references(&self, branch: &str) -> bool {
        self.head == branch || self.base == branch
    }

    fn to_json(&self) -> Value {
        json!({
            "number": self.number,
            "head": self.head,
            "base": self.base,
            "url": self.url,
            "draft": self.draft,
        })
    }
}

#[derive(Clone, Debug, Default)]
struct GithubEvidence {
    available: bool,
    repository: Option<String>,
    remote_branches: BTreeMap<String, String>,
    open_prs: Vec<OpenPr>,
    error: Option<Value>,
}

#[derive(Clone, Debug)]
struct BranchRef {
    full_ref: String,
    name: String,
    oid: String,
    scope: &'static str,
}

#[derive(Clone, Debug, Default)]
struct RawWorktree {
    path: Option<PathBuf>,
    head: Option<String>,
    branch_ref: Option<String>,
    detached: bool,
    bare: bool,
    locked: bool,
    prunable: bool,
}

#[derive(Clone, Debug)]
struct WorktreeRecord {
    path: PathBuf,
    head: Option<String>,
    branch_ref: Option<String>,
    detached: bool,
    bare: bool,
    locked: bool,
    prunable: bool,
    primary: bool,
}

pub fn preview(params: &Value, snapshot: &Value) -> Value {
    let Some(object) = params.as_object() else {
        return invalid_params("params must be an object");
    };
    let allowed = ["project_root", "target_ref"]
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
            "message": "unknown cleanup preview params",
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

    let target_ref = match object.get("target_ref") {
        None | Some(Value::Null) => DEFAULT_TARGET_REF,
        Some(value) => match value.as_str() {
            Some(text) if !text.trim().is_empty() => text.trim(),
            _ => return invalid_params("target_ref must be a non-empty string"),
        },
    };

    let github = github_evidence(&root);
    render_preview(&root, target_ref, snapshot, github)
}

fn render_preview(
    root: &Path,
    target_ref: &str,
    snapshot: &Value,
    github: GithubEvidence,
) -> Value {
    let target_expr = format!("{target_ref}^{{commit}}");
    let target_oid = match run_git_text(
        root,
        &["rev-parse", "--verify", "--end-of-options", &target_expr],
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let target_full_ref = run_git_text(
        root,
        &[
            "rev-parse",
            "--symbolic-full-name",
            "--end-of-options",
            target_ref,
        ],
    )
    .ok()
    .filter(|value| !value.is_empty());
    let target_branch = target_full_ref
        .as_deref()
        .and_then(logical_branch_name)
        .or_else(|| logical_branch_name(target_ref))
        .map(str::to_owned);

    let branch_text = match run_git_text(
        root,
        &[
            "for-each-ref",
            "--format=%(refname)%09%(objectname)",
            "refs/heads",
            "refs/remotes/origin",
        ],
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let branches = parse_branch_refs(&branch_text);

    let merged_arg = format!("--merged={target_oid}");
    let merged_text = match run_git_text(
        root,
        &[
            "for-each-ref",
            &merged_arg,
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes/origin",
        ],
    ) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let merged_refs = merged_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

    let worktree_text = match run_git_text(root, &["worktree", "list", "--porcelain"]) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let worktrees = parse_worktrees(&worktree_text);
    let checked_out_by_branch = checked_out_paths(&worktrees);

    let resource_evidence_complete = snapshot.get("workspaces").is_some_and(Value::is_array)
        && snapshot.get("agents").is_some_and(Value::is_array);
    let target_ref_fresh =
        target_remote_branch(target_ref, target_full_ref.as_deref()).map_or(Some(true), |branch| {
            if !github.available {
                None
            } else {
                Some(
                    github
                        .remote_branches
                        .get(branch)
                        .is_some_and(|oid| oid == &target_oid),
                )
            }
        });

    let mut worktree_json = Vec::new();
    for worktree in &worktrees {
        let dirty = worktree_dirty(&worktree.path).ok();
        let branch_name = worktree
            .branch_ref
            .as_deref()
            .and_then(logical_branch_name)
            .map(str::to_owned);
        let reachable = if let Some(branch_ref) = worktree.branch_ref.as_deref() {
            Some(merged_refs.contains(branch_ref))
        } else if let Some(head) = worktree.head.as_deref() {
            is_ancestor(root, head, &target_oid).ok()
        } else {
            None
        };
        let pr_refs = branch_name
            .as_deref()
            .map(|name| open_pr_refs(&github.open_prs, name))
            .unwrap_or_default();
        let workspace_ids = workspace_ids_for_path(snapshot, &worktree.path);
        let agents = agents_for_path(snapshot, &worktree.path);

        let mut reasons = Vec::<&str>::new();
        if worktree.primary {
            reasons.push("primary_worktree");
        }
        if worktree.bare {
            reasons.push("bare_worktree");
        }
        if worktree.locked {
            reasons.push("worktree_locked");
        }
        if worktree.prunable {
            reasons.push("worktree_prunable");
        }
        match dirty {
            Some(true) => reasons.push("worktree_dirty"),
            Some(false) => {}
            None => reasons.push("worktree_status_unavailable"),
        }
        match reachable {
            Some(true) => {}
            Some(false) => reasons.push("not_reachable_from_target"),
            None => reasons.push("reachability_unavailable"),
        }
        if !github.available {
            reasons.push("github_evidence_unavailable");
        } else {
            if target_ref_fresh == Some(false) {
                reasons.push("target_ref_not_fresh");
            }
            if !pr_refs.is_empty() {
                reasons.push("open_pr_reference");
            }
        }
        if !resource_evidence_complete {
            reasons.push("resource_evidence_unavailable");
        } else {
            if !workspace_ids.is_empty() {
                reasons.push("workspace_present");
            }
            if !agents.is_empty() {
                reasons.push("agent_present");
            }
        }

        let content_safe = dirty == Some(false)
            && reachable == Some(true)
            && github.available
            && target_ref_fresh != Some(false)
            && pr_refs.is_empty();
        let safe_to_delete = reasons.is_empty();
        worktree_json.push(json!({
            "path": worktree.path,
            "head": worktree.head,
            "branch": branch_name,
            "branch_ref": worktree.branch_ref,
            "primary": worktree.primary,
            "detached": worktree.detached,
            "bare": worktree.bare,
            "locked": worktree.locked,
            "prunable": worktree.prunable,
            "dirty": dirty,
            "reachable_from_target": reachable,
            "workspace_ids": workspace_ids,
            "agents": agents,
            "open_pr_refs": pr_refs,
            "content_safe": content_safe,
            "safe_to_delete": safe_to_delete,
            "reasons": reasons,
        }));
    }

    let mut branch_json = Vec::new();
    for branch in &branches {
        let reachable = merged_refs.contains(&branch.full_ref);
        let pr_refs = open_pr_refs(&github.open_prs, &branch.name);
        let checked_out_in = checked_out_by_branch
            .get(&branch.name)
            .cloned()
            .unwrap_or_default();
        let remote_ref_fresh = if branch.scope == "remote" {
            if github.available {
                Some(
                    github
                        .remote_branches
                        .get(&branch.name)
                        .is_some_and(|oid| oid == &branch.oid),
                )
            } else {
                None
            }
        } else {
            None
        };

        let mut reasons = Vec::<&str>::new();
        if target_branch.as_deref() == Some(branch.name.as_str()) {
            reasons.push("target_branch");
        }
        if !reachable {
            reasons.push("not_reachable_from_target");
        }
        if !github.available {
            reasons.push("github_evidence_unavailable");
        } else {
            if target_ref_fresh == Some(false) {
                reasons.push("target_ref_not_fresh");
            }
            if !pr_refs.is_empty() {
                reasons.push("open_pr_reference");
            }
            if remote_ref_fresh == Some(false) {
                reasons.push("remote_ref_not_fresh");
            }
        }
        if !checked_out_in.is_empty() {
            reasons.push("checked_out_in_worktree");
        }
        if !resource_evidence_complete {
            reasons.push("resource_evidence_unavailable");
        }

        branch_json.push(json!({
            "name": branch.name,
            "ref": branch.full_ref,
            "scope": branch.scope,
            "oid": branch.oid,
            "reachable_from_target": reachable,
            "remote_ref_fresh": remote_ref_fresh,
            "checked_out_in": checked_out_in,
            "open_pr_refs": pr_refs,
            "safe_to_delete": reasons.is_empty(),
            "reasons": reasons,
        }));
    }

    let safe_worktrees = worktree_json
        .iter()
        .filter(|item| item.get("safe_to_delete").and_then(Value::as_bool) == Some(true))
        .count();
    let content_safe_worktrees = worktree_json
        .iter()
        .filter(|item| item.get("content_safe").and_then(Value::as_bool) == Some(true))
        .count();
    let safe_branches = branch_json
        .iter()
        .filter(|item| item.get("safe_to_delete").and_then(Value::as_bool) == Some(true))
        .count();

    json!({
        "ok": true,
        "effect": "read_only_preview",
        "project_root": root,
        "target_ref": target_ref,
        "target_full_ref": target_full_ref,
        "target_oid": target_oid,
        "target_ref_fresh": target_ref_fresh,
        "resource_evidence_complete": resource_evidence_complete,
        "github": {
            "available": github.available,
            "repository": github.repository,
            "remote_branch_count": github.remote_branches.len(),
            "open_pr_count": github.open_prs.len(),
            "error": github.error,
        },
        "summary": {
            "worktrees": worktree_json.len(),
            "content_safe_worktrees": content_safe_worktrees,
            "safe_worktrees": safe_worktrees,
            "branches": branch_json.len(),
            "safe_branches": safe_branches,
        },
        "worktrees": worktree_json,
        "branches": branch_json,
        "safety": {
            "mutation_performed": false,
            "fail_closed": true,
            "note": "safe_to_delete is true only when local Git, GitHub, target freshness, and live Herdr resource evidence all satisfy the conservative reclaim checks; re-check immediately before mutation",
        },
    })
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

fn github_evidence(root: &Path) -> GithubEvidence {
    let repository = match crate::github_status::github_repository(root) {
        Ok(repository) => repository,
        Err(error) => return unavailable_github(None, error),
    };
    let Some(gh) = crate::github_status::find_gh() else {
        return unavailable_github(
            Some(repository),
            json!({
                "ok": false,
                "code": "gh_unavailable",
                "message": "GitHub CLI (gh) is not available on the runtime PATH or standard install locations",
            }),
        );
    };

    let pulls_endpoint = format!("repos/{repository}/pulls?state=open&per_page=100");
    let pulls = match crate::github_status::run_gh_json(
        &gh,
        root,
        &["api", "--paginate", "--slurp", &pulls_endpoint],
    ) {
        Ok(value) => value,
        Err(error) => return unavailable_github(Some(repository), error),
    };
    let branches_endpoint = format!("repos/{repository}/branches?per_page=100");
    let remote_branches_json = match crate::github_status::run_gh_json(
        &gh,
        root,
        &["api", "--paginate", "--slurp", &branches_endpoint],
    ) {
        Ok(value) => value,
        Err(error) => return unavailable_github(Some(repository), error),
    };

    GithubEvidence {
        available: true,
        repository: Some(repository),
        remote_branches: parse_remote_branches(&remote_branches_json),
        open_prs: parse_open_prs(&pulls),
        error: None,
    }
}

fn unavailable_github(repository: Option<String>, error: Value) -> GithubEvidence {
    GithubEvidence {
        available: false,
        repository,
        remote_branches: BTreeMap::new(),
        open_prs: Vec::new(),
        error: Some(error),
    }
}

fn parse_remote_branches(value: &Value) -> BTreeMap<String, String> {
    fn visit(value: &Value, branches: &mut BTreeMap<String, String>) {
        let Some(array) = value.as_array() else {
            return;
        };
        for item in array {
            if let (Some(name), Some(sha)) = (
                item.get("name").and_then(Value::as_str),
                item.get("commit")
                    .and_then(|commit| commit.get("sha"))
                    .and_then(Value::as_str),
            ) {
                branches.insert(name.to_owned(), sha.to_owned());
            } else if item.is_array() {
                visit(item, branches);
            }
        }
    }

    let mut branches = BTreeMap::new();
    visit(value, &mut branches);
    branches
}

fn parse_open_prs(value: &Value) -> Vec<OpenPr> {
    fn visit(value: &Value, prs: &mut Vec<OpenPr>) {
        let Some(array) = value.as_array() else {
            return;
        };
        for item in array {
            let number = item.get("number").and_then(Value::as_u64);
            let head = item
                .get("head")
                .and_then(|head| head.get("ref"))
                .and_then(Value::as_str);
            let base = item
                .get("base")
                .and_then(|base| base.get("ref"))
                .and_then(Value::as_str);
            if let (Some(number), Some(head), Some(base)) = (number, head, base) {
                prs.push(OpenPr {
                    number,
                    head: head.to_owned(),
                    base: base.to_owned(),
                    url: item
                        .get("html_url")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    draft: item.get("draft").and_then(Value::as_bool).unwrap_or(false),
                });
            } else if item.is_array() {
                visit(item, prs);
            }
        }
    }

    let mut prs = Vec::new();
    visit(value, &mut prs);
    prs
}

fn parse_branch_refs(text: &str) -> Vec<BranchRef> {
    text.lines()
        .filter_map(|line| {
            let (full_ref, oid) = line.split_once('\t')?;
            if full_ref == "refs/remotes/origin/HEAD" {
                return None;
            }
            if let Some(name) = full_ref.strip_prefix("refs/heads/") {
                return Some(BranchRef {
                    full_ref: full_ref.to_owned(),
                    name: name.to_owned(),
                    oid: oid.to_owned(),
                    scope: "local",
                });
            }
            full_ref
                .strip_prefix("refs/remotes/origin/")
                .map(|name| BranchRef {
                    full_ref: full_ref.to_owned(),
                    name: name.to_owned(),
                    oid: oid.to_owned(),
                    scope: "remote",
                })
        })
        .collect()
}

fn parse_worktrees(text: &str) -> Vec<WorktreeRecord> {
    fn finish(raw: RawWorktree, primary: bool, out: &mut Vec<WorktreeRecord>) {
        let Some(path) = raw.path else {
            return;
        };
        out.push(WorktreeRecord {
            path,
            head: raw.head,
            branch_ref: raw.branch_ref,
            detached: raw.detached,
            bare: raw.bare,
            locked: raw.locked,
            prunable: raw.prunable,
            primary,
        });
    }

    let mut out = Vec::new();
    let mut raw = RawWorktree::default();
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if raw.path.is_some() {
                let primary = out.is_empty();
                finish(std::mem::take(&mut raw), primary, &mut out);
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            raw.path = Some(PathBuf::from(path));
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            raw.head = Some(head.to_owned());
        } else if let Some(branch) = line.strip_prefix("branch ") {
            raw.branch_ref = Some(branch.to_owned());
        } else if line == "detached" {
            raw.detached = true;
        } else if line == "bare" {
            raw.bare = true;
        } else if line == "locked" || line.starts_with("locked ") {
            raw.locked = true;
        } else if line == "prunable" || line.starts_with("prunable ") {
            raw.prunable = true;
        }
    }
    out
}

fn checked_out_paths(worktrees: &[WorktreeRecord]) -> BTreeMap<String, Vec<PathBuf>> {
    let mut by_branch = BTreeMap::<String, Vec<PathBuf>>::new();
    for worktree in worktrees {
        let Some(name) = worktree.branch_ref.as_deref().and_then(logical_branch_name) else {
            continue;
        };
        by_branch
            .entry(name.to_owned())
            .or_default()
            .push(worktree.path.clone());
    }
    by_branch
}

fn logical_branch_name(reference: &str) -> Option<&str> {
    reference
        .strip_prefix("refs/heads/")
        .or_else(|| reference.strip_prefix("refs/remotes/origin/"))
        .or_else(|| reference.strip_prefix("origin/"))
        .or_else(|| (!reference.starts_with("refs/")).then_some(reference))
}

fn target_remote_branch<'a>(
    target_ref: &'a str,
    target_full_ref: Option<&'a str>,
) -> Option<&'a str> {
    target_full_ref
        .and_then(|reference| reference.strip_prefix("refs/remotes/origin/"))
        .or_else(|| target_ref.strip_prefix("origin/"))
        .or_else(|| target_ref.strip_prefix("refs/remotes/origin/"))
}

fn open_pr_refs(prs: &[OpenPr], branch: &str) -> Vec<Value> {
    prs.iter()
        .filter(|pr| pr.references(branch))
        .map(OpenPr::to_json)
        .collect()
}

fn workspace_ids_for_path(snapshot: &Value, worktree: &Path) -> Vec<String> {
    let mut ids = BTreeSet::new();
    let Some(workspaces) = snapshot.get("workspaces").and_then(Value::as_array) else {
        return Vec::new();
    };
    for workspace in workspaces {
        let mut candidates = Vec::<&str>::new();
        if let Some(path) = workspace
            .get("worktree")
            .and_then(|worktree| worktree.get("checkout_path"))
            .and_then(Value::as_str)
        {
            candidates.push(path);
        }
        if let Some(path) = workspace.get("cwd").and_then(Value::as_str) {
            candidates.push(path);
        }
        if let Some(projects) = workspace.get("projects").and_then(Value::as_array) {
            for project in projects {
                if let Some(path) = project.get("root").and_then(Value::as_str) {
                    candidates.push(path);
                }
            }
        }
        if candidates
            .iter()
            .any(|candidate| path_is_same_or_descendant(worktree, candidate))
            && let Some(id) = workspace
                .get("workspace_id")
                .or_else(|| workspace.get("id"))
                .and_then(Value::as_str)
        {
            ids.insert(id.to_owned());
        }
    }
    ids.into_iter().collect()
}

fn agents_for_path(snapshot: &Value, worktree: &Path) -> Vec<Value> {
    let Some(agents) = snapshot.get("agents").and_then(Value::as_array) else {
        return Vec::new();
    };
    agents
        .iter()
        .filter(|agent| {
            agent
                .get("cwd")
                .and_then(Value::as_str)
                .is_some_and(|cwd| path_is_same_or_descendant(worktree, cwd))
        })
        .map(|agent| {
            json!({
                "name": agent.get("name").cloned().unwrap_or(Value::Null),
                "kind": agent.get("kind").or_else(|| agent.get("agent")).cloned().unwrap_or(Value::Null),
                "status": agent.get("agent_status").or_else(|| agent.get("status")).cloned().unwrap_or(Value::Null),
                "pane_id": agent.get("pane_id").or_else(|| agent.get("pane")).cloned().unwrap_or(Value::Null),
                "workspace_id": agent.get("workspace_id").or_else(|| agent.get("workspace")).cloned().unwrap_or(Value::Null),
            })
        })
        .collect()
}

fn path_is_same_or_descendant(root: &Path, candidate: &str) -> bool {
    let root = canonical_or_original(root);
    let candidate = canonical_or_original(Path::new(candidate));
    candidate == root || candidate.starts_with(&root)
}

fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn worktree_dirty(path: &Path) -> Result<bool, Value> {
    let stdout = run_git_bytes(path, &["status", "--porcelain=v1", "-z"])?;
    Ok(!stdout.is_empty())
}

fn is_ancestor(root: &Path, ancestor: &str, target: &str) -> Result<bool, Value> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["merge-base", "--is-ancestor", ancestor, target]);
    let output =
        crate::child_process::run_bounded_output(&mut command, GIT_TIMEOUT, MAX_COMMAND_BYTES)
            .map_err(|error| command_error("git_exec_failed", error.to_string()))?
            .ok_or_else(|| {
                command_error("git_timeout", "git ancestry probe timed out".to_owned())
            })?;
    if output.truncated {
        return Err(command_error(
            "git_output_too_large",
            "git ancestry probe exceeded the bounded capture budget".to_owned(),
        ));
    }
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(command_error(
            "git_command_failed",
            bounded_stderr(&output.stderr),
        )),
    }
}

fn run_git_text(root: &Path, args: &[&str]) -> Result<String, Value> {
    let stdout = run_git_bytes(root, args)?;
    Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
}

fn run_git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>, Value> {
    let mut command = Command::new("git");
    command.arg("-C").arg(root).args(args);
    let output =
        crate::child_process::run_bounded_output(&mut command, GIT_TIMEOUT, MAX_COMMAND_BYTES)
            .map_err(|error| command_error("git_exec_failed", error.to_string()))?
            .ok_or_else(|| {
                command_error("git_timeout", "git cleanup preview timed out".to_owned())
            })?;
    if output.truncated {
        return Err(command_error(
            "git_output_too_large",
            "git cleanup preview output exceeded the bounded capture budget".to_owned(),
        ));
    }
    if !output.status.success() {
        return Err(command_error(
            "git_command_failed",
            bounded_stderr(&output.stderr),
        ));
    }
    Ok(output.stdout)
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

    fn temp_root(label: &str) -> PathBuf {
        let nonce = NEXT_REPO.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "herdr-cleanup-preview-{label}-{}-{timestamp}-{nonce}",
            std::process::id()
        ))
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn repo_with_reclaimable_worktree() -> (PathBuf, PathBuf, GithubEvidence) {
        let root = temp_root("repo");
        let linked = temp_root("linked");
        fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-b", "main"])
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "user.name", "Herdr Test"]);
        fs::write(root.join("base.txt"), "one\n").unwrap();
        git(&root, &["add", "base.txt"]);
        git(&root, &["commit", "-m", "base"]);
        let old_oid = git(&root, &["rev-parse", "HEAD"]);
        git(&root, &["branch", "old", &old_oid]);
        fs::write(root.join("main.txt"), "two\n").unwrap();
        git(&root, &["add", "main.txt"]);
        git(&root, &["commit", "-m", "main"]);
        let main_oid = git(&root, &["rev-parse", "HEAD"]);
        git(
            &root,
            &["update-ref", "refs/remotes/origin/main", &main_oid],
        );
        git(&root, &["update-ref", "refs/remotes/origin/old", &old_oid]);
        git(&root, &["worktree", "add", linked.to_str().unwrap(), "old"]);

        // macOS reports worktrees beneath /var through their canonical
        // /private/var path. Keep test identity aligned with Git metadata.
        let root = fs::canonicalize(root).unwrap();
        let linked = fs::canonicalize(linked).unwrap();

        let github = GithubEvidence {
            available: true,
            repository: Some("owner/repo".to_owned()),
            remote_branches: BTreeMap::from([
                ("main".to_owned(), main_oid),
                ("old".to_owned(), old_oid),
            ]),
            open_prs: Vec::new(),
            error: None,
        };
        (root, linked, github)
    }

    fn cleanup_temp_repo(root: &Path, linked: &Path) {
        let _ = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["worktree", "remove", "--force"])
            .arg(linked)
            .status();
        let _ = fs::remove_dir_all(linked);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clean_merged_linked_worktree_is_content_safe_without_live_resources() {
        let (root, linked, github) = repo_with_reclaimable_worktree();
        let snapshot = json!({"workspaces": [], "agents": []});
        let result = render_preview(&root, "origin/main", &snapshot, github);
        assert_eq!(result["ok"], true);
        assert_eq!(result["target_ref_fresh"], true);
        let worktree = result["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["path"] == json!(linked))
            .unwrap();
        assert_eq!(worktree["dirty"], false);
        assert_eq!(worktree["reachable_from_target"], true);
        assert_eq!(worktree["content_safe"], true);
        assert_eq!(worktree["safe_to_delete"], true);
        assert_eq!(worktree["reasons"], json!([]));
        cleanup_temp_repo(&root, &linked);
    }

    #[test]
    fn live_workspace_and_agent_block_deletion_without_changing_content_safety() {
        let (root, linked, github) = repo_with_reclaimable_worktree();
        let snapshot = json!({
            "workspaces": [{
                "workspace_id": "w1",
                "worktree": {"checkout_path": linked},
            }],
            "agents": [{
                "name": "worker",
                "kind": "pi",
                "status": "idle",
                "cwd": linked,
                "pane_id": "w1:p1",
                "workspace_id": "w1",
            }],
        });
        let result = render_preview(&root, "origin/main", &snapshot, github);
        let worktree = result["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["path"] == json!(linked))
            .unwrap();
        assert_eq!(worktree["content_safe"], true);
        assert_eq!(worktree["safe_to_delete"], false);
        assert!(
            worktree["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("workspace_present"))
        );
        assert!(
            worktree["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("agent_present"))
        );
        cleanup_temp_repo(&root, &linked);
    }

    #[test]
    fn dirty_or_open_pr_worktree_fails_closed() {
        let (root, linked, mut github) = repo_with_reclaimable_worktree();
        fs::write(linked.join("untracked.txt"), "dirty\n").unwrap();
        github.open_prs.push(OpenPr {
            number: 42,
            head: "old".to_owned(),
            base: "main".to_owned(),
            url: Some("https://github.com/owner/repo/pull/42".to_owned()),
            draft: false,
        });
        let snapshot = json!({"workspaces": [], "agents": []});
        let result = render_preview(&root, "origin/main", &snapshot, github);
        let worktree = result["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["path"] == json!(linked))
            .unwrap();
        assert_eq!(worktree["content_safe"], false);
        assert_eq!(worktree["safe_to_delete"], false);
        let reasons = worktree["reasons"].as_array().unwrap();
        assert!(reasons.contains(&json!("worktree_dirty")));
        assert!(reasons.contains(&json!("open_pr_reference")));
        cleanup_temp_repo(&root, &linked);
    }

    #[test]
    fn stale_target_ref_blocks_reclaim_even_when_local_git_looks_safe() {
        let (root, linked, mut github) = repo_with_reclaimable_worktree();
        github.remote_branches.insert(
            "main".to_owned(),
            "1111111111111111111111111111111111111111".to_owned(),
        );
        let snapshot = json!({"workspaces": [], "agents": []});
        let result = render_preview(&root, "origin/main", &snapshot, github);
        assert_eq!(result["target_ref_fresh"], false);
        let worktree = result["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["path"] == json!(linked))
            .unwrap();
        assert_eq!(worktree["reachable_from_target"], true);
        assert_eq!(worktree["safe_to_delete"], false);
        assert!(
            worktree["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("target_ref_not_fresh"))
        );
        cleanup_temp_repo(&root, &linked);
    }

    #[test]
    fn unavailable_github_evidence_blocks_reclaim() {
        let (root, linked, mut github) = repo_with_reclaimable_worktree();
        github.available = false;
        github.error = Some(json!({"code": "gh_unavailable"}));
        let snapshot = json!({"workspaces": [], "agents": []});
        let result = render_preview(&root, "origin/main", &snapshot, github);
        let worktree = result["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["path"] == json!(linked))
            .unwrap();
        assert_eq!(worktree["safe_to_delete"], false);
        assert!(
            worktree["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("github_evidence_unavailable"))
        );
        cleanup_temp_repo(&root, &linked);
    }

    #[test]
    fn parser_keeps_worktree_flags_and_primary_identity() {
        let records = parse_worktrees(
            "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\nworktree /repo/wt\nHEAD def\ndetached\nlocked reason\nprunable stale\n\n",
        );
        assert_eq!(records.len(), 2);
        assert!(records[0].primary);
        assert!(!records[1].primary);
        assert!(records[1].detached);
        assert!(records[1].locked);
        assert!(records[1].prunable);
    }

    #[test]
    fn unknown_params_fail_before_git_or_github_access() {
        let result = preview(
            &json!({"project_root": "/repo", "delete": true}),
            &json!({}),
        );
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "invalid_params");
        assert_eq!(result["unknown"], json!(["delete"]));
    }
}
