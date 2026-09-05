use crate::projects::{self, ProjectTopology};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ManagedPath {
    pub root: PathBuf,
    pub resolved: PathBuf,
    pub real: PathBuf,
}

pub fn managed_roots(snapshot: &Value) -> Vec<PathBuf> {
    managed_roots_from(&projects::derive_routing(snapshot))
}

/// Extract managed Git roots from an already-derived routing topology.
///
/// Prefer this when the same request also needs busy-agent checks so the
/// cwd→git-root identity is resolved once for the snapshot.
pub fn managed_roots_from(topology: &ProjectTopology) -> Vec<PathBuf> {
    let mut roots = topology
        .projects
        .values()
        .filter(|project| project.managed && project.vcs == Some("git"))
        .map(|project| project.root.clone())
        .collect::<Vec<_>>();
    roots.sort_by(|left, right| {
        right
            .components()
            .count()
            .cmp(&left.components().count())
            .then_with(|| left.cmp(right))
    });
    roots.dedup();
    roots
}

pub fn validate_existing(snapshot: &Value, input: &str) -> Result<ManagedPath, Value> {
    let roots = managed_roots(snapshot);
    validate_existing_with_roots(&roots, input)
}

/// Validate an existing path using a routing topology already derived for
/// this request.
pub fn validate_existing_with_topology(
    topology: &ProjectTopology,
    input: &str,
) -> Result<ManagedPath, Value> {
    validate_existing_with_roots(&managed_roots_from(topology), input)
}

/// Validate that `input` is exactly one project root already declared by the
/// live topology without touching the filesystem.
///
/// This is intentionally narrower than ordinary fs validation. It exists for
/// command preflight where the tool contract already requires a project root
/// and the actual protected-folder execution is delegated to a Herdr pane.
/// Never use it to authorize arbitrary descendant reads/writes.
pub fn validate_exact_project_root_with_topology(
    topology: &ProjectTopology,
    input: &str,
) -> Result<ManagedPath, Value> {
    let roots = managed_roots_from(topology);
    let resolved = resolve_input(input)?;
    let Some(root) = roots.iter().find(|root| **root == resolved).cloned() else {
        return Err(json!({
            "ok": false,
            "reason": "root_not_project_root",
            "path": resolved.to_string_lossy(),
            "managed_roots": roots.iter().map(|root| root.to_string_lossy()).collect::<Vec<_>>(),
            "hint": "root must exactly match a managed project root from the live snapshot",
        }));
    };
    if denied_secret_path(&resolved) {
        return Err(json!({
            "ok": false,
            "reason": "secret_path_denied",
            "path": resolved.to_string_lossy(),
        }));
    }
    Ok(ManagedPath {
        root: root.clone(),
        resolved: root.clone(),
        real: root,
    })
}

/// Validate an existing path against one project root that was already
/// validated as managed by the caller.
pub fn validate_existing_in_root(project_root: &Path, input: &str) -> Result<ManagedPath, Value> {
    validate_existing_with_roots(&[project_root.to_path_buf()], input)
}

fn validate_existing_with_roots(roots: &[PathBuf], input: &str) -> Result<ManagedPath, Value> {
    let resolved = resolve_input(input)?;
    let Some(root) = containing_root(roots, &resolved).cloned() else {
        return Err(json!({
            "ok": false,
            "reason": "outside_managed_roots",
            "path": resolved.to_string_lossy(),
            "managed_roots": roots.iter().map(|root| root.to_string_lossy()).collect::<Vec<_>>(),
            "hint": outside_managed_roots_hint(),
        }));
    };
    if denied_secret_path(&resolved) {
        return Err(json!({
            "ok": false,
            "reason": "secret_path_denied",
            "path": resolved.to_string_lossy(),
        }));
    }
    let real = std::fs::canonicalize(&resolved).map_err(|error| {
        crate::macos_permissions::io_error_to_fs_value("not_found", &resolved, error)
    })?;
    let root_real = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    if !path_within(&root_real, &real) {
        return Err(json!({
            "ok": false,
            "reason": "symlink_escape",
            "path": resolved.to_string_lossy(),
            "real": real.to_string_lossy(),
        }));
    }
    if denied_secret_path(&real) {
        return Err(json!({
            "ok": false,
            "reason": "secret_path_denied",
            "path": resolved.to_string_lossy(),
            "real": real.to_string_lossy(),
        }));
    }
    Ok(ManagedPath {
        root,
        resolved,
        real,
    })
}

/// Validate a writable target using a routing topology already derived for
/// this request.
pub fn validate_target_with_topology(
    topology: &ProjectTopology,
    input: &str,
) -> Result<ManagedPath, Value> {
    validate_target_with_roots(&managed_roots_from(topology), input)
}

/// Validate a writable target against one project root that was already
/// validated as managed by the caller.
pub fn validate_target_in_root(project_root: &Path, input: &str) -> Result<ManagedPath, Value> {
    validate_target_with_roots(&[project_root.to_path_buf()], input)
}

fn validate_target_with_roots(roots: &[PathBuf], input: &str) -> Result<ManagedPath, Value> {
    let resolved = resolve_input(input)?;
    let Some(root) = containing_root(roots, &resolved).cloned() else {
        return Err(json!({
            "ok": false,
            "reason": "outside_managed_roots",
            "path": resolved.to_string_lossy(),
            "managed_roots": roots.iter().map(|root| root.to_string_lossy()).collect::<Vec<_>>(),
            "hint": outside_managed_roots_hint(),
        }));
    };
    if denied_secret_path(&resolved) {
        return Err(
            json!({"ok": false, "reason": "secret_path_denied", "path": resolved.to_string_lossy()}),
        );
    }
    let root_real = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let real = if resolved.exists() {
        std::fs::canonicalize(&resolved).map_err(|error| {
            crate::macos_permissions::io_error_to_fs_value("target_unresolvable", &resolved, error)
        })?
    } else {
        let parent = resolved.parent().ok_or_else(|| {
            json!({
                "ok": false,
                "reason": "parent_not_found",
                "path": resolved.to_string_lossy(),
            })
        })?;
        let parent_real = std::fs::canonicalize(parent).map_err(|error| {
            crate::macos_permissions::io_error_to_fs_value("parent_not_found", &resolved, error)
        })?;
        if !parent_real.is_dir() {
            return Err(
                json!({"ok": false, "reason": "parent_not_directory", "path": resolved.to_string_lossy()}),
            );
        }
        let name = resolved.file_name().ok_or_else(|| {
            json!({
                "ok": false,
                "reason": "invalid_target",
                "path": resolved.to_string_lossy(),
            })
        })?;
        parent_real.join(name)
    };
    if !path_within(&root_real, &real) {
        return Err(json!({
            "ok": false,
            "reason": "symlink_escape",
            "path": resolved.to_string_lossy(),
            "real": real.to_string_lossy(),
        }));
    }
    if denied_secret_path(&real) {
        return Err(json!({
            "ok": false,
            "reason": "secret_path_denied",
            "path": resolved.to_string_lossy(),
            "real": real.to_string_lossy(),
        }));
    }
    Ok(ManagedPath {
        root,
        resolved,
        real,
    })
}

/// Shared actionable hint for `outside_managed_roots` failures.
///
/// A path is rejected because it is not inside a git-backed project root
/// visible in the live Herdr snapshot. When the snapshot exposes no managed
/// roots, the hint must tell the user to open/create a Herdr workspace or pane
/// whose cwd is inside the intended Git repository, keep it available, then
/// retry. A coding agent is not required for this step.
pub fn outside_managed_roots_hint() -> &'static str {
    "only paths inside git-backed project roots visible in the live snapshot are accessible; open or create a Herdr workspace/pane whose cwd is inside the intended Git repository and keep it available, then retry (no coding agent required)"
}

pub fn denied_secret_path(path: &Path) -> bool {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    if lower.ends_with("/.git/config") {
        return true;
    }

    path.components().any(|component| {
        let segment = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        if segment == ".git" {
            return false;
        }
        segment == ".env"
            || segment.starts_with(".env.")
            || segment.starts_with("id_rsa")
            || segment.starts_with("id_dsa")
            || segment.starts_with("id_ecdsa")
            || segment.starts_with("id_ed25519")
            || segment.contains("secret")
            || segment.contains("token")
            || segment.contains("credential")
            || segment.ends_with(".env")
            || segment.ends_with(".pem")
            || segment.ends_with(".key")
            || segment.ends_with(".p12")
            || segment.ends_with(".pfx")
    })
}

pub fn path_within(root: &Path, path: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn containing_root<'a>(roots: &'a [PathBuf], path: &Path) -> Option<&'a PathBuf> {
    roots.iter().find(|root| path_within(root, path))
}

fn resolve_input(input: &str) -> Result<PathBuf, Value> {
    let path = PathBuf::from(input);
    let resolved = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| {
                json!({
                    "ok": false,
                    "reason": "path_resolution_failed",
                    "path": input,
                    "message": error.to_string(),
                })
            })?
    };
    Ok(normalize_lexical(&resolved))
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                output.pop();
            }
            other => output.push(other.as_os_str()),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
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
            "herdr-mcp-security-{}-{timestamp}-{sequence}",
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

    fn snapshot(root: &Path) -> Value {
        json!({
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": root.to_string_lossy()
            }],
            "agents": []
        })
    }

    #[test]
    fn managed_roots_from_matches_snapshot_derive() {
        let root = repo();
        let snap = snapshot(&root);
        let topology = projects::derive_routing(&snap);
        assert_eq!(managed_roots(&snap), managed_roots_from(&topology));
        let file = root.join("src.txt");
        fs::write(&file, "hello").unwrap();
        let via_topology =
            validate_existing_with_topology(&topology, file.to_str().unwrap()).unwrap();
        let via_snapshot = validate_existing(&snap, file.to_str().unwrap()).unwrap();
        assert_eq!(via_topology.root, via_snapshot.root);
        assert_eq!(via_topology.real, via_snapshot.real);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_project_root_validation_is_metadata_only() {
        let root = PathBuf::from("/Users/example/Documents/not-present-on-test-host");
        let mut project_map = std::collections::BTreeMap::new();
        project_map.insert(
            root.clone(),
            crate::projects::ProjectInfo {
                root: root.clone(),
                vcs: Some("git"),
                managed: true,
                dirty: false,
                changed_files: 0,
                git_status_observed: false,
                git_status_source: None,
                pane_ids: vec!["w1:p1".to_owned()],
                cwds: vec![root.clone()],
            },
        );
        let topology = ProjectTopology {
            projects: project_map,
            pane_to_workspace: [("w1:p1".to_owned(), "w1".to_owned())]
                .into_iter()
                .collect(),
        };

        let managed = validate_exact_project_root_with_topology(
            &topology,
            root.to_str().expect("utf8 test path"),
        )
        .expect("declared root should not need to exist on disk");
        assert_eq!(managed.root, root);
        assert_eq!(managed.real, managed.root);

        let nested = managed.root.join("nested");
        let error = validate_exact_project_root_with_topology(
            &topology,
            nested.to_str().expect("utf8 test path"),
        )
        .expect_err("descendant is not an exact command project root");
        assert_eq!(error["reason"], "root_not_project_root");
    }

    #[test]
    fn accepts_existing_file_inside_managed_git_root() {
        let root = repo();
        let file = root.join("src.txt");
        fs::write(&file, "hello").unwrap();
        let validated = validate_existing(&snapshot(&root), file.to_str().unwrap()).unwrap();
        assert_eq!(validated.root, root);
        assert_eq!(validated.real, fs::canonicalize(&file).unwrap());
        fs::remove_dir_all(validated.root).unwrap();
    }

    #[test]
    fn accepts_new_target_only_when_parent_is_managed_and_real() {
        let root = repo();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        let target = src.join("new.rs");
        let topology = projects::derive_routing(&snapshot(&root));
        let validated = validate_target_with_topology(&topology, target.to_str().unwrap()).unwrap();
        assert_eq!(validated.root, root);
        assert_eq!(
            validated.real,
            fs::canonicalize(&src).unwrap().join("new.rs")
        );

        let missing_parent = root.join("missing/new.rs");
        let error =
            validate_target_with_topology(&topology, missing_parent.to_str().unwrap()).unwrap_err();
        assert_eq!(error["reason"], "parent_not_found");
        fs::remove_dir_all(validated.root).unwrap();
    }

    #[test]
    fn rejects_secret_paths_and_outside_roots() {
        let root = repo();
        let secret = root.join(".env.production");
        fs::write(&secret, "secret").unwrap();
        let result = validate_existing(&snapshot(&root), secret.to_str().unwrap()).unwrap_err();
        assert_eq!(result["reason"], "secret_path_denied");

        let outside = std::env::temp_dir().join("herdr-mcp-outside.txt");
        fs::write(&outside, "outside").unwrap();
        let result = validate_existing(&snapshot(&root), outside.to_str().unwrap()).unwrap_err();
        assert_eq!(result["reason"], "outside_managed_roots");
        let _ = fs::remove_file(outside);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let root = repo();
        let outside =
            std::env::temp_dir().join(format!("herdr-mcp-outside-{}.txt", std::process::id()));
        fs::write(&outside, "outside").unwrap();
        let link = root.join("escape.txt");
        symlink(&outside, &link).unwrap();
        let result = validate_existing(&snapshot(&root), link.to_str().unwrap()).unwrap_err();
        assert_eq!(result["reason"], "symlink_escape");
        let _ = fs::remove_file(outside);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn secret_matcher_covers_current_denied_classes() {
        for path in [
            "/repo/.env",
            "/repo/.env.local",
            "/repo/key.pem",
            "/repo/private.key",
            "/repo/id_ed25519",
            "/repo/api-token.txt",
            "/repo/client_credentials.json",
            "/repo/.git/config",
        ] {
            assert!(denied_secret_path(Path::new(path)), "{path}");
        }
        assert!(!denied_secret_path(Path::new("/repo/src/config.ts")));
    }

    #[test]
    fn empty_managed_roots_hint_is_actionable_without_a_coding_agent() {
        // v0.4.6 #2: when the live snapshot exposes no managed git roots, the
        // hint must explicitly direct the user to open/create a Herdr workspace
        // or pane inside the intended Git repository, keep it available, then
        // retry — and must not imply a coding agent is required.
        let hint = outside_managed_roots_hint().to_ascii_lowercase();
        assert!(hint.contains("herdr workspace"), "{hint}");
        assert!(hint.contains("pane"), "{hint}");
        assert!(hint.contains("cwd"));
        assert!(hint.contains("git repository"));
        assert!(hint.contains("keep it available"));
        assert!(hint.contains("retry"));
        assert!(hint.contains("coding agent"));
        assert!(hint.contains("no coding agent required"));
    }

    #[cfg(unix)]
    #[test]
    fn outside_existing_file_carries_the_actionable_hint() {
        let root = repo();
        let outside =
            std::env::temp_dir().join(format!("herdr-mcp-hint-{}.txt", std::process::id()));
        fs::write(&outside, "outside").unwrap();
        let result = validate_existing(&snapshot(&root), outside.to_str().unwrap()).unwrap_err();
        assert_eq!(result["reason"], "outside_managed_roots");
        let hint = result["hint"].as_str().unwrap();
        assert!(hint.contains("Herdr workspace/pane"));
        assert!(hint.contains("keep it available"));
        let _ = fs::remove_file(outside);
        fs::remove_dir_all(root).unwrap();
    }
}
