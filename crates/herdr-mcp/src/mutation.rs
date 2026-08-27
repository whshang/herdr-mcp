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

pub fn check_global(action: &str) -> Result<(), Value> {
    check_global_with_policy(action, &MutationPolicy::from_env())
}

fn check_global_with_policy(action: &str, policy: &MutationPolicy) -> Result<(), Value> {
    if policy.readonly {
        return Err(json!({
            "ok": false,
            "reason": "readonly_mode",
            "action": action,
            "hint": "HERDR_MCP_READONLY=1 — all mutating operations are disabled",
        }));
    }
    Ok(())
}

pub fn working_agents(snapshot: &Value, root: &Path) -> Vec<Value> {
    let topology = projects::derive_routing(snapshot);
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
    working
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

    let working = working_agents(snapshot, root);

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

#[derive(Debug, Clone)]
pub struct AtomicStage {
    pub path: PathBuf,
    pub content: Option<Vec<u8>>,
}

impl AtomicStage {
    pub fn write(path: PathBuf, content: Vec<u8>) -> Self {
        Self {
            path,
            content: Some(content),
        }
    }

    pub fn delete(path: PathBuf) -> Self {
        Self {
            path,
            content: None,
        }
    }
}

pub fn atomic_write(path: &Path, content: &[u8]) -> Result<(), String> {
    commit_atomic(&[AtomicStage::write(path.to_path_buf(), content.to_vec())])
}

pub fn commit_atomic(stages: &[AtomicStage]) -> Result<(), String> {
    commit_atomic_inner(stages, None)
}

fn commit_atomic_inner(stages: &[AtomicStage], fail_after: Option<usize>) -> Result<(), String> {
    if stages.is_empty() {
        return Ok(());
    }
    let mut seen = HashSet::new();
    for stage in stages {
        if !seen.insert(stage.path.clone()) {
            return Err(format!("duplicate atomic target: {}", stage.path.display()));
        }
        let parent = stage
            .path
            .parent()
            .ok_or_else(|| format!("target has no parent: {}", stage.path.display()))?;
        if !parent.is_dir() {
            return Err(format!(
                "target parent is not a directory: {}",
                parent.display()
            ));
        }
        if stage.path.is_dir() {
            return Err(format!(
                "atomic file stage cannot target a directory: {}",
                stage.path.display()
            ));
        }
        if stage.content.is_none() && !stage.path.is_file() {
            return Err(format!(
                "delete target does not exist as a file: {}",
                stage.path.display()
            ));
        }
    }

    let transaction = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let mut prepared = Vec::<(PathBuf, PathBuf)>::new();
    for (index, stage) in stages.iter().enumerate() {
        let Some(content) = &stage.content else {
            continue;
        };
        let parent = stage.path.parent().expect("validated parent");
        let name = stage
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("target has invalid filename: {}", stage.path.display()))?;
        let temporary = parent.join(format!(".{name}.herdr-mcp-{transaction}-{index}.tmp"));
        let previous_permissions = fs::metadata(&stage.path)
            .ok()
            .map(|metadata| metadata.permissions());
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) => {
                cleanup_prepared(&prepared);
                return Err(format!("cannot create temporary file: {error}"));
            }
        };
        if let Some(permissions) = previous_permissions
            && let Err(error) = file.set_permissions(permissions)
        {
            let _ = fs::remove_file(&temporary);
            cleanup_prepared(&prepared);
            return Err(format!("cannot preserve target permissions: {error}"));
        }
        if let Err(error) = file.write_all(content).and_then(|_| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            cleanup_prepared(&prepared);
            return Err(format!("cannot prepare atomic file: {error}"));
        }
        drop(file);
        prepared.push((stage.path.clone(), temporary));
    }

    let mut backups = Vec::<(PathBuf, PathBuf)>::new();
    let mut created = Vec::<PathBuf>::new();
    let apply_result = (|| -> Result<(), String> {
        for (index, stage) in stages.iter().enumerate() {
            let existed = stage.path.exists();
            if existed {
                let parent = stage.path.parent().expect("validated parent");
                let name = stage
                    .path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| {
                        format!("target has invalid filename: {}", stage.path.display())
                    })?;
                let backup = parent.join(format!(".{name}.herdr-mcp-{transaction}-{index}.bak"));
                if backup.exists() {
                    return Err(format!(
                        "atomic backup path already exists: {}",
                        backup.display()
                    ));
                }
                fs::rename(&stage.path, &backup).map_err(|error| {
                    format!("cannot stage backup for {}: {error}", stage.path.display())
                })?;
                backups.push((stage.path.clone(), backup));
            } else if stage.content.is_none() {
                return Err(format!(
                    "delete target disappeared: {}",
                    stage.path.display()
                ));
            }

            if stage.content.is_some() {
                let temporary = prepared
                    .iter()
                    .find(|(path, _)| path == &stage.path)
                    .map(|(_, temporary)| temporary)
                    .ok_or_else(|| format!("missing prepared file for {}", stage.path.display()))?;
                fs::rename(temporary, &stage.path).map_err(|error| {
                    format!("cannot activate {}: {error}", stage.path.display())
                })?;
                if !existed {
                    created.push(stage.path.clone());
                }
            }
            if fail_after == Some(index) {
                return Err(format!(
                    "injected atomic commit failure after stage {index}"
                ));
            }
        }
        Ok(())
    })();

    if let Err(error) = apply_result {
        let mut rollback_errors = Vec::new();
        for path in created.iter().rev() {
            if path.exists()
                && let Err(rollback_error) = fs::remove_file(path)
            {
                rollback_errors.push(format!("remove {}: {rollback_error}", path.display()));
            }
        }
        for (path, backup) in backups.iter().rev() {
            if path.exists()
                && let Err(rollback_error) = fs::remove_file(path)
            {
                rollback_errors.push(format!("clear {}: {rollback_error}", path.display()));
                continue;
            }
            if backup.exists()
                && let Err(rollback_error) = fs::rename(backup, path)
            {
                rollback_errors.push(format!("restore {}: {rollback_error}", path.display()));
            }
        }
        cleanup_prepared(&prepared);
        return Err(if rollback_errors.is_empty() {
            error
        } else {
            format!("{error}; rollback errors: {}", rollback_errors.join("; "))
        });
    }

    cleanup_prepared(&prepared);
    for (_, backup) in backups {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

fn cleanup_prepared(prepared: &[(PathBuf, PathBuf)]) {
    for (_, temporary) in prepared {
        let _ = fs::remove_file(temporary);
    }
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

        let other = root.with_file_name(format!(
            "{}-other",
            root.file_name().unwrap().to_string_lossy()
        ));
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
    fn global_gate_only_applies_readonly_not_write_roots() {
        let readonly = MutationPolicy::new(true, vec![PathBuf::from("/tmp/only")]);
        let denied = check_global_with_policy("herdr_prompt", &readonly).unwrap_err();
        assert_eq!(denied["reason"], "readonly_mode");
        assert_eq!(denied["action"], "herdr_prompt");

        let writable = MutationPolicy::new(false, vec![PathBuf::from("/tmp/only")]);
        assert!(check_global_with_policy("herdr_prompt", &writable).is_ok());
    }

    #[test]
    fn multi_file_atomic_commit_rolls_back_partial_apply() {
        let root = repo();
        let existing = root.join("existing.txt");
        let created = root.join("created.txt");
        fs::write(&existing, "original").unwrap();
        let stages = vec![
            AtomicStage::write(existing.clone(), b"replaced".to_vec()),
            AtomicStage::write(created.clone(), b"created".to_vec()),
        ];
        let error = commit_atomic_inner(&stages, Some(1)).unwrap_err();
        assert!(error.contains("injected atomic commit failure"));
        assert_eq!(fs::read_to_string(&existing).unwrap(), "original");
        assert!(!created.exists());
        assert!(
            !fs::read_dir(&root)
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    name.contains(".herdr-mcp-")
                        && (name.ends_with(".tmp") || name.ends_with(".bak"))
                })
        );
        fs::remove_dir_all(root).unwrap();
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
