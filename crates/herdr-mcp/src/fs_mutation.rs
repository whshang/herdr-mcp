use crate::fs_security;
use crate::git_tools;
use crate::mutation;
use crate::projects;
use serde_json::{Value, json};
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
    let topology = projects::derive_routing(snapshot);
    let target = match fs_security::validate_existing_validated_with_topology(&topology, path) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if let Some(error) = fs_security::reject_operational_root_mutation(&topology, &target.root) {
        return error;
    }
    let working =
        match mutation::check_with_topology(snapshot, &topology, &target.root, confirm_busy) {
            Ok(value) => value,
            Err(error) => return error,
        };
    let old = match fs::read_to_string(&target.real) {
        Ok(value) => value,
        Err(error) => {
            return crate::macos_permissions::io_error_to_fs_value(
                "read_failed",
                &target.resolved,
                error,
            );
        }
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
                    "hint": "Uncommitted changes are present; confirm_dirty=true acknowledges editing this file.",
                });
            }
            Ok(false) => {}
            Err(message) => {
                return crate::macos_permissions::git_failure_to_value(
                    &target.root,
                    "status",
                    message,
                );
            }
        }
    }
    let next = old.replacen(old_string, new_string, 1);
    if let Err(message) = mutation::atomic_write(&target.real, next.as_bytes()) {
        return crate::macos_permissions::io_message_to_fs_value(
            "write_failed",
            &target.resolved,
            message,
        );
    }
    // `edit` writes through `atomic_write` directly, not `write_bytes`. A
    // successful replace still dirties the managed repository, so the next
    // inspect must not reuse the pre-edit git-status cache.
    // Use the caller-facing path. `target.real` is canonicalized and can miss
    // a cache key that still uses the snapshot root (for example `/var` versus
    // `/private/var` on macOS).
    projects::invalidate_git_status_cache_for_path(&target.resolved);
    projects::invalidate_git_status_cache_for_path(&target.real);
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
    write_bytes(
        snapshot,
        path,
        content.as_bytes(),
        overwrite,
        confirm_dirty,
        confirm_busy,
    )
}

pub(crate) fn write_bytes(
    snapshot: &Value,
    path: &str,
    content: &[u8],
    overwrite: bool,
    confirm_dirty: bool,
    confirm_busy: bool,
) -> Value {
    let topology = projects::derive_routing(snapshot);
    let target = match fs_security::validate_target_validated_with_topology(&topology, path) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if let Some(error) = fs_security::reject_operational_root_mutation(&topology, &target.root) {
        return error;
    }
    let working =
        match mutation::check_with_topology(snapshot, &topology, &target.root, confirm_busy) {
            Ok(value) => value,
            Err(error) => return error,
        };
    let existed = target.real.exists();
    if existed && !overwrite {
        return json!({
            "ok": false,
            "reason": "overwrite_confirmation_required",
            "path": target.resolved.to_string_lossy(),
            "hint": "The file already exists; overwrite=true acknowledges replacement, and confirm_dirty=true is also required when it has uncommitted changes.",
        });
    }
    if existed && !confirm_dirty {
        match git_tools::file_dirty(&target.root, &target.real) {
            Ok(true) => {
                return json!({
                    "ok": false,
                    "reason": "file_dirty_confirmation_required",
                    "path": target.resolved.to_string_lossy(),
                    "hint": "The existing file has uncommitted changes; confirm_dirty=true acknowledges replacement.",
                });
            }
            Ok(false) => {}
            Err(message) => {
                return crate::macos_permissions::git_failure_to_value(
                    &target.root,
                    "status",
                    message,
                );
            }
        }
    }
    if let Err(message) = mutation::atomic_write(&target.real, content) {
        return crate::macos_permissions::io_message_to_fs_value(
            "write_failed",
            &target.resolved,
            message,
        );
    }
    projects::invalidate_git_status_cache(&target.root);
    projects::invalidate_git_status_cache_for_path(&target.resolved);
    projects::invalidate_git_status_cache_for_path(&target.real);
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
    fn operational_root_mutation_fails_closed_without_touching_git() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-fs-operational-mutation-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let existing = root.join("artifact.md");
        fs::write(&existing, "before\n").unwrap();
        let snap = json!({
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}],
            "agents": []
        });

        // Reads and listings still work on the operational root.
        assert_eq!(
            crate::fs_tools::read(&snap, &json!({"path": existing}))["ok"],
            true
        );

        // Every mutation path fails closed deterministically instead of
        // borrowing Git-dirty semantics the root does not have.
        let edited = edit(
            &snap,
            &json!({"path": existing, "old_string": "before", "new_string": "after"}),
        );
        assert_eq!(edited["reason"], "operational_root_mutation_unsupported");
        let written = write(
            &snap,
            &json!({"path": root.join("new.md"), "content": "new\n"}),
        );
        assert_eq!(written["reason"], "operational_root_mutation_unsupported");
        let patched = crate::fs_patch::apply(
            &snap,
            &json!({
                "root": root,
                "patch": "*** Begin Patch\n*** Add File: added.md\n+added\n*** End Patch\n"
            }),
        );
        assert_eq!(patched["reason"], "operational_root_mutation_unsupported");

        assert_eq!(fs::read_to_string(&existing).unwrap(), "before\n");
        assert!(!root.join("new.md").exists());
        assert!(!root.join("added.md").exists());
        fs::remove_dir_all(root).unwrap();
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

    #[test]
    fn exec_write_invalidates_cache_and_next_inspect_observes_git_status() {
        let root = repo();
        let snap = snapshot(&root);

        projects::invalidate_git_status_cache(&root);
        let clean = projects::derive(&snap);
        assert!(!clean.projects[&root].dirty);

        let written = write(
            &snap,
            &json!({"path": root.join("created-by-write.txt"), "content": "changed\n"}),
        );
        assert_eq!(written["ok"], true);

        // No manual invalidate: the write itself must have invalidated the
        // cache, so the next inspect observes the updated git status.
        let refreshed = projects::derive(&snap);
        let project = &refreshed.projects[&root];
        assert!(project.dirty);
        assert_eq!(project.changed_files, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn edit_invalidates_cache_and_next_inspect_observes_git_status() {
        let root = repo();
        let snap = snapshot(&root);
        let file = root.join("tracked.txt");

        projects::invalidate_git_status_cache(&root);
        projects::reset_git_status_probe_count(&root);
        let clean = projects::derive(&snap);
        assert!(!clean.projects[&root].dirty);
        assert_eq!(
            projects::git_status_probe_count(&root),
            1,
            "the clean inspect must probe once"
        );
        let cached = projects::derive(&snap);
        assert!(!cached.projects[&root].dirty);
        assert_eq!(
            projects::git_status_probe_count(&root),
            1,
            "the pre-edit inspect must hit the live cache, not expire across the 250ms TTL"
        );

        let edited = edit(
            &snap,
            &json!({"path": file, "old_string": "before", "new_string": "after"}),
        );
        assert_eq!(edited["ok"], true, "{edited}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "after\n");

        // No manual invalidate: the successful edit must drop the cached
        // status for the repository that contains the edited file.
        let refreshed = projects::derive(&snap);
        let project = &refreshed.projects[&root];
        assert!(project.dirty, "{project:?}");
        assert!(project.changed_files >= 1, "{project:?}");
        assert_eq!(
            projects::git_status_probe_count(&root),
            2,
            "invalidation must force a new probe instead of reusing the clean entry"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_edit_keeps_the_git_status_cache_and_probe_count() {
        let root = repo();
        let snap = snapshot(&root);
        let file = root.join("tracked.txt");

        projects::invalidate_git_status_cache(&root);
        projects::reset_git_status_probe_count(&root);
        let clean = projects::derive(&snap);
        assert!(!clean.projects[&root].dirty);
        let probes_after_prime = projects::git_status_probe_count(&root);
        assert!(probes_after_prime >= 1);
        assert!(!projects::derive(&snap).projects[&root].dirty);
        assert_eq!(
            projects::git_status_probe_count(&root),
            probes_after_prime,
            "the second inspect must hit the live cache"
        );

        let missing = edit(
            &snap,
            &json!({"path": file, "old_string": "not-in-file", "new_string": "after"}),
        );
        assert_eq!(missing["reason"], "old_string_not_found");
        assert_eq!(
            projects::git_status_probe_count(&root),
            probes_after_prime,
            "a rejected edit must not drop the cached clean status"
        );
        assert!(!projects::derive(&snap).projects[&root].dirty);
        assert_eq!(projects::git_status_probe_count(&root), probes_after_prime);

        fs::write(&file, "dirty\n").unwrap();
        projects::invalidate_git_status_cache(&root);
        projects::reset_git_status_probe_count(&root);
        let dirty = projects::derive(&snap);
        assert!(dirty.projects[&root].dirty);
        let probes_after_dirty = projects::git_status_probe_count(&root);
        assert!(probes_after_dirty >= 1);

        let blocked = edit(
            &snap,
            &json!({"path": file, "old_string": "dirty", "new_string": "again"}),
        );
        assert_eq!(blocked["reason"], "file_dirty_confirmation_required");
        assert_eq!(
            projects::git_status_probe_count(&root),
            probes_after_dirty,
            "a dirty-confirmation refusal must not probe or invalidate again"
        );
        assert!(projects::derive(&snap).projects[&root].dirty);
        assert_eq!(projects::git_status_probe_count(&root), probes_after_dirty);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn edit_through_one_symlink_invalidates_the_other_alias_cache() {
        use std::os::unix::fs::symlink;

        let parent = std::env::temp_dir().join(format!(
            "herdr-mcp-fs-alias-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let real = parent.join("real");
        fs::create_dir_all(&real).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&real)
                .status()
                .unwrap()
                .success()
        );
        fs::write(real.join("tracked.txt"), "before\n").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "tracked.txt"])
                .current_dir(&real)
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
                .current_dir(&real)
                .status()
                .unwrap()
                .success()
        );
        let alias_a = parent.join("alias-a");
        let alias_b = parent.join("alias-b");
        symlink(&real, &alias_a).unwrap();
        symlink(&real, &alias_b).unwrap();

        let snap_b = snapshot(&alias_b);
        projects::invalidate_git_status_cache(&alias_b);
        projects::invalidate_git_status_cache(&real);
        projects::reset_git_status_probe_count(&alias_b);
        projects::reset_git_status_probe_count(&real);
        let clean = projects::derive(&snap_b);
        let clean_root = clean
            .projects
            .iter()
            .find(|(_, project)| project.vcs == Some("git"))
            .map(|(root, _)| root.clone())
            .expect("alias B resolves to a git project");
        assert!(!clean.projects[&clean_root].dirty, "{clean:?}");
        let probes_before = projects::git_status_probe_count(&clean_root)
            + projects::git_status_probe_count(&alias_b)
            + projects::git_status_probe_count(&real);
        assert!(probes_before >= 1, "{clean:?}");
        assert!(!projects::derive(&snap_b).projects[&clean_root].dirty);
        let probes_cached = projects::git_status_probe_count(&clean_root)
            + projects::git_status_probe_count(&alias_b)
            + projects::git_status_probe_count(&real);
        assert_eq!(
            probes_cached, probes_before,
            "alias B must be served from cache before the edit"
        );

        let snap_a = snapshot(&alias_a);
        let edited = edit(
            &snap_a,
            &json!({
                "path": alias_a.join("tracked.txt"),
                "old_string": "before",
                "new_string": "after"
            }),
        );
        assert_eq!(edited["ok"], true, "{edited}");

        let refreshed = projects::derive(&snap_b);
        assert!(
            refreshed.projects[&clean_root].dirty,
            "alias B must not keep the clean cache after an edit through alias A: {refreshed:?}"
        );
        let probes_after = projects::git_status_probe_count(&clean_root)
            + projects::git_status_probe_count(&alias_b)
            + projects::git_status_probe_count(&real);
        assert!(
            probes_after > probes_cached,
            "alias B must probe again inside the TTL"
        );
        fs::remove_dir_all(parent).unwrap();
    }
}
