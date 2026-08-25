use crate::fs_security;
use crate::projects;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct MutationPolicy {
    readonly: bool,
    write_roots: Vec<PathBuf>,
}

impl MutationPolicy {
    pub fn from_env() -> Self {
        let readonly = env::var("HERDR_MCP_READONLY").ok().as_deref() == Some("1");
        let write_roots = env::var("HERDR_MCP_WRITE_ROOTS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .collect();
        Self {
            readonly,
            write_roots,
        }
    }

    #[cfg(test)]
    fn new(readonly: bool, write_roots: Vec<PathBuf>) -> Self {
        Self {
            readonly,
            write_roots,
        }
    }
}

pub fn check(snapshot: &Value, root: &Path, confirm_busy: bool) -> Result<Vec<Value>, Value> {
    check_with_policy(snapshot, root, confirm_busy, &MutationPolicy::from_env())
}

fn check_with_policy(
    snapshot: &Value,
    root: &Path,
    confirm_busy: bool,
    policy: &MutationPolicy,
) -> Result<Vec<Value>, Value> {
    if policy.readonly {
        return Err(json!({
            "ok": false,
            "reason": "readonly_mode",
            "hint": "HERDR_MCP_READONLY=1 — all mutating operations are disabled",
        }));
    }

    let root_real = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if !policy.write_roots.is_empty() {
        let allowed = policy.write_roots.iter().any(|base| {
            let base_real = fs::canonicalize(base).unwrap_or_else(|_| base.clone());
            fs_security::path_within(&base_real, &root_real)
        });
        if !allowed {
            return Err(json!({
                "ok": false,
                "reason": "root_not_whitelisted",
                "root": root.to_string_lossy(),
                "write_roots": policy.write_roots.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>(),
                "hint": "add this root to HERDR_MCP_WRITE_ROOTS, or unset it to allow all managed roots",
            }));
        }
    }

    let topology = projects::derive(snapshot);
    let pane_ids = topology
        .projects
        .get(root)
        .map(|project| project.pane_ids.iter().cloned().collect::<HashSet<_>>())
        .unwrap_or_default();
    let mut working = snapshot
        .get("agents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|agent| {
            let status = agent
                .get("agent_status")
                .and_then(Value::as_str)
                .or_else(|| agent.get("status").and_then(Value::as_str));
            if status != Some("working") {
                return None;
            }
            let pane = agent.get("pane_id").and_then(Value::as_str)?;
            if !pane_ids.contains(pane) {
                return None;
            }
            Some(json!({
                "pane": pane,
                "agent": agent.get("agent").cloned().unwrap_or(Value::Null),
            }))
        })
        .collect::<Vec<_>>();
    working.sort_by(|left, right| {
        left.get("pane")
            .and_then(Value::as_str)
            .cmp(&right.get("pane").and_then(Value::as_str))
    });

    if !working.is_empty() && !confirm_busy {
        return Err(json!({
            "ok": false,
            "reason": "agent_working",
            "root": root.to_string_lossy(),
            "working": working,
            "hint": "an agent in this project is working — pass confirm_busy:true to force, or wait for idle/done",
        }));
    }
    Ok(working)
}

pub fn atomic_write(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "target has no parent directory".to_owned())?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "target has invalid filename".to_owned())?;
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{name}.herdr-mcp-{}-{sequence}.tmp",
        std::process::id()
    ));
    let previous_permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("cannot create temporary file: {error}"))?;
        if let Some(permissions) = previous_permissions {
            file.set_permissions(permissions)
                .map_err(|error| format!("cannot preserve target permissions: {error}"))?;
        }
        file.write_all(content)
            .map_err(|error| format!("cannot write temporary file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync temporary file: {error}"))?;
        drop(file);
        fs::rename(&temporary, path)
            .map_err(|error| format!("cannot atomically replace target: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_REPO_ID: AtomicU64 = AtomicU64::new(0);

    fn repo() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_REPO_ID.fetch_add(1, Ordering::Relaxed);
        let root = env::temp_dir().join(format!(
            "herdr-mcp-mutation-{}-{timestamp}-{sequence}",
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

    fn snapshot(root: &Path, working: bool) -> Value {
        json!({
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}],
            "agents": if working {
                vec![json!({"agent": "pi", "pane_id": "w1:p1", "workspace_id": "w1", "cwd": root, "agent_status": "working"})]
            } else {
                vec![]
            }
        })
    }

    #[test]
    fn policy_enforces_readonly_write_roots_and_busy_agents() {
        let root = repo();
        let snap = snapshot(&root, true);
        let readonly = MutationPolicy::new(true, vec![]);
        assert_eq!(
            check_with_policy(&snap, &root, false, &readonly).unwrap_err()["reason"],
            "readonly_mode"
        );

        let other = root.parent().unwrap().join("other-root");
        fs::create_dir_all(&other).unwrap();
        let restricted = MutationPolicy::new(false, vec![other.clone()]);
        assert_eq!(
            check_with_policy(&snap, &root, false, &restricted).unwrap_err()["reason"],
            "root_not_whitelisted"
        );

        let allowed = MutationPolicy::new(false, vec![root.clone()]);
        assert_eq!(
            check_with_policy(&snap, &root, false, &allowed).unwrap_err()["reason"],
            "agent_working"
        );
        let forced = check_with_policy(&snap, &root, true, &allowed).unwrap();
        assert_eq!(forced.len(), 1);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn atomic_write_replaces_without_partial_content() {
        let root = repo();
        let file = root.join("file.txt");
        fs::write(&file, "before").unwrap();
        atomic_write(&file, b"after").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "after");
        assert!(
            !fs::read_dir(&root)
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().contains(".herdr-mcp-"))
        );
        fs::remove_dir_all(root).unwrap();
    }
}
