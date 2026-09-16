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

/// Unified validated-root access surface: managed Git roots plus vcs-less
/// Operational roots proven by the live topology.
///
/// `validated_roots_from` is the single root set for the read/exec surface
/// (`herdr_fs_read` / `herdr_fs_list` / `herdr_fs_grep` / `herdr_exec`).
/// [`managed_roots_from`] keeps the pre-existing Git-only meaning for every
/// Git-oriented consumer, so Git semantics are unchanged.
pub fn validated_roots_from(topology: &ProjectTopology) -> Vec<PathBuf> {
    sorted_deepest_first(
        topology
            .projects
            .values()
            .filter_map(root_kind_of)
            .map(|(root, _)| root)
            .collect(),
    )
}

/// Extract managed Git roots from an already-derived routing topology.
///
/// Git roots are exactly the `managed && vcs == Some("git")` projects; this
/// stays Git-only so no Git-oriented consumer widens silently.
///
/// Prefer this when the same request also needs busy-agent checks so the
/// cwd→git-root identity is resolved once for the snapshot.
pub fn managed_roots_from(topology: &ProjectTopology) -> Vec<PathBuf> {
    sorted_deepest_first(
        topology
            .projects
            .values()
            .filter(|project| project.managed && project.vcs == Some("git"))
            .map(|project| project.root.clone())
            .collect(),
    )
}

fn sorted_deepest_first(mut roots: Vec<PathBuf>) -> Vec<PathBuf> {
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

/// Whether a root validated by the unified surface is vcs-less Operational.
///
/// Operational roots are read/exec-only: mutations that rely on Git-dirty
/// confirmation must fail closed for them instead of fabricating Git state.
pub fn is_operational_root(topology: &ProjectTopology, root: &Path) -> bool {
    topology.projects.get(root).is_some_and(|project| {
        root_kind_of(project).is_some_and(|(_, kind)| kind == RootKind::Operational)
    })
}

/// Deterministic fail-closed guard for mutations whose safety depends on
/// Git-dirty confirmation. Returns the error value when `root` is an
/// operational root, otherwise `None`.
pub fn reject_operational_root_mutation(topology: &ProjectTopology, root: &Path) -> Option<Value> {
    is_operational_root(topology, root).then(|| {
        json!({
            "ok": false,
            "reason": "operational_root_mutation_unsupported",
            "root": root.to_string_lossy(),
            "hint": "this vcs-less operational root has no Git dirty state to confirm against; write inside a Git-backed project root, or restrict this change to reading it",
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootKind {
    /// A Git-backed managed root (pre-existing behavior, unchanged).
    Git,
    /// A vcs-less canonical existing directory exactly proven by live
    /// workspace/pane cwd.
    Operational,
}

fn root_kind_of(project: &projects::ProjectInfo) -> Option<(PathBuf, RootKind)> {
    let home = home_dir();
    match project.vcs {
        Some("git") => project
            .managed
            .then_some((project.root.clone(), RootKind::Git)),
        None if operational_root_eligible(project, home.as_deref()) => {
            Some((project.root.clone(), RootKind::Operational))
        }
        _ => None,
    }
}

/// An Operational root must be a vcs-less canonical existing directory that
/// the live workspace/pane cwds prove exactly.
///
/// Rejected: HOME itself or any ancestor of HOME, any unproven sibling, a
/// secret-like path, and a symlink escape (the canonical real path must still
/// be a directory that is not HOME or an ancestor of HOME).
fn operational_root_eligible(project: &projects::ProjectInfo, home: Option<&Path>) -> bool {
    if project.vcs.is_some() || project.cwds.is_empty() {
        return false;
    }
    let Some(home) = home else {
        // Without a known HOME the ancestry guard cannot be evaluated, so a
        // vcs-less root fails closed instead of guessing.
        return false;
    };
    let root = &project.root;
    if !root.is_absolute() || rejects_home_ancestry(home, root) || denied_secret_path(root) {
        return false;
    }
    // "canonical existing directory": the live cwd must resolve to a real
    // directory, and that real directory must itself survive the HOME-ancestry
    // and secret-path guards, so a symlinked cwd cannot escape them.
    let root_real = match std::fs::canonicalize(root) {
        Ok(real) => {
            if !real.is_dir() || rejects_home_ancestry(home, &real) || denied_secret_path(&real) {
                return false;
            }
            Some(real)
        }
        // The rotating runtime is deliberately not the macOS TCC client, so
        // Documents/Desktop/Downloads existence, symlink resolution, and real
        // paths are verified by the stable broker (`herdr_fs_*`) or the
        // delegated utility pane (`herdr_exec`) instead — the same route the
        // pre-existing protected-root command preflight already uses. Every
        // other unresolvable path is refused.
        Err(_) if tcc_delegated_root(home, root) => None,
        Err(_) => return false,
    };
    // Exactly proven by live cwd: every cwd attached to this project resolves
    // to this root's real directory (or, when the real path is delegated to
    // TCC, must be this exact cwd). A sibling outside the group therefore
    // cannot borrow this root's authority.
    project
        .cwds
        .iter()
        .all(|cwd| match (&root_real, std::fs::canonicalize(cwd)) {
            (Some(root_real), Ok(cwd_real)) => &cwd_real == root_real,
            _ => cwd == root,
        })
}

/// macOS Documents/Desktop/Downloads roots are executed/read through the TCC
/// broker or utility pane, which can verify what this runtime cannot.
fn tcc_delegated_root(home: &Path, candidate: &Path) -> bool {
    cfg!(target_os = "macos")
        && ["Documents", "Desktop", "Downloads"]
            .iter()
            .any(|name| candidate.starts_with(home.join(name)))
}

/// HOME itself, or any ancestor of HOME (for example `/`, `/Users`), is never
/// an operational root. This also covers a symlinked cwd whose real target is
/// HOME or above it.
fn rejects_home_ancestry(home: &Path, candidate: &Path) -> bool {
    home == candidate || home.starts_with(candidate)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Validate an existing path against managed Git roots only.
///
/// Git-oriented consumers (for example `herdr_git`) keep this pre-existing
/// Git-only boundary so Git semantics do not change.
pub fn validate_existing(snapshot: &Value, input: &str) -> Result<ManagedPath, Value> {
    if let Ok(resolved) = resolve_input(input)
        && let Some(root) = target_scoped_herdr_worktree_root(snapshot, &resolved)
    {
        return validate_existing_with_roots(&[root], input);
    }
    let roots = managed_roots(snapshot);
    validate_existing_with_roots(&roots, input)
}

fn target_scoped_herdr_worktree_root(snapshot: &Value, resolved: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    target_scoped_worktree_root_under(snapshot, resolved, &home.join(".herdr/worktrees"))
}

fn target_scoped_worktree_root_under(
    snapshot: &Value,
    resolved: &Path,
    worktrees_root: &Path,
) -> Option<PathBuf> {
    if !resolved.is_absolute() || !resolved.starts_with(worktrees_root) {
        return None;
    }

    let mut current = resolved.to_path_buf();
    let root = loop {
        if current == worktrees_root {
            return None;
        }
        if current.join(".git").exists() {
            break current;
        }
        if !current.pop() || !current.starts_with(worktrees_root) {
            return None;
        }
    };

    let live_cwd_declares_root = ["panes", "agents"].into_iter().any(|key| {
        snapshot
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                item.get("cwd")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("foreground_cwd").and_then(Value::as_str))
            })
            .map(Path::new)
            .any(|cwd| cwd.starts_with(&root))
    });
    let live_workspace_declares_root = snapshot
        .get("workspaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|workspace| workspace.get("worktree").and_then(Value::as_object))
        .filter_map(|worktree| {
            worktree
                .get("checkout_path")
                .and_then(Value::as_str)
                .or_else(|| worktree.get("path").and_then(Value::as_str))
        })
        .map(Path::new)
        .any(|checkout| checkout == root);

    (live_cwd_declares_root || live_workspace_declares_root).then_some(root)
}

/// Validate an existing path against the unified validated-root surface
/// (managed Git roots plus operational roots proven by the live topology).
pub fn validate_existing_validated(snapshot: &Value, input: &str) -> Result<ManagedPath, Value> {
    validate_existing_validated_with_topology(&projects::derive_routing(snapshot), input)
}

/// See [`validate_existing_validated`]. Reuses one routing topology per
/// request so the root identity is resolved once.
pub fn validate_existing_validated_with_topology(
    topology: &ProjectTopology,
    input: &str,
) -> Result<ManagedPath, Value> {
    validate_existing_with_roots(&validated_roots_from(topology), input)
}

/// Validate that `input` is exactly one root on the unified validated-root
/// surface, using only live topology metadata.
///
/// This is the protected-path (macOS Documents/Desktop/Downloads) preflight
/// form: existence is delegated to the Herdr utility pane, exactly as the
/// pre-existing Git-only variant does.
pub fn validate_exact_validated_root_with_topology(
    topology: &ProjectTopology,
    input: &str,
) -> Result<ManagedPath, Value> {
    validate_exact_root_with_roots(&validated_roots_from(topology), input)
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
    validate_exact_root_with_roots(&managed_roots_from(topology), input)
}

fn validate_exact_root_with_roots(roots: &[PathBuf], input: &str) -> Result<ManagedPath, Value> {
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

/// Validate a writable target against the unified validated-root surface.
///
/// Callers must still fail closed for operational roots when their safety
/// depends on Git-dirty confirmation; see
/// [`reject_operational_root_mutation`].
pub fn validate_target_validated_with_topology(
    topology: &ProjectTopology,
    input: &str,
) -> Result<ManagedPath, Value> {
    validate_target_with_roots(&validated_roots_from(topology), input)
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
/// A path is rejected because it is neither inside a Git-backed project root
/// nor inside a vcs-less operational root proven by the live Herdr snapshot.
/// When the snapshot exposes no validated roots, the hint must tell the user
/// to open/create a Herdr workspace or pane whose cwd is exactly the intended
/// directory, keep it available, then retry. A coding agent is not required
/// for this step.
pub fn outside_managed_roots_hint() -> &'static str {
    "only paths inside project roots visible in the live snapshot are accessible: a Git-backed project root, or a non-Git operational root exactly proven by a live Herdr workspace/pane cwd; open or create a Herdr workspace/pane whose cwd is exactly the intended directory (or is inside the intended Git repository) and keep it available, then retry (no coding agent required)"
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

    /// A vcs-less scratch parent plus the named child directories under it.
    fn plain_parent(name: &str, children: &[&str]) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_REPO_ID.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "herdr-mcp-operational-{name}-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&parent).unwrap();
        for child in children {
            fs::create_dir_all(parent.join(child)).unwrap();
        }
        parent
    }

    #[test]
    fn exact_live_non_git_cwd_is_an_operational_root() {
        let parent = plain_parent("exact", &["project"]);
        let root = parent.join("project");
        let file = root.join("artifact.md");
        fs::write(&file, "lark-cli output\n").unwrap();
        let snap = snapshot(&root);

        let topology = projects::derive_routing(&snap);
        let project = topology.projects.get(&root).unwrap();
        assert_eq!(project.vcs, None);
        // Operational roots never claim managed-Git or fabricate Git state.
        assert!(!project.managed);
        assert!(!project.dirty);
        assert_eq!(project.changed_files, 0);
        assert!(!project.git_status_observed);
        assert_eq!(project.git_status_source, None);
        assert!(is_operational_root(&topology, &root));

        let validated = validate_existing_validated(&snap, file.to_str().unwrap()).unwrap();
        assert_eq!(validated.root, root);
        assert_eq!(validated.real, fs::canonicalize(&file).unwrap());
        assert_eq!(
            validated_roots_from(&topology),
            vec![root.clone()],
            "the proven live cwd is the operational root identity"
        );

        // Git semantics are unchanged: the Git-only surface still refuses it.
        assert_eq!(managed_roots(&snap), Vec::<PathBuf>::new());
        assert_eq!(
            validate_existing(&snap, file.to_str().unwrap()).unwrap_err()["reason"],
            "outside_managed_roots"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn unproven_siblings_and_parent_directories_are_rejected() {
        let parent = plain_parent("sibling", &["project", "other", "elsewhere"]);
        let root = parent.join("project");
        let sibling_file = parent.join("other/artifact.md");
        fs::write(&sibling_file, "not mine\n").unwrap();
        let snap = snapshot(&root);

        // A sibling directory is never covered by the proven root.
        assert_eq!(
            validate_existing_validated(&snap, sibling_file.to_str().unwrap()).unwrap_err()["reason"],
            "outside_managed_roots"
        );
        // Neither is the shared parent that only contains the proven root.
        assert_eq!(
            validate_existing_validated(&snap, parent.to_str().unwrap()).unwrap_err()["reason"],
            "outside_managed_roots"
        );
        // A declared workspace root without an exact live cwd is not proof.
        let declared = json!({
            "workspaces": [{
                "workspace_id": "w1",
                "worktree": {"checkout_path": root.to_string_lossy()}
            }],
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": parent.join("elsewhere").to_string_lossy()
            }],
            "agents": []
        });
        assert_eq!(
            validate_existing_validated(&declared, root.join("artifact.md").to_str().unwrap())
                .unwrap_err()["reason"],
            "outside_managed_roots"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn tcc_delegated_documents_root_is_proven_by_metadata_and_still_fails_closed() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set on this host"));
        // Never touch user data: the directory deliberately does not exist, so
        // this exercises exactly the runtime's TCC-delegated case (the
        // rotating runtime cannot resolve Documents paths by design).
        let root = home
            .join("Documents")
            .join("herdr-mcp-operational-root-not-present");
        let snap = snapshot(&root);
        let topology = projects::derive_routing(&snap);
        assert!(is_operational_root(&topology, &root));
        assert!(
            validate_exact_validated_root_with_topology(&topology, root.to_str().unwrap()).is_ok(),
            "the protected exec preflight is metadata-only"
        );

        // Reads still fail closed: existence/symlink verification belongs to
        // the TCC broker, never to an inferred directory.
        let refused = validate_existing_validated(&snap, root.to_str().unwrap()).unwrap_err();
        assert_ne!(refused["ok"], true, "{refused}");
        assert!(
            matches!(
                refused["reason"].as_str(),
                Some("not_found") | Some("macos_tcc_access_blocked")
            ),
            "{refused}"
        );

        // HOME itself under the same hierarchical prefix is still refused.
        assert_eq!(
            validate_existing_validated(&snapshot(&home), home.to_str().unwrap()).unwrap_err()["reason"],
            "outside_managed_roots"
        );
    }

    #[test]
    fn home_and_its_ancestors_are_never_operational_roots() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set on this host"));
        let mut candidates = vec![home.clone()];
        let mut ancestor = home.as_path();
        while let Some(parent) = ancestor.parent() {
            candidates.push(parent.to_path_buf());
            ancestor = parent;
        }
        for candidate in candidates {
            let snap = snapshot(&candidate);
            assert_eq!(
                validate_existing_validated(&snap, candidate.to_str().unwrap()).unwrap_err()["reason"],
                "outside_managed_roots",
                "{} must never be an operational root",
                candidate.display()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_home_is_not_an_operational_root() {
        use std::os::unix::fs::symlink;
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set on this host"));
        let parent = plain_parent("symlink-home", &[]);
        let link = parent.join("innocent");
        symlink(&home, &link).unwrap();
        let snap = snapshot(&link);
        assert_eq!(
            validate_existing_validated(&snap, link.to_str().unwrap()).unwrap_err()["reason"],
            "outside_managed_roots"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn secret_like_roots_and_targets_are_rejected() {
        let parent = plain_parent("denied", &["work", "api-secrets", "credentials-store"]);
        for child in ["api-secrets", "credentials-store"] {
            let root = parent.join(child);
            let snap = snapshot(&root);
            assert_eq!(
                validate_existing_validated(&snap, root.to_str().unwrap()).unwrap_err()["reason"],
                "outside_managed_roots",
                "{child} must not become an operational root"
            );
        }

        let root = parent.join("work");
        let env_file = root.join(".env");
        fs::write(&env_file, "TOKEN=1\n").unwrap();
        let snap = snapshot(&root);
        assert_eq!(
            validate_existing_validated(&snap, env_file.to_str().unwrap()).unwrap_err()["reason"],
            "secret_path_denied"
        );
        assert!(validate_existing_validated(&snap, root.to_str().unwrap()).is_ok());
        fs::remove_dir_all(parent).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_inside_an_operational_root_is_rejected() {
        use std::os::unix::fs::symlink;
        let parent = plain_parent("escape", &["project"]);
        let root = parent.join("project");
        let outside = parent.join("outside.txt");
        fs::write(&outside, "outside\n").unwrap();
        let link = root.join("escape.txt");
        symlink(&outside, &link).unwrap();
        let snap = snapshot(&root);
        assert_eq!(
            validate_existing_validated(&snap, link.to_str().unwrap()).unwrap_err()["reason"],
            "symlink_escape"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn git_root_behavior_is_unchanged_by_the_operational_surface() {
        let root = repo();
        let file = root.join("src.txt");
        fs::write(&file, "hello").unwrap();
        let snap = snapshot(&root);
        let topology = projects::derive_routing(&snap);

        assert!(managed_roots_from(&topology).contains(&root));
        assert!(!is_operational_root(&topology, &root));
        assert!(reject_operational_root_mutation(&topology, &root).is_none());
        let git_only = validate_existing(&snap, file.to_str().unwrap()).unwrap();
        let validated = validate_existing_validated(&snap, file.to_str().unwrap()).unwrap();
        assert_eq!(git_only.root, validated.root);
        assert_eq!(git_only.real, validated.real);
        assert_eq!(
            validate_exact_project_root_with_topology(&topology, root.to_str().unwrap())
                .unwrap()
                .root,
            root
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operational_roots_fail_closed_for_git_dirty_confirmation() {
        let parent = plain_parent("mutation", &["project"]);
        let root = parent.join("project");
        let snap = snapshot(&root);
        let topology = projects::derive_routing(&snap);
        let denied = reject_operational_root_mutation(&topology, &root)
            .expect("operational roots must fail closed for Git-dirty gates");
        assert_eq!(denied["ok"], false);
        assert_eq!(denied["reason"], "operational_root_mutation_unsupported");
        fs::remove_dir_all(parent).unwrap();
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
            validate_existing_validated_with_topology(&topology, file.to_str().unwrap()).unwrap();
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
    fn target_scoped_worktree_root_requires_live_snapshot_ownership() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let worktrees_root = std::env::temp_dir().join(format!(
            "herdr-mcp-worktree-scope-{}-{timestamp}",
            std::process::id()
        ));
        let root = worktrees_root.join("rc");
        let file = root.join("src/lib.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            root.join(".git"),
            "gitdir: /protected/repo/.git/worktrees/rc\n",
        )
        .unwrap();
        fs::write(&file, "hello\n").unwrap();

        let live = json!({
            "panes": [
                {"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root},
                {"pane_id": "w2:p1", "workspace_id": "w2", "cwd": "/Users/example/Documents/unrelated"}
            ],
            "agents": []
        });
        assert_eq!(
            target_scoped_worktree_root_under(&live, &file, &worktrees_root),
            Some(root.clone())
        );

        let declared = json!({
            "workspaces": [{"workspace_id": "w1", "worktree": {"checkout_path": root}}],
            "panes": [{"pane_id": "w2:p1", "workspace_id": "w2", "cwd": "/tmp/unrelated"}],
            "agents": []
        });
        assert_eq!(
            target_scoped_worktree_root_under(&declared, &file, &worktrees_root),
            Some(root.clone())
        );

        let unrelated = json!({
            "panes": [{"pane_id": "w2:p1", "workspace_id": "w2", "cwd": "/tmp/unrelated"}],
            "agents": []
        });
        assert_eq!(
            target_scoped_worktree_root_under(&unrelated, &file, &worktrees_root),
            None
        );
        fs::remove_dir_all(worktrees_root).unwrap();
    }

    #[test]
    fn accepts_new_target_only_when_parent_is_managed_and_real() {
        let root = repo();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        let target = src.join("new.rs");
        let topology = projects::derive_routing(&snapshot(&root));
        let validated =
            validate_target_validated_with_topology(&topology, target.to_str().unwrap()).unwrap();
        assert_eq!(validated.root, root);
        assert_eq!(
            validated.real,
            fs::canonicalize(&src).unwrap().join("new.rs")
        );

        let missing_parent = root.join("missing/new.rs");
        let error =
            validate_target_validated_with_topology(&topology, missing_parent.to_str().unwrap())
                .unwrap_err();
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
