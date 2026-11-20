use crate::projects::{self, ProjectTopology};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct ManagedPath {
    pub root: PathBuf,
    pub resolved: PathBuf,
    pub real: PathBuf,
}

pub fn managed_roots(snapshot: &Value) -> Vec<PathBuf> {
    managed_roots_from(&projects::derive_routing_cached(snapshot))
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
///
/// The topology may be a cached projection, so a root classified Git-backed
/// is re-verified at this write boundary: if its `.git` marker (a directory
/// for a plain clone, a file for a linked worktree) has been removed or
/// renamed inside the cache TTL, the root is effectively vcs-less and the
/// mutation is refused instead of trusting the stale classification.
///
/// The re-verification distinguishes "marker gone" from "probe failed":
/// macOS privacy-protected roots (Documents/Desktop/Downloads) are listed
/// through the stable TCC broker — the authorized client for those folders —
/// because the rotating runtime must never probe them itself (a direct stat
/// would make the runtime the TCC requester and could park the control path
/// behind a consent prompt). Any broker failure, missing broker, truncated
/// listing, or permission error is reported as `git_root_vcs_unverifiable`,
/// never folded into `git_root_vcs_disappeared` (a real repo would otherwise
/// be misreported as stripped of its VCS marker).
pub fn reject_operational_root_mutation(
    snapshot: &Value,
    topology: &ProjectTopology,
    root: &Path,
) -> Option<Value> {
    if is_operational_root(topology, root) {
        return Some(json!({
            "ok": false,
            "reason": "operational_root_mutation_unsupported",
            "root": root.to_string_lossy(),
            "hint": "this vcs-less operational root has no Git dirty state to confirm against; write inside a Git-backed project root, or restrict this change to reading it",
        }));
    }
    let probe = topology
        .projects
        .get(root)
        .and_then(root_kind_of)
        .and_then(|(git_root, kind)| {
            (kind == RootKind::Git).then(|| git_marker_probe(snapshot, &git_root))
        });
    match probe {
        Some(GitMarkerProbe::Gone) => Some(json!({
            "ok": false,
            "reason": "git_root_vcs_disappeared",
            "root": root.to_string_lossy(),
            "hint": "this root was Git-managed when the routing topology was derived but .git is no longer present; write confirmation cannot be trusted — retry after refreshing herdr_inspect",
        })),
        Some(GitMarkerProbe::Inconclusive) => Some(json!({
            "ok": false,
            "reason": "git_root_vcs_unverifiable",
            "root": root.to_string_lossy(),
            "hint": "the .git marker could not be verified (permission denied, consent prompt pending, or probe timeout); write confirmation cannot be trusted — retry after refreshing herdr_inspect",
        })),
        Some(GitMarkerProbe::Present) | None => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitMarkerProbe {
    Present,
    /// Marker definitively absent — the cached Git classification is stale.
    Gone,
    /// Probe could not decide (permission denied, consent prompt, I/O
    /// error). Must never be reported as "gone".
    Inconclusive,
}

fn git_marker_probe(snapshot: &Value, git_root: &Path) -> GitMarkerProbe {
    let marker = git_root.join(".git");
    if crate::macos_permissions::runtime_must_not_probe(&marker) {
        protected_git_marker_probe(snapshot, git_root)
    } else {
        match marker.try_exists() {
            Ok(true) => GitMarkerProbe::Present,
            Ok(false) => GitMarkerProbe::Gone,
            Err(_) => GitMarkerProbe::Inconclusive,
        }
    }
}

/// Ask the authorized stable broker to read the marker path itself and
/// decide presence from the `fs_read` result contract. `fs_list` cannot be
/// used here: its walker deliberately skips `.git` entries, so a listing can
/// never observe the marker. A plain clone's `.git` directory answers
/// `not_a_file`, a linked worktree's `.git` file answers `ok`, a missing
/// marker answers `stat_failed` carrying errno `ENOENT` (`os_error`), and
/// every other failure — permission, read error, broker unavailable, or an
/// older broker that does not emit `os_error` — is
/// [`GitMarkerProbe::Inconclusive`] so the write stays fail-closed without
/// the runtime becoming the TCC requester.
#[cfg(target_os = "macos")]
fn protected_git_marker_probe(snapshot: &Value, git_root: &Path) -> GitMarkerProbe {
    match crate::tcc_broker::fs_read_via_stable_broker(snapshot, &git_root.join(".git"), 64) {
        Ok(result) => git_marker_from_marker_read(&result),
        Err(_) => GitMarkerProbe::Inconclusive,
    }
}

#[cfg(not(target_os = "macos"))]
fn protected_git_marker_probe(_snapshot: &Value, _git_root: &Path) -> GitMarkerProbe {
    // Nothing is privacy-protected off macOS, so production never reaches
    // this arm; keep it fail-closed for tests that force the protected
    // classification.
    GitMarkerProbe::Inconclusive
}

/// Decide marker presence from a broker `fs_read` of `<root>/.git`. Covers
/// both `.git` spellings: a successful read is the linked-worktree file and
/// `not_a_file` is the plain-clone directory. Only `stat_failed` with errno
/// `ENOENT` proves the marker is gone — a missing `os_error` field (broker
/// older than the `io_error_to_fs_value` addition) or any other failure is
/// Inconclusive.
#[cfg(any(target_os = "macos", test))]
fn git_marker_from_marker_read(result: &Value) -> GitMarkerProbe {
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        return GitMarkerProbe::Present;
    }
    // Mirrors the real `fs_tools::read` failure contract: `not_found` is the
    // validate-existing refusal for a missing/canonicalize-unresolvable path
    // (the common missing-.git case), `stat_failed` covers a path that
    // vanished between validation and stat. `outside_managed_roots` only says
    // the root no longer admits the probe — the marker may or may not exist —
    // so it stays inconclusive rather than Gone.
    match result.get("reason").and_then(Value::as_str) {
        Some("not_a_file") => GitMarkerProbe::Present,
        Some("not_found") | Some("stat_failed") => {
            match result.get("os_error").and_then(Value::as_i64) {
                Some(code) if code == libc::ENOENT as i64 => GitMarkerProbe::Gone,
                _ => GitMarkerProbe::Inconclusive,
            }
        }
        _ => GitMarkerProbe::Inconclusive,
    }
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
    let guard = HomeGuard::new(home);
    let root = &project.root;
    if !root.is_absolute() || guard.rejects(root) || denied_secret_path(root) {
        return false;
    }
    // "canonical existing directory": the live cwd must resolve to a real
    // directory, and that real directory must itself survive the HOME-ancestry
    // and secret-path guards, so a symlinked cwd cannot escape them.
    // The rotating runtime is deliberately not the macOS TCC client:
    // Documents/Desktop/Downloads roots are classified before any in-process
    // canonicalize, and their existence/symlink resolution is verified by the
    // stable broker (`herdr_fs_*`) or the delegated utility pane
    // (`herdr_exec`) — the same route the pre-existing protected-root command
    // preflight already uses. Every other unresolvable path is refused.
    // `runtime_must_not_probe` already carries the full protected-path
    // contract (Documents/Desktop/Downloads, the /System/Volumes/Data firmlink
    // spelling, APFS case variants) *and* the broker-child exemption: inside
    // the stable TCC broker it returns false, so the authorized child still
    // runs the complete canonicalize + HomeGuard + secret-path validation
    // instead of inheriting the rotating runtime's delegation shortcut.
    let root_real = if crate::macos_permissions::runtime_must_not_probe(root) {
        None
    } else {
        match std::fs::canonicalize(root) {
            Ok(real) => {
                if !real.is_dir() || guard.rejects(&real) || denied_secret_path(&real) {
                    return false;
                }
                Some(real)
            }
            Err(_) => return false,
        }
    };
    // Exactly proven by live cwd: every cwd attached to this project resolves
    // to this root's real directory (or, when the real path is delegated to
    // TCC, must be this exact cwd). A sibling outside the group therefore
    // cannot borrow this root's authority. A delegated-root cwd is likewise
    // never probed in-process; only an exact match carries authority.
    project.cwds.iter().all(|cwd| {
        if crate::macos_permissions::runtime_must_not_probe(cwd) {
            cwd == root
        } else {
            match (&root_real, std::fs::canonicalize(cwd)) {
                (Some(root_real), Ok(cwd_real)) => &cwd_real == root_real,
                _ => cwd == root,
            }
        }
    })
}

/// Stable filesystem identity of an existing path: `(device, inode)` on Unix.
///
/// This is deliberately not a string comparison. On macOS the Data-volume
/// firmlink makes `/Users/<u>` and `/System/Volumes/Data/Users/<u>` the same
/// directory while `realpath(3)`/`canonicalize` happily returns the spelling it
/// was given, so only device+inode identifies them as one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

/// Identity of `path`, following symlinks. `None` when the path cannot be
/// stat'ed at all (missing, or TCC-blocked for the rotating runtime).
fn path_identity(path: &Path) -> Option<FileIdentity> {
    let metadata = std::fs::metadata(path).ok()?;
    file_identity(&metadata)
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn file_identity(_metadata: &std::fs::Metadata) -> Option<FileIdentity> {
    // No portable device+inode pair here; every caller also runs the lexical
    // and canonical-path guards, and a non-Unix path that cannot be resolved
    // is refused rather than allowed.
    None
}

/// Rejects HOME itself, any ancestor of HOME, and any alternate spelling of
/// either, before a vcs-less cwd may become an operational root.
///
/// Three independent layers, because each one alone is bypassable:
///
/// 1. lexical ancestry against both the given and firmlink-normalized spelling
///    (`/System/Volumes/Data/Users` is an ancestor of HOME even though no
///    lexical prefix check on `$HOME` sees it);
/// 2. stable identity (`device`+`inode`) against HOME and each of its
///    ancestors, which catches symlinked and firmlink aliases regardless of
///    spelling, and keeps working when `canonicalize` cannot run;
/// 3. the caller additionally re-runs this guard on the canonical real path.
struct HomeGuard {
    spellings: Vec<PathBuf>,
    identities: std::collections::BTreeSet<FileIdentity>,
}

impl HomeGuard {
    fn new(home: &Path) -> Self {
        let mut spellings = vec![home.to_path_buf()];
        if let Some(primary) = firmlink_primary_spelling(home) {
            spellings.push(primary);
        }
        let identities = home
            .ancestors()
            .filter_map(path_identity)
            .collect::<std::collections::BTreeSet<_>>();
        Self {
            spellings,
            identities,
        }
    }

    fn rejects(&self, candidate: &Path) -> bool {
        let mut candidates = vec![candidate.to_path_buf()];
        if let Some(primary) = firmlink_primary_spelling(candidate) {
            candidates.push(primary);
        }
        for candidate in &candidates {
            if self
                .spellings
                .iter()
                .any(|home| home == candidate || home.starts_with(candidate))
            {
                return true;
            }
        }
        path_identity(candidate).is_some_and(|identity| self.identities.contains(&identity))
    }
}

/// macOS firmlink alias for the Data volume. `/System/Volumes/Data/<rest>` is
/// the same object as `/<rest>` for firmlinked subtrees such as `/Users`.
const FIRMLINK_DATA_PREFIX: &str = "/System/Volumes/Data";

/// Map an alternate Data-volume spelling onto its primary spelling so the
/// lexical ancestor checks see through the firmlink. Returns `None` when the
/// path does not use that prefix.
fn firmlink_primary_spelling(candidate: &Path) -> Option<PathBuf> {
    let rest = candidate.strip_prefix(FIRMLINK_DATA_PREFIX).ok()?;
    Some(Path::new("/").join(rest))
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
    validate_existing_scoped(
        snapshot,
        input,
        &managed_roots(snapshot),
        default_worktrees_root().as_deref(),
        None,
    )
}

/// Shared entry point behind both the Git-only and the validated fs surface.
///
/// The target-scoped Herdr worktree special case (`~/.herdr/worktrees/<repo>
/// /<branch>` without a declared managed root) is part of both, so linked
/// worktree fs reads keep the pre-existing behavior.
fn validate_existing_scoped(
    snapshot: &Value,
    input: &str,
    roots: &[PathBuf],
    worktrees_root: Option<&Path>,
    topology: Option<&ProjectTopology>,
) -> Result<ManagedPath, Value> {
    if let Ok(resolved) = resolve_input(input)
        && let Some(root) = worktrees_root.and_then(|worktrees_root| {
            target_scoped_worktree_root_under(snapshot, &resolved, worktrees_root)
        })
    {
        return validate_existing_with_roots(&[root], input, None);
    }
    validate_existing_with_roots(roots, input, topology)
}

fn default_worktrees_root() -> Option<PathBuf> {
    let home = home_dir()?;
    Some(home.join(".herdr/worktrees"))
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
    validate_existing_validated_with_worktrees_root(snapshot, input, default_worktrees_root())
}

fn validate_existing_validated_with_worktrees_root(
    snapshot: &Value,
    input: &str,
    worktrees_root: Option<PathBuf>,
) -> Result<ManagedPath, Value> {
    let topology = projects::derive_routing_cached(snapshot);
    validate_existing_scoped(
        snapshot,
        input,
        &validated_roots_cached(snapshot),
        worktrees_root.as_deref(),
        Some(&topology),
    )
}

struct ValidatedRootsCacheEntry {
    expires_at: std::time::Instant,
    roots: Arc<Vec<PathBuf>>,
}

impl projects::TimedCacheEntry for ValidatedRootsCacheEntry {
    fn expires_at(&self) -> std::time::Instant {
        self.expires_at
    }
}

static VALIDATED_ROOTS_CACHE: std::sync::OnceLock<
    Mutex<std::collections::HashMap<projects::RoutingFingerprint, ValidatedRootsCacheEntry>>,
> = std::sync::OnceLock::new();

/// [`validated_roots_from`] memoized on the routing fingerprint for
/// [`projects::ROUTING_CACHE_TTL`]. Skips the per-call root/cwd
/// `canonicalize` and `HomeGuard` ancestor stats inside
/// `operational_root_eligible` during scroll churn. Filesystem changes the
/// fingerprint cannot see (`git init` inside an existing cwd, a renamed
/// repo, a swapped symlink) surface after the TTL. Per-request target-path
/// canonicalize, secret-path denial, and symlink-escape checks in
/// `validate_existing_scoped` still run live on every call.
///
/// The recomputation runs *outside* the cache mutex so a slow topology
/// derivation cannot serialize unrelated fingerprints behind the lock; a
/// racing refill wins when still fresh.
fn validated_roots_cached(snapshot: &Value) -> Arc<Vec<PathBuf>> {
    let fingerprint = projects::routing_fingerprint(snapshot);
    let cache = VALIDATED_ROOTS_CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    {
        let guard = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = guard.get(&fingerprint)
            && entry.expires_at > projects::cache_now()
        {
            return Arc::clone(&entry.roots);
        }
    }
    let roots = Arc::new(validated_roots_from(&projects::derive_routing_cached(
        snapshot,
    )));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let now = projects::cache_now();
    if let Some(entry) = guard.get(&fingerprint)
        && entry.expires_at > now
    {
        return Arc::clone(&entry.roots);
    }
    // Deadlines are stored on the real clock so entries stay comparable
    // across threads (see `projects::cache_now`).
    projects::routing_cache_insert_bounded(
        &mut guard,
        fingerprint,
        ValidatedRootsCacheEntry {
            expires_at: std::time::Instant::now() + projects::ROUTING_CACHE_TTL,
            roots: Arc::clone(&roots),
        },
    );
    roots
}

/// See [`validate_existing_validated`]. Reuses one routing topology per
/// request so the root identity is resolved once.
pub fn validate_existing_validated_with_topology(
    topology: &ProjectTopology,
    input: &str,
) -> Result<ManagedPath, Value> {
    validate_existing_with_roots(&validated_roots_from(topology), input, Some(topology))
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
    validate_existing_with_roots(&[project_root.to_path_buf()], input, None)
}

/// Live re-verification for a matched Operational root.
///
/// The roots list may come from the routing-fingerprint cache, so an
/// Operational root's eligibility was proven at fill time only. A lexical
/// root swapped for a symlink inside the TTL would still satisfy the
/// containment check below (both root and target resolve into the new
/// location) while silently widening into HOME or secret material, because
/// [`operational_root_eligible`] would never re-run. Authorization therefore
/// re-runs the live identity and HOME/secret boundary instead of trusting
/// the cached classification: for a vcs-less project, [`root_kind_of`]
/// re-evaluates the eligibility rules live (HOME ancestry, secret path,
/// canonical directory identity, cwd proof) and only yields `Operational`
/// when the root still is the directory the snapshot proved. Git roots keep
/// the pre-existing behavior: their write boundary re-verifies `.git`
/// separately.
fn operational_root_still_eligible(topology: Option<&ProjectTopology>, root: &Path) -> bool {
    let Some(project) = topology.and_then(|topology| topology.projects.get(root)) else {
        return true;
    };
    if project.vcs.is_some() {
        return true;
    }
    matches!(root_kind_of(project), Some((_, RootKind::Operational)))
}

fn operational_root_no_longer_eligible_error(root: &Path) -> Value {
    json!({
        "ok": false,
        "reason": "operational_root_no_longer_eligible",
        "root": root.to_string_lossy(),
        "hint": "the operational root's live identity changed since the routing snapshot was taken (swapped, removed, or it now escapes the HOME boundary); retry after refreshing herdr_inspect",
    })
}

fn validate_existing_with_roots(
    roots: &[PathBuf],
    input: &str,
    topology: Option<&ProjectTopology>,
) -> Result<ManagedPath, Value> {
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
    if !operational_root_still_eligible(topology, &root) {
        return Err(operational_root_no_longer_eligible_error(&root));
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
    validate_target_with_roots(&validated_roots_from(topology), input, Some(topology))
}

/// Validate a writable target against one project root that was already
/// validated as managed by the caller.
pub fn validate_target_in_root(project_root: &Path, input: &str) -> Result<ManagedPath, Value> {
    validate_target_with_roots(&[project_root.to_path_buf()], input, None)
}

fn validate_target_with_roots(
    roots: &[PathBuf],
    input: &str,
    topology: Option<&ProjectTopology>,
) -> Result<ManagedPath, Value> {
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
    if !operational_root_still_eligible(topology, &root) {
        return Err(operational_root_no_longer_eligible_error(&root));
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

/// Directory names whose entire subtree is credential material. A match on any
/// single component denies the path, so nesting, `..` spelling, symlink
/// resolution and case variants are all covered by the callers that re-run this
/// on the canonical real path.
const DENIED_SECRET_DIRECTORIES: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".docker",
    ".azure",
    ".password-store",
];

/// Credential directories that only exist below `~/.config`.
const DENIED_CONFIG_DIRECTORIES: &[&str] = &["gcloud", "gh"];

pub fn denied_secret_path(path: &Path) -> bool {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    if lower.ends_with("/.git/config") {
        return true;
    }

    let segments = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => {
                Some(value.to_string_lossy().to_ascii_lowercase())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if segments
        .iter()
        .any(|segment| denied_secret_segment(segment))
    {
        return true;
    }
    segments
        .windows(2)
        .any(|pair| pair[0] == ".config" && DENIED_CONFIG_DIRECTORIES.contains(&pair[1].as_str()))
}

fn denied_secret_segment(segment: &str) -> bool {
    if segment == ".git" {
        return false;
    }
    DENIED_SECRET_DIRECTORIES.contains(&segment)
        || segment == ".env"
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
}

/// Extended credential-path guard for HOME-wide text transfer and herdr-core
/// file operations. This intentionally does not alter the shared project guard.
pub(crate) fn denied_home_credential_path(path: &Path) -> bool {
    // File-shaped credentials the text transfer surface additionally refuses.
    // The credential *directory* set is shared with `denied_secret_path` so the
    // file tools and the text transfer surface cannot drift apart (#938).
    const EXTRA_FILE_SEGMENTS: &[&str] = &[
        ".netrc",
        ".npmrc",
        ".pypirc",
        ".git-credentials",
        ".htpasswd",
    ];
    denied_secret_path(path)
        || path.components().any(|component| {
            let segment = component.as_os_str().to_string_lossy().to_ascii_lowercase();
            EXTRA_FILE_SEGMENTS.contains(&segment.as_str())
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

    /// A `ProjectInfo` for a vcs-less cwd, for guard-level unit tests that need
    /// an injected HOME instead of the host's real one.
    fn vcs_less_project(root: &Path) -> crate::projects::ProjectInfo {
        crate::projects::ProjectInfo {
            root: root.to_path_buf(),
            vcs: None,
            managed: false,
            dirty: false,
            changed_files: 0,
            git_status_observed: false,
            git_status_source: None,
            pane_ids: vec!["w1:p1".to_owned()],
            cwds: vec![root.to_path_buf()],
        }
    }

    #[test]
    fn fs_validated_path_keeps_the_herdr_worktree_special_case() {
        let worktrees_root = plain_parent("worktree-special", &[]);
        let root = worktrees_root.join("repo").join("feature");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join(".git"),
            "gitdir: /elsewhere/repo/.git/worktrees/feature\n",
        )
        .unwrap();
        let file = root.join("src/lib.rs");
        fs::write(&file, "fn main() {}\n").unwrap();

        // A linked worktree with no declared managed root is proven only by
        // the live cwd / checked-out workspace.
        let idle = json!({"panes": [], "agents": []});
        let live = json!({
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}],
            "agents": []
        });
        let declared = json!({
            "workspaces": [{"workspace_id": "w1", "worktree": {"checkout_path": root}}],
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": "/tmp/elsewhere"}],
            "agents": []
        });
        let path = file.to_str().unwrap();

        for snapshot in [&idle, &live, &declared] {
            let expected = if snapshot == &idle {
                "outside_managed_roots"
            } else {
                ""
            };
            if expected.is_empty() {
                let validated = validate_existing_validated_with_worktrees_root(
                    snapshot,
                    path,
                    Some(worktrees_root.clone()),
                )
                .expect("live-proven worktree stays readable through the fs path");
                assert_eq!(validated.root, root);
                // The Git-only entry point keeps the same special case.
                let git_only = validate_existing_scoped(
                    snapshot,
                    path,
                    &[],
                    Some(worktrees_root.as_path()),
                    None,
                )
                .expect("git-only entry keeps the worktree special case");
                assert_eq!(git_only.root, root);
            } else {
                assert_eq!(
                    validate_existing_validated_with_worktrees_root(
                        snapshot,
                        path,
                        Some(worktrees_root.clone())
                    )
                    .unwrap_err()["reason"],
                    expected
                );
            }
        }
        // Without the worktree special case the declared-only root really is
        // unmanaged (no pane cwd inside it), so the positive assertion above is
        // not vacuous for that snapshot.
        let declared_topology = projects::derive_routing(&declared);
        assert!(!validated_roots_from(&declared_topology).contains(&root));
        assert!(!managed_roots_from(&declared_topology).contains(&root));
        fs::remove_dir_all(worktrees_root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn delegated_protected_root_uses_metadata_when_canonicalize_is_unavailable() {
        use std::os::unix::fs::PermissionsExt;

        let parent = plain_parent("delegated", &["elsewhere"]);
        let home = parent.join("home");
        let documents = home.join("Documents");
        let project = documents.join("feishu");
        let outside = parent.join("elsewhere/project");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(project.join("notes.md"), "artifact\n").unwrap();

        // Real canonicalization failure for a directory that exists: the
        // process cannot traverse the protected parent.
        fs::set_permissions(&documents, fs::Permissions::from_mode(0o000)).unwrap();
        fs::set_permissions(outside.parent().unwrap(), fs::Permissions::from_mode(0o000)).unwrap();
        let privileged_runner = std::fs::canonicalize(&project).is_ok();
        if !privileged_runner {
            assert!(std::fs::canonicalize(&project).is_err());
            assert!(std::fs::metadata(&project).is_err());
            assert!(
                operational_root_eligible(&vcs_less_project(&project), Some(&home)),
                "an exact protected operational cwd is proven from live metadata alone"
            );
            // The delegation is scoped to the protected folders: the identical
            // canonicalize failure outside them is still refused instead of
            // being treated as an allow.
            assert!(!operational_root_eligible(
                &vcs_less_project(&outside),
                Some(&home)
            ));
            // The guard layers still apply while canonicalization is down:
            // HOME itself is refused from metadata alone.
            assert!(!operational_root_eligible(
                &vcs_less_project(&home),
                Some(&home)
            ));
        }

        fs::set_permissions(&documents, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(outside.parent().unwrap(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            !privileged_runner,
            "a privileged runner cannot express permission-denied; skipping"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn firmlink_and_alternate_home_spellings_are_rejected() {
        // Pure lexical layer: deterministic on every platform. On macOS the
        // Data-volume firmlink makes these the same directory as `/Users/...`
        // while canonicalize does not collapse the spelling.
        for candidate in [
            "/Users/example",
            "/Users",
            "/",
            "/System/Volumes/Data/Users/example",
            "/System/Volumes/Data/Users",
            "/System/Volumes/Data",
        ] {
            let guard = HomeGuard::new(Path::new("/Users/example"));
            assert!(
                guard.rejects(Path::new(candidate)),
                "{candidate} must never be an operational root"
            );
        }
        // A descendant of HOME keeps working under either spelling.
        for candidate in [
            "/Users/example/Documents/feishu",
            "/System/Volumes/Data/Users/example/Documents/feishu",
        ] {
            let guard = HomeGuard::new(Path::new("/Users/example"));
            assert!(!guard.rejects(Path::new(candidate)), "{candidate}");
        }
        assert_eq!(
            firmlink_primary_spelling(Path::new("/System/Volumes/Data/Users")),
            Some(PathBuf::from("/Users"))
        );
        assert_eq!(firmlink_primary_spelling(Path::new("/Users")), None);
    }

    #[cfg(unix)]
    #[test]
    fn alternate_home_spelling_by_identity_is_rejected() {
        use std::os::unix::fs::symlink;
        let parent = plain_parent("identity", &["home", "other"]);
        let home = parent.join("home");
        let alias = parent.join("home-alias");
        symlink(&home, &alias).unwrap();

        // Same object, different spelling: only device+inode sees it.
        assert_eq!(path_identity(&home), path_identity(&alias));
        assert_ne!(path_identity(&home), path_identity(&parent));
        let guard = HomeGuard::new(&home);
        assert!(guard.rejects(&alias));
        // The lexical layer still catches the ancestor, and an unrelated
        // sibling stays available.
        assert!(guard.rejects(&parent));
        assert!(!guard.rejects(&parent.join("other")));

        // A guarded cwd never becomes an operational root, while a real
        // descendant and an unrelated sibling still do.
        assert!(!operational_root_eligible(
            &vcs_less_project(&alias),
            Some(&home)
        ));
        let child = home.join("child");
        fs::create_dir_all(&child).unwrap();
        assert!(operational_root_eligible(
            &vcs_less_project(&child),
            Some(&home)
        ));
        fs::remove_dir_all(parent).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn real_firmlink_home_ancestors_are_rejected() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set on this host"));
        let Some(name) = home.file_name() else {
            return;
        };
        let firmlink_home = PathBuf::from("/System/Volumes/Data/Users").join(name);
        if path_identity(&firmlink_home).is_none() {
            // This host has no Data-volume spelling for HOME; the portable
            // identity and lexical regressions still cover the guard.
            return;
        }
        assert_eq!(
            path_identity(&home),
            path_identity(&firmlink_home),
            "the Data-volume spelling must be the same object as HOME"
        );
        let guard = HomeGuard::new(&home);
        for candidate in [
            firmlink_home.clone(),
            PathBuf::from("/System/Volumes/Data/Users"),
            PathBuf::from("/System/Volumes/Data"),
        ] {
            assert!(guard.rejects(&candidate), "{}", candidate.display());
        }
        // The same spelling never becomes an operational root through the
        // snapshot either.
        let snap = snapshot(&firmlink_home);
        assert_eq!(
            validate_existing_validated(&snap, firmlink_home.to_str().unwrap()).unwrap_err()["reason"],
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
        assert!(reject_operational_root_mutation(&snap, &topology, &root).is_none());
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
        let denied = reject_operational_root_mutation(&snap, &topology, &root)
            .expect("operational roots must fail closed for Git-dirty gates");
        assert_eq!(denied["ok"], false);
        assert_eq!(denied["reason"], "operational_root_mutation_unsupported");
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn git_root_whose_vcs_marker_vanished_fails_closed_at_the_write_boundary() {
        let root = repo();
        let snap = snapshot(&root);
        let topology = projects::derive_routing(&snap);
        assert!(reject_operational_root_mutation(&snap, &topology, &root).is_none());

        // Simulates `.git` being removed/renamed inside the routing-cache
        // TTL: the topology still says Git, but the write boundary must not
        // trust the stale classification.
        fs::rename(root.join(".git"), root.join(".git-moved")).unwrap();
        let denied = reject_operational_root_mutation(&snap, &topology, &root)
            .expect("a Git root whose .git disappeared must fail closed");
        assert_eq!(denied["ok"], false);
        assert_eq!(denied["reason"], "git_root_vcs_disappeared");

        // A linked-worktree style .git file also counts as a VCS marker.
        fs::write(root.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert!(reject_operational_root_mutation(&snap, &topology, &root).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn git_marker_probe_failure_is_reported_as_unverifiable_not_disappeared() {
        use std::os::unix::fs::PermissionsExt;
        let root = repo();
        let snap = snapshot(&root);
        let topology = projects::derive_routing(&snap);
        assert!(reject_operational_root_mutation(&snap, &topology, &root).is_none());

        // stat(<root>/.git) fails with EACCES when the root loses search
        // permission — a probe failure, not a missing marker.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
        let denied = reject_operational_root_mutation(&snap, &topology, &root)
            .expect("an unverifiable .git probe must still fail closed");
        assert_eq!(denied["ok"], false);
        assert_eq!(denied["reason"], "git_root_vcs_unverifiable");

        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    /// Given a broker fs_read answer for a protected root's `.git` marker
    /// When the write boundary decides marker presence
    /// Then both spellings are "present" (worktree file `ok`, clone
    /// directory `not_a_file`), only `stat_failed` carrying errno ENOENT is
    /// "gone", and every other failure — including an older broker that
    /// does not emit `os_error` — is inconclusive so a real repo is never
    /// misreported as stripped of its VCS marker.
    #[test]
    fn git_marker_from_marker_read_covers_both_spellings_and_failures() {
        // Linked-worktree marker: `.git` is a file, readable.
        assert_eq!(
            git_marker_from_marker_read(&json!({"ok": true, "content": "gitdir: /x/.git/wt"})),
            GitMarkerProbe::Present
        );
        // Plain-clone marker: `.git` is a directory.
        assert_eq!(
            git_marker_from_marker_read(&json!({"ok": false, "reason": "not_a_file"})),
            GitMarkerProbe::Present
        );
        // Marker removed: real `fs_tools::read` reports `not_found` (the
        // validate-existing refusal) or `stat_failed` when the entry vanished
        // between validation and stat; both carry the structured errno.
        for gone in [
            json!({"ok": false, "reason": "not_found", "os_error": 2}),
            json!({"ok": false, "reason": "stat_failed", "os_error": 2}),
        ] {
            assert_eq!(
                git_marker_from_marker_read(&gone),
                GitMarkerProbe::Gone,
                "{gone}"
            );
        }
        // Failures without the structured ENOENT (older broker, different
        // errno, or a refusal that cannot prove removal) are not proof the
        // marker is gone.
        for failure in [
            json!({"ok": false, "reason": "not_found"}),
            json!({"ok": false, "reason": "not_found", "os_error": 13}),
            json!({"ok": false, "reason": "stat_failed"}),
            json!({"ok": false, "reason": "stat_failed", "os_error": 5}),
            json!({"ok": false, "reason": "outside_managed_roots"}),
            json!({"ok": false, "reason": "read_failed", "os_error": 2}),
            json!({"ok": false, "reason": "macos_tcc_access_blocked"}),
            json!({}),
        ] {
            assert_eq!(
                git_marker_from_marker_read(&failure),
                GitMarkerProbe::Inconclusive,
                "{failure}"
            );
        }
    }

    /// Given a cached Operational root
    /// When its lexical path is swapped for a symlink inside the cache TTL
    /// Then authorization re-runs the live identity and HOME boundary instead
    /// of trusting the stale classification, so the swap cannot widen reads
    /// into an ancestor of HOME.
    #[cfg(unix)]
    #[test]
    fn swapped_operational_root_is_reverified_not_trusted_from_cache() {
        use std::os::unix::fs::symlink;
        let parent = plain_parent("rootswap", &["project"]);
        let root = parent.join("project");
        let file = root.join("artifact.md");
        fs::write(&file, "lark-cli output\n").unwrap();
        let snap = snapshot(&root);

        // First access proves and caches the operational root.
        let topology = projects::derive_routing(&snap);
        assert!(is_operational_root(&topology, &root));
        assert!(validate_existing_validated(&snap, file.to_str().unwrap()).is_ok());

        // Replace the directory with a symlink to an ancestor of HOME. Both
        // the cached root and any target under it now resolve below `/`, so
        // containment alone would still pass — the live eligibility re-check
        // is what must fail closed.
        fs::remove_file(&file).unwrap();
        fs::remove_dir(&root).unwrap();
        symlink("/", &root).unwrap();
        let denied =
            validate_existing_validated(&snap, root.join("etc").to_str().unwrap()).unwrap_err();
        assert_eq!(denied["reason"], "operational_root_no_longer_eligible");

        // Restore the real directory: the same path authorizes again.
        fs::remove_file(&root).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(&file, "lark-cli output\n").unwrap();
        assert!(validate_existing_validated(&snap, file.to_str().unwrap()).is_ok());
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
    fn credential_directories_are_denied_for_every_caller_spelling() {
        // Credential *directories*: every descendant is off limits for the file
        // tools and the text transfer surface alike, whether it appears in HOME,
        // nested inside an allowed root, or after `..` spelling. Case variants
        // are denied lexically before any filesystem lookup, so the check is
        // deterministic on case-insensitive and case-sensitive hosts.
        for path in [
            "/Users/example/.ssh",
            "/Users/example/.ssh/config",
            "/Users/example/.ssh/known_hosts",
            "/Users/example/.aws/config",
            "/Users/example/.aws/credentials",
            "/Users/example/.gnupg/trustdb.gpg",
            "/Users/example/.kube/config",
            "/Users/example/.docker/config.json",
            "/Users/example/.azure/msal_token_cache.json",
            "/Users/example/.password-store/work/mail.gpg",
            "/Users/example/.config/gcloud/application_default_credentials.json",
            "/Users/example/.config/gh/hosts.yml",
            "/home/example/project/.ssh/config",
            "/Users/example/project/data/../.aws/config",
            "/Users/EXAMPLE/.SSH/config",
            "/Users/example/.Config/GH/hosts.yml",
        ] {
            assert!(denied_secret_path(Path::new(path)), "{path}");
            assert!(denied_home_credential_path(Path::new(path)), "{path}");
        }
        for path in [
            "/Users/example/project/src/config.ts",
            "/Users/example/.ssh_config_notes.md",
            "/Users/example/work/.awsome-report.md",
            "/Users/example/.config/herdr-mcp/config.json",
            "/Users/example/.config/herdr/config.json",
        ] {
            assert!(!denied_secret_path(Path::new(path)), "{path}");
            assert!(!denied_home_credential_path(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn empty_managed_roots_hint_is_actionable_without_a_coding_agent() {
        // When the live snapshot exposes no managed Git roots, the
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

    #[test]
    fn validated_roots_cached_reuses_roots_across_scroll_churn_and_misses_on_cwd_change() {
        let parent = plain_parent("cached-roots", &["project"]);
        let root = parent.join("project");
        let snap = snapshot(&root);
        let mut churned = snap.clone();
        churned["panes"][0]["scrollback_offset"] = json!(42_000);

        let first = validated_roots_cached(&snap);
        let second = validated_roots_cached(&churned);
        assert!(
            Arc::ptr_eq(&first, &second),
            "scroll churn must reuse the cached roots"
        );

        let other = plain_parent("cached-roots-moved", &["project"]);
        let moved = snapshot(&other.join("project"));
        let third = validated_roots_cached(&moved);
        assert!(
            !Arc::ptr_eq(&first, &third),
            "a cwd change must recompute roots"
        );

        fs::remove_dir_all(parent).unwrap();
        fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn validated_roots_cached_recomputes_after_ttl_expiry() {
        let parent = plain_parent("cached-roots-ttl", &["project"]);
        let snap = snapshot(&parent.join("project"));

        let first = validated_roots_cached(&snap);
        projects::advance_cache_clock(
            projects::ROUTING_CACHE_TTL + std::time::Duration::from_millis(1),
        );
        let second = validated_roots_cached(&snap);
        assert!(
            !Arc::ptr_eq(&first, &second),
            "an expired entry must recompute so fingerprint-invisible fs changes surface"
        );

        fs::remove_dir_all(parent).unwrap();
    }
}
