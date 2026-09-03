use crate::child_process;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const GIT_STATUS_TIMEOUT: Duration = Duration::from_millis(750);
const GIT_OUTPUT_LIMIT: usize = 1024 * 1024;

#[cfg(test)]
thread_local! {
    static DERIVE_ROUTING_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProjectInfo {
    pub root: PathBuf,
    pub vcs: Option<&'static str>,
    pub managed: bool,
    pub dirty: bool,
    pub changed_files: usize,
    pub git_status_observed: bool,
    pub git_status_source: Option<&'static str>,
    pub pane_ids: Vec<String>,
    pub cwds: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectTopology {
    pub projects: BTreeMap<PathBuf, ProjectInfo>,
    pub pane_to_workspace: HashMap<String, String>,
}

pub fn derive(snapshot: &Value) -> ProjectTopology {
    derive_inner(snapshot, true)
}

/// Derive project routing without running `git status` for dirty enrichment.
///
/// Security gates and busy-agent routing only need the managed Git root and
/// pane/workspace ownership. Keeping that path separate avoids paying for a
/// full repository status scan on every file read, write, grep, or exec gate.
///
/// Callers that need both managed-root validation and busy-agent checks in
/// the same request should derive once and pass the topology through
/// `fs_security` / `mutation` helpers so the same snapshot identity is not
/// recomputed.
pub fn derive_routing(snapshot: &Value) -> ProjectTopology {
    #[cfg(test)]
    DERIVE_ROUTING_CALLS.with(|count| count.set(count.get() + 1));
    derive_inner(snapshot, false)
}

#[cfg(test)]
pub(crate) fn derive_routing_call_count() -> usize {
    DERIVE_ROUTING_CALLS.with(|count| count.get())
}

#[cfg(test)]
pub(crate) fn reset_derive_routing_call_count() {
    DERIVE_ROUTING_CALLS.with(|count| count.set(0));
}

fn derive_inner(snapshot: &Value, include_git_status: bool) -> ProjectTopology {
    let pane_to_workspace = pane_workspaces(snapshot);
    let pane_cwds = pane_cwds(snapshot);
    let declared_roots = declared_workspace_roots(snapshot);
    let home = home_dir();

    let mut grouped =
        BTreeMap::<PathBuf, (Option<&'static str>, BTreeSet<String>, BTreeSet<PathBuf>)>::new();
    for (pane_id, cwd) in pane_cwds {
        let git_root = declared_git_root(&pane_id, &cwd, &pane_to_workspace, &declared_roots)
            .or_else(|| git_toplevel_tcc_safe(snapshot, &cwd));
        let (root, vcs) = match git_root {
            Some(root) => (root, Some("git")),
            None => (cwd.clone(), None),
        };
        let entry = grouped
            .entry(root.clone())
            .or_insert_with(|| (vcs, BTreeSet::new(), BTreeSet::new()));
        entry.1.insert(pane_id.clone());
        entry.2.insert(cwd);
    }

    let statuses = if include_git_status {
        let status_roots = grouped
            .iter()
            .filter_map(|(root, (vcs, _, _))| {
                let managed = vcs.is_some() && !is_unmanaged_root(root, home.as_deref());
                managed.then_some(root.clone())
            })
            .collect::<Vec<_>>();
        git_statuses(snapshot, &status_roots)
    } else {
        HashMap::new()
    };

    let projects = grouped
        .into_iter()
        .map(|(root, (vcs, pane_ids, cwds))| {
            let managed = vcs.is_some() && !is_unmanaged_root(&root, home.as_deref());
            let status = statuses.get(&root);
            let (dirty, changed_files) = status
                .map(|status| (status.dirty, status.changed_files))
                .unwrap_or((false, 0));
            let info = ProjectInfo {
                root: root.clone(),
                vcs,
                managed,
                dirty,
                changed_files,
                git_status_observed: status.is_some(),
                git_status_source: status.map(|status| status.source),
                pane_ids: pane_ids.into_iter().collect(),
                cwds: cwds.into_iter().collect(),
            };
            (root, info)
        })
        .collect();

    ProjectTopology {
        projects,
        pane_to_workspace,
    }
}

fn declared_workspace_roots(snapshot: &Value) -> HashMap<String, PathBuf> {
    array(snapshot, "workspaces")
        .iter()
        .filter_map(|workspace| {
            let workspace_id = workspace.get("workspace_id")?.as_str()?;
            let worktree = workspace.get("worktree")?.as_object()?;
            let root = worktree
                .get("checkout_path")
                .and_then(Value::as_str)
                .or_else(|| worktree.get("path").and_then(Value::as_str))?;
            let root = PathBuf::from(root);
            root.is_absolute().then(|| (workspace_id.to_owned(), root))
        })
        .collect()
}

fn declared_git_root(
    pane_id: &str,
    cwd: &Path,
    pane_to_workspace: &HashMap<String, String>,
    declared_roots: &HashMap<String, PathBuf>,
) -> Option<PathBuf> {
    if let Some(root) = pane_to_workspace
        .get(pane_id)
        .and_then(|workspace| declared_roots.get(workspace))
        .filter(|root| cwd.starts_with(root))
    {
        return Some(root.clone());
    }

    declared_roots
        .values()
        .filter(|root| cwd.starts_with(root))
        .max_by_key(|root| root.components().count())
        .cloned()
}

pub fn workspaces_for_root(topology: &ProjectTopology, root: &Path) -> Vec<String> {
    let mut workspaces = topology
        .projects
        .get(root)
        .map(|project| {
            project
                .pane_ids
                .iter()
                .filter_map(|pane| topology.pane_to_workspace.get(pane))
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default()
        .into_iter()
        .collect::<Vec<_>>();
    workspaces.sort();
    workspaces
}

pub fn projects_for_workspace(topology: &ProjectTopology, workspace_id: &str) -> Vec<ProjectInfo> {
    let mut projects = topology
        .projects
        .values()
        .filter(|project| {
            project.pane_ids.iter().any(|pane| {
                topology
                    .pane_to_workspace
                    .get(pane)
                    .is_some_and(|workspace| workspace == workspace_id)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    projects.sort_by(|left, right| left.root.cmp(&right.root));
    projects
}

fn pane_workspaces(snapshot: &Value) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for key in ["agents", "panes"] {
        for item in array(snapshot, key) {
            let Some(pane_id) = item.get("pane_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(workspace_id) = item.get("workspace_id").and_then(Value::as_str) else {
                continue;
            };
            result
                .entry(pane_id.to_owned())
                .or_insert_with(|| workspace_id.to_owned());
        }
    }
    result
}

fn pane_cwds(snapshot: &Value) -> BTreeMap<String, PathBuf> {
    let mut result = BTreeMap::new();
    for key in ["agents", "panes"] {
        for item in array(snapshot, key) {
            let Some(pane_id) = item.get("pane_id").and_then(Value::as_str) else {
                continue;
            };
            let cwd = item
                .get("cwd")
                .and_then(Value::as_str)
                .or_else(|| item.get("foreground_cwd").and_then(Value::as_str));
            let Some(cwd) = cwd else {
                continue;
            };
            result
                .entry(pane_id.to_owned())
                .or_insert_with(|| PathBuf::from(cwd));
        }
    }
    result
}

fn git_toplevel(cwd: &Path) -> Option<PathBuf> {
    let mut current = absolute_path(cwd)?;
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }

    None
}

fn git_toplevel_tcc_safe(snapshot: &Value, cwd: &Path) -> Option<PathBuf> {
    if should_use_stable_broker(snapshot, cwd) {
        return broker_git_status(snapshot, cwd).map(|status| status.root);
    }
    git_toplevel(cwd)
}

#[derive(Debug, Clone)]
struct GitStatus {
    root: PathBuf,
    dirty: bool,
    changed_files: usize,
    source: &'static str,
}

fn git_statuses(snapshot: &Value, roots: &[PathBuf]) -> HashMap<PathBuf, GitStatus> {
    thread::scope(|scope| {
        let jobs = roots
            .iter()
            .map(|root| {
                let root = root.clone();
                scope.spawn(move || {
                    let status = git_status(snapshot, &root);
                    (root, status)
                })
            })
            .collect::<Vec<_>>();

        jobs.into_iter()
            .filter_map(|job| job.join().ok())
            .filter_map(|(root, status)| status.map(|status| (root, status)))
            .collect()
    })
}

fn git_status(snapshot: &Value, root: &Path) -> Option<GitStatus> {
    if should_use_stable_broker(snapshot, root) {
        return broker_git_status(snapshot, root);
    }
    git_status_direct(root).map(|(dirty, changed_files)| GitStatus {
        root: root.to_path_buf(),
        dirty,
        changed_files,
        source: "local_git",
    })
}

fn git_status_direct(root: &Path) -> Option<(bool, usize)> {
    let output = run_git(
        root,
        &["status", "--porcelain", "--untracked-files=normal"],
        GIT_STATUS_TIMEOUT,
    )?;
    if !output.success {
        return None;
    }
    let changed_files = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    Some((changed_files > 0, changed_files))
}

fn should_use_stable_broker(snapshot: &Value, path: &Path) -> bool {
    if crate::tcc_broker::is_broker_child_process() {
        return false;
    }
    crate::macos_permissions::is_protected_user_path(path)
        || is_herdr_managed_worktree(path)
        || snapshot_declares_protected_repo_storage(snapshot, path)
}

fn is_herdr_managed_worktree(path: &Path) -> bool {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .is_some_and(|home| path.starts_with(home.join(".herdr").join("worktrees")))
}

fn snapshot_declares_protected_repo_storage(snapshot: &Value, path: &Path) -> bool {
    array(snapshot, "workspaces").iter().any(|workspace| {
        let Some(worktree) = workspace.get("worktree").and_then(Value::as_object) else {
            return false;
        };
        let checkout = worktree
            .get("checkout_path")
            .and_then(Value::as_str)
            .or_else(|| worktree.get("path").and_then(Value::as_str))
            .map(Path::new);
        if !checkout.is_some_and(|checkout| path.starts_with(checkout)) {
            return false;
        }
        ["repo_key", "repo_root"]
            .into_iter()
            .filter_map(|field| worktree.get(field).and_then(Value::as_str))
            .any(|repo_path| crate::macos_permissions::is_protected_user_path(Path::new(repo_path)))
    })
}

fn broker_git_status(snapshot: &Value, root: &Path) -> Option<GitStatus> {
    let value = crate::tcc_broker::git_status_via_stable_broker(snapshot, root).ok()?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let resolved_root = value.get("root").and_then(Value::as_str)?;
    let changed_files = value
        .get("counts")
        .and_then(|counts| counts.get("files"))
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())?;
    Some(GitStatus {
        root: PathBuf::from(resolved_root),
        dirty: changed_files > 0,
        changed_files,
        source: "tcc_broker",
    })
}

struct CommandOutput {
    success: bool,
    stdout: Vec<u8>,
}

fn run_git(cwd: &Path, args: &[&str], timeout: Duration) -> Option<CommandOutput> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    child_process::configure_process_group(&mut command);
    let mut child = command.spawn().ok()?;
    let _registration = child_process::register_owned_child("git-topology", &child);
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;
    let stdout_reader = thread::spawn(move || read_limited(stdout, GIT_OUTPUT_LIMIT));
    let stderr_reader = thread::spawn(move || read_limited(stderr, 64 * 1024));
    let status = match child_process::wait_bounded(&mut child, timeout).ok()? {
        Some(status) => status,
        None => {
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return None;
        }
    };

    let stdout = stdout_reader.join().ok()??;
    let _ = stderr_reader.join().ok()??;
    Some(CommandOutput {
        success: status.success(),
        stdout,
    })
}

fn read_limited(mut reader: impl Read, limit: usize) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = reader.read(&mut buffer).ok()?;
        if count == 0 {
            return Some(output);
        }
        if output.len().saturating_add(count) > limit {
            return None;
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

fn absolute_path(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

fn is_unmanaged_root(root: &Path, home: Option<&Path>) -> bool {
    root.parent().is_none() || home.is_some_and(|home| root == home)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("herdr-mcp-projects-{unique}"))
    }

    #[test]
    fn agent_cwd_wins_over_pane_fallback() {
        let snapshot = json!({
            "agents": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": "/agent"}],
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": "/pane"}]
        });
        let cwds = pane_cwds(&snapshot);
        assert_eq!(cwds["w1:p1"], PathBuf::from("/agent"));
    }

    #[test]
    fn derives_git_root_dirty_state_and_workspace_mapping() {
        let root = temp_dir();
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(root.join("untracked.txt"), "hello").unwrap();

        let snapshot = json!({
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": nested.to_string_lossy()
            }],
            "agents": []
        });
        let topology = derive(&snapshot);
        let project = topology.projects.get(&root).unwrap();
        assert_eq!(project.vcs, Some("git"));
        assert!(project.managed);
        assert!(project.dirty);
        assert_eq!(project.changed_files, 1);
        assert!(project.git_status_observed);
        assert_eq!(project.git_status_source, Some("local_git"));
        assert_eq!(project.pane_ids, vec!["w1:p1"]);
        assert_eq!(topology.pane_to_workspace["w1:p1"], "w1");

        let routing = derive_routing(&snapshot);
        let routed_project = routing.projects.get(&root).unwrap();
        assert_eq!(routed_project.vcs, Some("git"));
        assert!(routed_project.managed);
        assert!(!routed_project.dirty);
        assert_eq!(routed_project.changed_files, 0);
        assert!(!routed_project.git_status_observed);
        assert_eq!(routed_project.git_status_source, None);
        assert_eq!(routed_project.pane_ids, vec!["w1:p1"]);
        assert_eq!(routing.pane_to_workspace["w1:p1"], "w1");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn routing_handles_non_repo_cwd_without_git_fallback() {
        let root = temp_dir();
        let nested = root.join("plain/nested");
        fs::create_dir_all(&nested).unwrap();
        let snapshot = json!({
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": nested.to_string_lossy()
            }],
            "agents": []
        });
        let routing = derive_routing(&snapshot);
        let project = routing.projects.get(&nested).unwrap();
        assert_eq!(project.vcs, None);
        assert!(!project.managed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn routing_trusts_declared_worktree_without_touching_the_filesystem() {
        let root = PathBuf::from("/Users/example/Documents/does-not-need-to-exist");
        let nested = root.join("nested");
        let snapshot = json!({
            "workspaces": [{
                "workspace_id": "w1",
                "worktree": {
                    "checkout_path": root.to_string_lossy(),
                    "repo_root": root.to_string_lossy(),
                    "repo_key": "/Users/example/Documents/does-not-need-to-exist/.git"
                }
            }],
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": nested.to_string_lossy()
            }],
            "agents": []
        });

        let topology = derive_routing(&snapshot);
        let project = topology.projects.get(&root).unwrap();
        assert_eq!(project.vcs, Some("git"));
        assert!(project.managed);
        assert!(!project.git_status_observed);
        assert_eq!(project.cwds, vec![nested]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn declared_protected_repo_storage_routes_linked_checkout_to_broker() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let checkout = PathBuf::from("/tmp/herdr-linked-checkout-not-present");
        let repo_root = home.join("Documents").join("repo-not-present");
        let snapshot = json!({
            "workspaces": [{
                "workspace_id": "w1",
                "worktree": {
                    "checkout_path": checkout.to_string_lossy(),
                    "repo_key": repo_root.join(".git").to_string_lossy(),
                    "repo_root": repo_root.to_string_lossy()
                }
            }]
        });
        assert!(snapshot_declares_protected_repo_storage(
            &snapshot, &checkout
        ));
    }

    #[test]
    fn herdr_managed_worktree_is_broker_owned_without_disk_lookup() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let checkout = home
            .join(".herdr")
            .join("worktrees")
            .join("repo")
            .join("feature-not-present");
        assert!(is_herdr_managed_worktree(&checkout));
    }
}
