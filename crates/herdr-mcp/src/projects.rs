use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const GIT_TOPLEVEL_TIMEOUT: Duration = Duration::from_secs(1);
const GIT_STATUS_TIMEOUT: Duration = Duration::from_millis(750);
const GIT_OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProjectInfo {
    pub root: PathBuf,
    pub vcs: Option<&'static str>,
    pub managed: bool,
    pub dirty: bool,
    pub changed_files: usize,
    pub pane_ids: Vec<String>,
    pub cwds: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectTopology {
    pub projects: BTreeMap<PathBuf, ProjectInfo>,
    pub pane_to_workspace: HashMap<String, String>,
}

pub fn derive(snapshot: &Value) -> ProjectTopology {
    let pane_to_workspace = pane_workspaces(snapshot);
    let pane_cwds = pane_cwds(snapshot);
    let home = home_dir();

    let mut grouped =
        BTreeMap::<PathBuf, (Option<&'static str>, BTreeSet<String>, BTreeSet<PathBuf>)>::new();
    for (pane_id, cwd) in pane_cwds {
        let git_root = git_toplevel(&cwd);
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

    let status_roots = grouped
        .iter()
        .filter_map(|(root, (vcs, _, _))| {
            let managed = vcs.is_some() && !is_unmanaged_root(root, home.as_deref());
            managed.then_some(root.clone())
        })
        .collect::<Vec<_>>();
    let statuses = git_statuses(&status_roots);

    let projects = grouped
        .into_iter()
        .map(|(root, (vcs, pane_ids, cwds))| {
            let managed = vcs.is_some() && !is_unmanaged_root(&root, home.as_deref());
            let (dirty, changed_files) = statuses.get(&root).copied().unwrap_or((false, 0));
            let info = ProjectInfo {
                root: root.clone(),
                vcs,
                managed,
                dirty,
                changed_files,
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

    let output = run_git(cwd, &["rev-parse", "--show-toplevel"], GIT_TOPLEVEL_TIMEOUT)?;
    if !output.success {
        return None;
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!root.is_empty()).then(|| PathBuf::from(root))
}

fn git_statuses(roots: &[PathBuf]) -> HashMap<PathBuf, (bool, usize)> {
    thread::scope(|scope| {
        let jobs = roots
            .iter()
            .map(|root| {
                let root = root.clone();
                scope.spawn(move || {
                    let status = git_status(&root);
                    (root, status)
                })
            })
            .collect::<Vec<_>>();

        jobs.into_iter().filter_map(|job| job.join().ok()).collect()
    })
}

fn git_status(root: &Path) -> (bool, usize) {
    let Some(output) = run_git(
        root,
        &["status", "--porcelain", "--untracked-files=normal"],
        GIT_STATUS_TIMEOUT,
    ) else {
        return (false, 0);
    };
    if !output.success {
        return (false, 0);
    }
    let changed_files = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    (changed_files > 0, changed_files)
}

struct CommandOutput {
    success: bool,
    stdout: Vec<u8>,
}

fn run_git(cwd: &Path, args: &[&str], timeout: Duration) -> Option<CommandOutput> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;
    let stdout_reader = thread::spawn(move || read_limited(stdout, GIT_OUTPUT_LIMIT));
    let stderr_reader = thread::spawn(move || read_limited(stderr, 64 * 1024));
    let started = Instant::now();

    let status = loop {
        match child.try_wait().ok()? {
            Some(status) => break status,
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return None;
            }
            None => thread::sleep(Duration::from_millis(10)),
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
        assert_eq!(project.pane_ids, vec!["w1:p1"]);
        assert_eq!(topology.pane_to_workspace["w1:p1"], "w1");

        fs::remove_dir_all(root).unwrap();
    }
}
