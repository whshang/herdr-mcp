use crate::projects::ProjectTopology;
use crate::skill;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::env;
use std::path::{Path, PathBuf};

const EXEC_HINT: &str = "Short shell commands, Git facts, filesystem operations and agent control are native herdr-mcp capabilities. Use long-running execution sessions for non-trivial commands; do not treat process exit alone as completion evidence for agent work.";

pub fn info(view: &Value, topology: &ProjectTopology) -> Value {
    let managed_git_roots = topology
        .projects
        .values()
        .filter(|project| project.managed && project.vcs == Some("git"))
        .map(|project| project.root.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let write_roots = write_roots();

    json!({
        "server_name": "herdr-mcp",
        "server_version": crate::runtime_meta::runtime_version(),
        "runtime_channel": crate::runtime_meta::runtime_channel(),
        "runtime_source_commit": crate::runtime_meta::compiled_source_commit(),
        "runtime_source_dirty": crate::runtime_meta::compiled_source_dirty(),
        "default_cwd": default_cwd(view),
        "managed_git_roots": managed_git_roots,
        "readonly_mode": env::var("HERDR_MCP_READONLY").ok().as_deref() == Some("1"),
        "write_roots": if write_roots.is_empty() { Value::Null } else { json!(write_roots) },
        "agent_visibility": view.get("agent_visibility").cloned().unwrap_or(Value::Null),
        "agent_skill": skill::pointer(),
        "exec_environment": {
            "shell": execution_shell(),
            "path_has": {
                "herdr": which("herdr"),
                "git": which("git"),
                "rg": which("rg"),
                "dsh": which("dsh"),
                "dsh_tui": which("dsh-tui"),
            },
            "hint": EXEC_HINT,
        }
    })
}

fn default_cwd(view: &Value) -> Value {
    if let Some(focused_pane) = view.get("focused_pane").and_then(Value::as_str)
        && let Some(cwd) = view
            .get("panes")
            .and_then(Value::as_array)
            .and_then(|panes| {
                panes
                    .iter()
                    .find(|pane| pane.get("id").and_then(Value::as_str) == Some(focused_pane))
            })
            .and_then(|pane| pane.get("cwd"))
            .filter(|value| value.is_string())
    {
        return cwd.clone();
    }

    view.get("workspaces")
        .and_then(Value::as_array)
        .and_then(|workspaces| {
            workspaces
                .iter()
                .find(|workspace| workspace.get("focused").and_then(Value::as_bool) == Some(true))
        })
        .and_then(|workspace| workspace.get("cwd"))
        .filter(|value| value.is_string())
        .cloned()
        .unwrap_or(Value::Null)
}

fn write_roots() -> Vec<String> {
    env::var("HERDR_MCP_WRITE_ROOTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn execution_shell() -> String {
    env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "cmd.exe".to_owned()
            } else {
                "/bin/sh".to_owned()
            }
        })
}

fn which(name: &str) -> Value {
    find_executable(name)
        .map(|path| json!(path.to_string_lossy()))
        .unwrap_or(Value::Null)
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let extensions = executable_extensions();
    for directory in env::split_paths(&path) {
        for extension in &extensions {
            let candidate = directory.join(format!("{name}{extension}"));
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn executable_extensions() -> Vec<String> {
    if cfg!(windows) {
        let mut extensions = env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned())
            .split(';')
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase())
            .collect::<Vec<_>>();
        extensions.insert(0, String::new());
        extensions
    } else {
        vec![String::new()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::ProjectInfo;
    use std::collections::{BTreeMap, HashMap};

    #[test]
    fn focused_pane_cwd_wins_for_default_cwd() {
        let view = json!({
            "focused_pane": "w1:p2",
            "panes": [
                {"id": "w1:p1", "cwd": "/one"},
                {"id": "w1:p2", "cwd": "/two"}
            ],
            "workspaces": [{"focused": true, "cwd": "/workspace"}]
        });
        assert_eq!(default_cwd(&view), "/two");
    }

    #[test]
    fn info_lists_only_managed_git_projects() {
        let mut projects = BTreeMap::new();
        projects.insert(
            PathBuf::from("/repo"),
            ProjectInfo {
                root: PathBuf::from("/repo"),
                vcs: Some("git"),
                managed: true,
                dirty: false,
                changed_files: 0,
                git_status_observed: true,
                git_status_source: Some("local_git"),
                pane_ids: vec![],
                cwds: vec![],
            },
        );
        projects.insert(
            PathBuf::from("/scratch"),
            ProjectInfo {
                root: PathBuf::from("/scratch"),
                vcs: None,
                managed: false,
                dirty: false,
                changed_files: 0,
                git_status_observed: false,
                git_status_source: None,
                pane_ids: vec![],
                cwds: vec![],
            },
        );
        let topology = ProjectTopology {
            projects,
            pane_to_workspace: HashMap::new(),
        };
        let result = info(&json!({"agent_visibility": "allowlist"}), &topology);
        assert_eq!(result["managed_git_roots"], json!(["/repo"]));
        assert_eq!(result["agent_visibility"], "allowlist");
    }
}
