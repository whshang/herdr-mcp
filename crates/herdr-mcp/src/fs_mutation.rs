use crate::fs_security;
use crate::git_tools;
use crate::mutation;
use serde_json::{Map, Value, json};
use std::fs;

pub fn edit(snapshot: &Value, args: &Value) -> Value {
    let path = match required_str(args, "path") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let old_string = match required_str(args, "old_string") {
        Ok("") => return invalid("old_string must not be empty"),
        Ok(value) => value,
        Err(error) => return error,
    };
    let new_string = match required_str(args, "new_string") {
        Ok(value) => value,
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
    let target = match fs_security::validate_existing(snapshot, path) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let working = match mutation::check(snapshot, &target.root, confirm_busy) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let old = match fs::read_to_string(&target.real) {
        Ok(value) => value,
        Err(error) => return fail("read_failed", path, Some(error.to_string())),
    };
    let occurrences = old.match_indices(old_string).count();
    if occurrences != 1 {
        return json!({
            "ok": false,
            "reason": if occurrences == 0 { "old_string_not_found" } else { "old_string_not_unique" },
            "path": target.resolved.to_string_lossy(),
            "occurrences": occurrences,
        });
    }
    if !confirm_dirty {
        match git_tools::file_dirty(&target.root, &target.real) {
            Ok(true) => {
                return json!({
                    "ok": false,
                    "reason": "file_dirty_confirmation_required",
                    "path": target.resolved.to_string_lossy(),
                    "hint": "file has uncommitted changes — re-send with confirm_dirty:true to proceed",
                });
            }
            Ok(false) => {}
            Err(message) => return fail("git_status_failed", path, Some(message)),
        }
    }
    let next = old.replacen(old_string, new_string, 1);
    if let Err(message) = mutation::atomic_write(&target.real, next.as_bytes()) {
        return fail("write_failed", path, Some(message));
    }
    success_with_working(
        json!({
            "ok": true,
            "path": target.resolved.to_string_lossy(),
            "root": target.root.to_string_lossy(),
            "replaced": 1,
            "bytes_before": old.len(),
            "bytes_after": next.len(),
        }),
        working,
    )
}

pub fn write(snapshot: &Value, args: &Value) -> Value {
    let path = match required_str(args, "path") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let content = match required_str(args, "content") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let overwrite = match optional_bool(args, "overwrite") {
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
    let target = match fs_security::validate_target(snapshot, path) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let working = match mutation::check(snapshot, &target.root, confirm_busy) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let existed = target.real.exists();
    if existed && !overwrite {
        return json!({
            "ok": false,
            "reason": "overwrite_confirmation_required",
            "path": target.resolved.to_string_lossy(),
            "hint": "file exists — re-send with overwrite:true (and confirm_dirty:true if dirty)",
        });
    }
    if existed && !confirm_dirty {
        match git_tools::file_dirty(&target.root, &target.real) {
            Ok(true) => {
                return json!({
                    "ok": false,
                    "reason": "file_dirty_confirmation_required",
                    "path": target.resolved.to_string_lossy(),
                    "hint": "existing file has uncommitted changes — re-send with confirm_dirty:true to overwrite",
                });
            }
            Ok(false) => {}
            Err(message) => return fail("git_status_failed", path, Some(message)),
        }
    }
    if let Err(message) = mutation::atomic_write(&target.real, content.as_bytes()) {
        return fail("write_failed", path, Some(message));
    }
    success_with_working(
        json!({
            "ok": true,
            "path": target.resolved.to_string_lossy(),
            "root": target.root.to_string_lossy(),
            "created": !existed,
            "overwritten": existed,
            "bytes": content.len(),
        }),
        working,
    )
}

fn success_with_working(mut result: Value, working: Vec<Value>) -> Value {
    if !working.is_empty()
        && let Some(object) = result.as_object_mut()
    {
        object.insert("warnings".to_owned(), json!({"working": working}));
    }
    result
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
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
            "herdr-mcp-fs-mutation-{}-{timestamp}-{sequence}",
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
        fs::write(root.join("tracked.txt"), "before\n").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "tracked.txt"])
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
    fn edit_is_unique_and_dirty_fail_closed() {
        let root = repo();
        let file = root.join("tracked.txt");
        let snap = snapshot(&root);
        let result = edit(
            &snap,
            &json!({"path": file, "old_string": "before", "new_string": "after"}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(fs::read_to_string(&file).unwrap(), "after\n");
        let blocked = edit(
            &snap,
            &json!({"path": file, "old_string": "after", "new_string": "again"}),
        );
        assert_eq!(blocked["reason"], "file_dirty_confirmation_required");
        let forced = edit(
            &snap,
            &json!({"path": file, "old_string": "after", "new_string": "again", "confirm_dirty": true}),
        );
        assert_eq!(forced["ok"], true);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_requires_explicit_overwrite_and_dirty_confirmation() {
        let root = repo();
        let snap = snapshot(&root);
        let new_file = root.join("new.txt");
        assert_eq!(
            write(&snap, &json!({"path": new_file, "content": "new\n"}))["created"],
            true
        );
        let blocked = write(&snap, &json!({"path": new_file, "content": "next\n"}));
        assert_eq!(blocked["reason"], "overwrite_confirmation_required");
        let dirty = write(
            &snap,
            &json!({"path": new_file, "content": "next\n", "overwrite": true}),
        );
        assert_eq!(dirty["reason"], "file_dirty_confirmation_required");
        let forced = write(
            &snap,
            &json!({"path": new_file, "content": "next\n", "overwrite": true, "confirm_dirty": true}),
        );
        assert_eq!(forced["overwritten"], true);
        assert_eq!(fs::read_to_string(&new_file).unwrap(), "next\n");
        fs::remove_dir_all(root).unwrap();
    }
}
