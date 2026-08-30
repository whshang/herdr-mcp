use crate::fs_security;
use crate::git_tools;
use crate::mutation::{self, AtomicStage};
use crate::patch::{self, PatchOp};
use crate::projects;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct Staged {
    display: PathBuf,
    stage: AtomicStage,
    operation: &'static str,
    dirty_source: Option<PathBuf>,
}

pub fn apply(snapshot: &Value, args: &Value) -> Value {
    let root_input = match required_str(args, "root") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let patch_text = match required_str(args, "patch") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let dry_run = match optional_bool(args, "dry_run") {
        Ok(value) => value.unwrap_or(false),
        Err(error) => return error,
    };
    let confirm_dirty = match optional_bool(args, "confirm_dirty") {
        Ok(value) => value.unwrap_or(false),
        Err(error) => return error,
    };
    let confirm_busy = match optional_bool(args, "confirm_busy") {
        Ok(value) => value.unwrap_or(false),
        Err(error) => return error,
    };

    let topology = projects::derive_routing(snapshot);
    let root_path = match fs_security::validate_existing_with_topology(&topology, root_input) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !root_path.real.is_dir() {
        return fail(
            "not_a_directory",
            root_path.resolved.to_string_lossy().as_ref(),
            None,
        );
    }
    let root_real = fs::canonicalize(&root_path.root).unwrap_or_else(|_| root_path.root.clone());
    if root_path.real != root_real {
        return json!({
            "ok": false,
            "reason": "root_not_project_root",
            "root": root_path.resolved.to_string_lossy(),
            "project_root": root_path.root.to_string_lossy(),
            "hint": "herdr_fs_patch root must be the managed Git project root",
        });
    }

    let operations = match patch::parse_patch(patch_text) {
        Ok(value) if !value.is_empty() => value,
        Ok(_) => {
            return json!({"ok": false, "code": "PATCH_FAILED", "message": "No files were modified."});
        }
        Err(error) => return error.to_value(),
    };

    let working = if dry_run {
        mutation::working_agents_from(&topology, snapshot, &root_path.root)
    } else {
        match mutation::check_with_topology(snapshot, &topology, &root_path.root, confirm_busy) {
            Ok(value) => value,
            Err(error) => return error,
        }
    };

    let mut staged = Vec::<Staged>::new();
    let mut summaries = Vec::<String>::new();
    let mut additions = 0usize;
    let mut removals = 0usize;

    for operation in operations {
        match operation {
            PatchOp::Add { path, content } => {
                let target = match resolve_target(&root_path.root, &path, false) {
                    Ok(value) => value,
                    Err(error) => return error,
                };
                if target.real.exists() {
                    return patch_failure(
                        "PATCH_FAILED",
                        "Cannot add file that already exists.",
                        &target.resolved,
                    );
                }
                additions += content.split('\n').count();
                summaries.push(format!("A {}", target.resolved.display()));
                staged.push(Staged {
                    display: target.resolved,
                    stage: AtomicStage::write(target.real, content.into_bytes()),
                    operation: "add",
                    dirty_source: None,
                });
            }
            PatchOp::Delete { path } => {
                let target = match resolve_target(&root_path.root, &path, true) {
                    Ok(value) => value,
                    Err(error) => return error,
                };
                let text = match fs::read_to_string(&target.real) {
                    Ok(value) => value,
                    Err(error) => {
                        return fail(
                            "read_failed",
                            target.resolved.to_string_lossy().as_ref(),
                            Some(error.to_string()),
                        );
                    }
                };
                removals += text.split('\n').count();
                summaries.push(format!("D {}", target.resolved.display()));
                staged.push(Staged {
                    display: target.resolved,
                    dirty_source: Some(target.real.clone()),
                    stage: AtomicStage::delete(target.real),
                    operation: "delete",
                });
            }
            PatchOp::Update {
                path,
                hunks,
                move_to,
            } => {
                let source = match resolve_target(&root_path.root, &path, true) {
                    Ok(value) => value,
                    Err(error) => return error,
                };
                let old = match fs::read_to_string(&source.real) {
                    Ok(value) => value,
                    Err(error) => {
                        return fail(
                            "read_failed",
                            source.resolved.to_string_lossy().as_ref(),
                            Some(error.to_string()),
                        );
                    }
                };
                let updated = match patch::apply_update_hunks(
                    &old,
                    &hunks,
                    source.resolved.to_string_lossy().as_ref(),
                ) {
                    Ok(value) => value,
                    Err(error) => return error.to_value(),
                };
                for hunk in &hunks {
                    for line in hunk {
                        if line.starts_with('+') {
                            additions += 1;
                        } else if line.starts_with('-') {
                            removals += 1;
                        }
                    }
                }
                if let Some(destination_path) = move_to {
                    let destination =
                        match resolve_target(&root_path.root, &destination_path, false) {
                            Ok(value) => value,
                            Err(error) => return error,
                        };
                    if destination.real.exists() {
                        return patch_failure(
                            "PATCH_FAILED",
                            "Move destination already exists.",
                            &destination.resolved,
                        );
                    }
                    summaries.push(format!(
                        "R {} -> {}",
                        source.resolved.display(),
                        destination.resolved.display()
                    ));
                    staged.push(Staged {
                        display: source.resolved.clone(),
                        dirty_source: Some(source.real.clone()),
                        stage: AtomicStage::delete(source.real),
                        operation: "delete",
                    });
                    staged.push(Staged {
                        display: destination.resolved,
                        dirty_source: None,
                        stage: AtomicStage::write(destination.real, updated.into_bytes()),
                        operation: "add",
                    });
                } else {
                    summaries.push(format!("M {}", source.resolved.display()));
                    staged.push(Staged {
                        display: source.resolved,
                        dirty_source: Some(source.real.clone()),
                        stage: AtomicStage::write(source.real, updated.into_bytes()),
                        operation: "update",
                    });
                }
            }
        }
    }

    if let Err(error) = reject_duplicate_targets(&staged) {
        return error;
    }
    if !dry_run && !confirm_dirty {
        let dirty_sources = staged
            .iter()
            .filter_map(|item| item.dirty_source.clone())
            .collect::<Vec<_>>();
        match git_tools::first_dirty_file(&root_path.root, &dirty_sources) {
            Ok(Some(dirty)) => {
                let display = staged
                    .iter()
                    .find(|item| item.dirty_source.as_ref() == Some(&dirty))
                    .map(|item| item.display.as_path())
                    .unwrap_or(dirty.as_path());
                return json!({
                    "ok": false,
                    "reason": "file_dirty_confirmation_required",
                    "path": display.to_string_lossy(),
                    "hint": "re-send with confirm_dirty:true",
                });
            }
            Ok(None) => {}
            Err(message) => {
                return crate::macos_permissions::git_failure_to_value(
                    &root_path.root,
                    "status",
                    message,
                );
            }
        }
    }

    if !dry_run {
        let transaction = staged
            .iter()
            .map(|item| item.stage.clone())
            .collect::<Vec<_>>();
        if let Err(message) = mutation::commit_atomic(&transaction) {
            return json!({
                "ok": false,
                "code": "PATCH_COMMIT_FAILED",
                "message": message,
                "hint": "patch rolled back when possible; re-read files and regenerate",
            });
        }
    }

    let mut output = Map::new();
    output.insert("ok".to_owned(), json!(true));
    output.insert("dry_run".to_owned(), json!(dry_run));
    output.insert("root".to_owned(), json!(root_path.root.to_string_lossy()));
    output.insert("summary".to_owned(), json!(summaries.join("\n")));
    output.insert(
        "affected_files".to_owned(),
        Value::Array(
            staged
                .iter()
                .map(|item| {
                    json!({
                        "path": item.display.to_string_lossy(),
                        "operation": item.operation,
                    })
                })
                .collect(),
        ),
    );
    output.insert("additions".to_owned(), json!(additions));
    output.insert("removals".to_owned(), json!(removals));
    if !working.is_empty() {
        output.insert("warnings".to_owned(), json!({"working": working}));
    }
    Value::Object(output)
}

fn resolve_target(
    project_root: &Path,
    raw: &str,
    must_exist: bool,
) -> Result<fs_security::ManagedPath, Value> {
    let raw_path = Path::new(raw);
    let absolute = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        project_root.join(raw_path)
    };
    let absolute = absolute.to_string_lossy();
    let target = if must_exist {
        fs_security::validate_existing_in_root(project_root, &absolute)?
    } else {
        fs_security::validate_target_in_root(project_root, &absolute)?
    };
    let expected_root =
        fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let actual_root = fs::canonicalize(&target.root).unwrap_or_else(|_| target.root.clone());
    if expected_root != actual_root {
        return Err(json!({
            "ok": false,
            "reason": "cross_project_patch_denied",
            "path": target.resolved.to_string_lossy(),
            "project_root": project_root.to_string_lossy(),
            "target_root": target.root.to_string_lossy(),
        }));
    }
    if must_exist && !target.real.is_file() {
        return Err(json!({
            "ok": false,
            "reason": "not_a_file",
            "path": target.resolved.to_string_lossy(),
        }));
    }
    Ok(target)
}

fn reject_duplicate_targets(staged: &[Staged]) -> Result<(), Value> {
    let mut seen = HashSet::new();
    for item in staged {
        if !seen.insert(item.stage.path.clone()) {
            return Err(json!({
                "ok": false,
                "code": "PATCH_FAILED",
                "message": format!("Patch targets the same path more than once: {}", item.stage.path.display()),
                "path": item.display.to_string_lossy(),
            }));
        }
    }
    Ok(())
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("{key} must be a string")))
}

fn optional_bool(args: &Value, key: &str) -> Result<Option<bool>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(invalid(&format!("{key} must be a boolean"))),
    }
}

fn invalid(message: &str) -> Value {
    json!({"ok": false, "code": "invalid_params", "message": message})
}

fn fail(reason: &str, path: &str, message: Option<String>) -> Value {
    let mut output = Map::new();
    output.insert("ok".to_owned(), json!(false));
    output.insert("reason".to_owned(), json!(reason));
    output.insert("path".to_owned(), json!(path));
    if let Some(message) = message {
        output.insert("message".to_owned(), json!(message));
    }
    Value::Object(output)
}

fn patch_failure(code: &str, message: &str, path: &Path) -> Value {
    json!({
        "ok": false,
        "code": code,
        "message": message,
        "path": path.to_string_lossy(),
    })
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
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-fs-patch-{}-{timestamp}-{sequence}",
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
        fs::write(root.join("update.txt"), "one\nold\ntwo\n").unwrap();
        fs::write(root.join("delete.txt"), "delete me\n").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=Herdr Test",
                    "-c",
                    "user.email=herdr@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "baseline"
                ])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        root
    }

    fn snapshot(root: &Path) -> Value {
        json!({
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}],
            "agents": []
        })
    }

    #[test]
    fn dry_run_stages_without_writing_and_real_apply_is_atomic() {
        let root = repo();
        let snap = snapshot(&root);
        let patch = "*** Begin Patch\n*** Add File: added.txt\n+hello\n*** Update File: update.txt\n@@\n one\n-old\n+new\n two\n*** Delete File: delete.txt\n*** End Patch";
        let dry = apply(
            &snap,
            &json!({"root": root, "patch": patch, "dry_run": true}),
        );
        assert_eq!(dry["ok"], true);
        assert_eq!(dry["affected_files"].as_array().unwrap().len(), 3);
        assert!(!root.join("added.txt").exists());
        assert_eq!(
            fs::read_to_string(root.join("update.txt")).unwrap(),
            "one\nold\ntwo\n"
        );
        assert!(root.join("delete.txt").exists());

        let result = apply(&snap, &json!({"root": root, "patch": patch}));
        assert_eq!(result["ok"], true);
        assert_eq!(
            fs::read_to_string(root.join("added.txt")).unwrap(),
            "hello\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("update.txt")).unwrap(),
            "one\nnew\ntwo\n"
        );
        assert!(!root.join("delete.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dirty_source_is_fail_closed_before_transaction() {
        let root = repo();
        let snap = snapshot(&root);
        fs::write(root.join("update.txt"), "one\ndirty\ntwo\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: update.txt\n@@\n-dirty\n+new\n*** End Patch";
        let blocked = apply(&snap, &json!({"root": root, "patch": patch}));
        assert_eq!(blocked["reason"], "file_dirty_confirmation_required");
        assert_eq!(
            fs::read_to_string(root.join("update.txt")).unwrap(),
            "one\ndirty\ntwo\n"
        );
        let forced = apply(
            &snap,
            &json!({"root": root, "patch": patch, "confirm_dirty": true}),
        );
        assert_eq!(forced["ok"], true);
        fs::remove_dir_all(root).unwrap();
    }
}
