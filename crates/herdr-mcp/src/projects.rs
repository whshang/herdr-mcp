use crate::child_process;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const GIT_STATUS_TIMEOUT: Duration = Duration::from_millis(750);
const GIT_OUTPUT_LIMIT: usize = 1024 * 1024;
const GIT_STATUS_CACHE_TTL: Duration = Duration::from_millis(250);
/// Upper bound a waiter spends on another thread's probe before treating the
/// probe as dead and becoming the next prober itself. Keeps a panicked or
/// wedged probe from stranding callers on the same root.
#[cfg(test)]
const GIT_STATUS_WAIT_TIMEOUT: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const GIT_STATUS_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(test)]
static GIT_STATUS_CLOCK_OFFSET_MS: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);

/// The cache's clock. Tests shift it forward instead of sleeping so TTL
/// expiry is deterministic and cannot flake on a busy runner.
fn cache_now() -> Instant {
    #[cfg(test)]
    {
        Instant::now()
            + Duration::from_millis(
                GIT_STATUS_CLOCK_OFFSET_MS.load(std::sync::atomic::Ordering::Relaxed) as u64,
            )
    }
    #[cfg(not(test))]
    {
        Instant::now()
    }
}

#[cfg(test)]
pub(crate) fn advance_git_status_clock(by: Duration) {
    GIT_STATUS_CLOCK_OFFSET_MS
        .fetch_add(by.as_millis() as i64, std::sync::atomic::Ordering::Relaxed);
}

struct GitStatusCacheEntry {
    expires_at: Instant,
    value: Option<GitStatus>,
    /// Canonical repository identity captured while this entry was probed.
    /// Alias cache keys for one repo share it so a later write through another
    /// symlink can drop every alias without a fresh protected-path stat.
    repo_identity: Option<PathBuf>,
}

/// A probe currently running for one root. `invalidated` is set when a write
/// under that root lands while the probe is in flight, so the stale result is
/// never published to the cache.
struct GitStatusInFlight {
    done: Mutex<bool>,
    ready: Condvar,
    invalidated: std::sync::atomic::AtomicBool,
    /// Set before the probe runs when a safe canonical identity is already
    /// known, so an invalidation during the probe can still see this alias.
    repo_identity: Mutex<Option<PathBuf>>,
}

#[cfg(test)]
static CANONICAL_IDENTITY_DELAY: std::sync::Mutex<Option<(PathBuf, u64)>> =
    std::sync::Mutex::new(None);

/// Test-only pause inside the unlocked canonical lookup for one path. Other
/// repositories keep the normal lookup so a slow test cannot expire their
/// 250ms cache entries.
#[cfg(test)]
pub(crate) fn set_canonical_identity_delay_for_test(path: &Path, delay: Duration) {
    *CANONICAL_IDENTITY_DELAY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((
        path.to_path_buf(),
        u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
    ));
}

/// Prober-side cleanup: dropping removes the in-flight marker and wakes all
/// waiters, including on probe panic, so a dead probe never strands callers.
struct GitStatusProbeGuard {
    key: PathBuf,
    waiter: Arc<GitStatusInFlight>,
}

impl Drop for GitStatusProbeGuard {
    fn drop(&mut self) {
        let Some(cache) = GIT_STATUS_CACHE.get() else {
            return;
        };
        let mut state = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A wedged probe may already have been replaced by a new one; only
        // remove the marker this guard owns.
        if state
            .in_flight
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.waiter))
        {
            state.in_flight.remove(&self.key);
        }
        drop(state);
        *self
            .waiter
            .done
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        self.waiter.ready.notify_all();
    }
}

#[derive(Default)]
struct GitStatusCacheState {
    entries: HashMap<PathBuf, GitStatusCacheEntry>,
    in_flight: HashMap<PathBuf, Arc<GitStatusInFlight>>,
    /// Monotonic count of invalidations. A probe that starts without a
    /// repository identity records this value and refuses to publish if any
    /// later invalidation could name its repository.
    invalidation_generation: u64,
    /// Bounded record of invalidations that happened while a protected probe
    /// still did not know its repository identity.
    recent_invalidations: Vec<RecentInvalidation>,
}

#[derive(Clone)]
struct RecentInvalidation {
    generation: u64,
    path: PathBuf,
    identities: Vec<PathBuf>,
}

const RECENT_INVALIDATION_LIMIT: u64 = 64;

static GIT_STATUS_CACHE: OnceLock<Mutex<GitStatusCacheState>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static DERIVE_ROUTING_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
static GIT_STATUS_PROBES: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn reset_git_status_probe_count(root: &Path) {
    if let Some(probes) = GIT_STATUS_PROBES.get() {
        probes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(root);
    }
}

#[cfg(test)]
pub(crate) fn git_status_probe_count(root: &Path) -> usize {
    GIT_STATUS_PROBES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(root)
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
fn git_status_cache_has(root: &Path) -> bool {
    GIT_STATUS_CACHE
        .get_or_init(|| Mutex::new(GitStatusCacheState::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entries
        .contains_key(root)
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProjectInfo {
    pub root: PathBuf,
    pub vcs: Option<&'static str>,
    pub managed: bool,
    pub dirty: bool,
    pub changed_files: usize,
    pub git_status_observed: bool,
    pub git_status_source: Option<&'static str>,
    pub pane_ids: Vec<String>,
    pub cwds: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectTopology {
    pub projects: BTreeMap<PathBuf, ProjectInfo>,
    pub pane_to_workspace: HashMap<String, String>,
}

pub fn derive(snapshot: &Value) -> ProjectTopology {
    derive_inner(snapshot, true)
}

/// Derive project routing without running `git status` for dirty enrichment.
///
/// Security gates and busy-agent routing only need the managed Git root and
/// pane/workspace ownership. Keeping that path separate avoids paying for a
/// full repository status scan on every file read, write, grep, or exec gate.
///
/// Callers that need both managed-root validation and busy-agent checks in
/// the same request should derive once and pass the topology through
/// `fs_security` / `mutation` helpers so the same snapshot identity is not
/// recomputed.
pub fn derive_routing(snapshot: &Value) -> ProjectTopology {
    #[cfg(test)]
    DERIVE_ROUTING_CALLS.with(|count| count.set(count.get() + 1));
    derive_inner(snapshot, false)
}

#[cfg(test)]
pub(crate) fn derive_routing_call_count() -> usize {
    DERIVE_ROUTING_CALLS.with(|count| count.get())
}

#[cfg(test)]
pub(crate) fn reset_derive_routing_call_count() {
    DERIVE_ROUTING_CALLS.with(|count| count.set(0));
}

fn derive_inner(snapshot: &Value, include_git_status: bool) -> ProjectTopology {
    let pane_to_workspace = pane_workspaces(snapshot);
    let pane_cwds = pane_cwds(snapshot);
    let declared_roots = declared_workspace_roots(snapshot);
    let home = home_dir();

    let mut grouped =
        BTreeMap::<PathBuf, (Option<&'static str>, BTreeSet<String>, BTreeSet<PathBuf>)>::new();
    for (pane_id, cwd) in pane_cwds {
        let git_root = declared_git_root(&pane_id, &cwd, &pane_to_workspace, &declared_roots)
            .or_else(|| git_toplevel_tcc_safe(snapshot, &cwd));
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

    let statuses = if include_git_status {
        let status_roots = grouped
            .iter()
            .filter_map(|(root, (vcs, _, _))| {
                let managed = vcs.is_some() && !is_unmanaged_root(root, home.as_deref());
                managed.then_some(root.clone())
            })
            .collect::<Vec<_>>();
        git_statuses(snapshot, &status_roots)
    } else {
        HashMap::new()
    };

    let projects = grouped
        .into_iter()
        .map(|(root, (vcs, pane_ids, cwds))| {
            let managed = vcs.is_some() && !is_unmanaged_root(&root, home.as_deref());
            let status = statuses.get(&root);
            let (dirty, changed_files) = status
                .map(|status| (status.dirty, status.changed_files))
                .unwrap_or((false, 0));
            let info = ProjectInfo {
                root: root.clone(),
                vcs,
                managed,
                dirty,
                changed_files,
                git_status_observed: status.is_some(),
                git_status_source: status.map(|status| status.source),
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

fn declared_workspace_roots(snapshot: &Value) -> HashMap<String, PathBuf> {
    array(snapshot, "workspaces")
        .iter()
        .filter_map(|workspace| {
            let workspace_id = workspace.get("workspace_id")?.as_str()?;
            let worktree = workspace.get("worktree")?.as_object()?;
            let root = worktree
                .get("checkout_path")
                .and_then(Value::as_str)
                .or_else(|| worktree.get("path").and_then(Value::as_str))?;
            let root = PathBuf::from(root);
            root.is_absolute().then(|| (workspace_id.to_owned(), root))
        })
        .collect()
}

fn declared_git_root(
    pane_id: &str,
    cwd: &Path,
    pane_to_workspace: &HashMap<String, String>,
    declared_roots: &HashMap<String, PathBuf>,
) -> Option<PathBuf> {
    if let Some(root) = pane_to_workspace
        .get(pane_id)
        .and_then(|workspace| declared_roots.get(workspace))
        .filter(|root| cwd.starts_with(root))
    {
        return Some(root.clone());
    }

    declared_roots
        .values()
        .filter(|root| cwd.starts_with(root))
        .max_by_key(|root| root.components().count())
        .cloned()
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

pub fn projects_for_workspace(topology: &ProjectTopology, workspace_id: &str) -> Vec<ProjectInfo> {
    let mut projects = topology
        .projects
        .values()
        .filter(|project| {
            project.pane_ids.iter().any(|pane| {
                topology
                    .pane_to_workspace
                    .get(pane)
                    .is_some_and(|workspace| workspace == workspace_id)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    projects.sort_by(|left, right| left.root.cmp(&right.root));
    projects
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
        // Never probe a macOS privacy-protected folder from the rotating
        // runtime: the first `.git` lookup inside Documents/Desktop/Downloads
        // can block this thread on a consent prompt. Callers that need a
        // protected root resolve it through the stable broker instead.
        if crate::macos_permissions::runtime_must_not_probe(&current) {
            return None;
        }
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }

    None
}

fn git_toplevel_tcc_safe(snapshot: &Value, cwd: &Path) -> Option<PathBuf> {
    if should_use_broker_for_toplevel(snapshot, cwd) {
        return cached_git_status(snapshot, cwd).map(|status| status.root);
    }
    git_toplevel(cwd)
}

fn should_use_broker_for_toplevel(snapshot: &Value, cwd: &Path) -> bool {
    !is_herdr_managed_worktree(cwd) && should_use_stable_broker(snapshot, cwd)
}

#[derive(Debug, Clone)]
struct GitStatus {
    /// Repository root used for routing and display. A subdirectory cwd stays
    /// attached to this root instead of becoming a new project.
    root: PathBuf,
    dirty: bool,
    changed_files: usize,
    source: &'static str,
    /// Canonical repository identity used only to match aliases. `None` means
    /// the probe has not proven one yet.
    repo_identity: Option<PathBuf>,
}

fn git_statuses(snapshot: &Value, roots: &[PathBuf]) -> HashMap<PathBuf, GitStatus> {
    thread::scope(|scope| {
        let jobs = roots
            .iter()
            .map(|root| {
                let root = root.clone();
                scope.spawn(move || {
                    let status = cached_git_status(snapshot, &root);
                    (root, status)
                })
            })
            .collect::<Vec<_>>();

        jobs.into_iter()
            .filter_map(|job| job.join().ok())
            .filter_map(|(root, status)| status.map(|status| (root, status)))
            .collect()
    })
}

fn cached_git_status(snapshot: &Value, root: &Path) -> Option<GitStatus> {
    let key = root.to_path_buf();
    let cache = GIT_STATUS_CACHE.get_or_init(|| Mutex::new(GitStatusCacheState::default()));
    loop {
        let mut state = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = state.entries.get(&key)
            && entry.expires_at > cache_now()
        {
            return entry.value.clone();
        }
        if let Some(waiter) = state.in_flight.get(&key).cloned() {
            drop(state);
            let mut done = waiter
                .done
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while !*done {
                let wait = waiter
                    .ready
                    .wait_timeout(done, GIT_STATUS_WAIT_TIMEOUT)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                done = wait.0;
                if !(wait.1.timed_out() && !*done) {
                    continue;
                }
                // The probe is wedged past the bound: drop its marker so the
                // next waiter becomes the prober instead of blocking forever.
                drop(done);
                let mut state = cache
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if state
                    .in_flight
                    .get(&key)
                    .is_some_and(|current| Arc::ptr_eq(current, &waiter))
                {
                    state.in_flight.remove(&key);
                    drop(state);
                    *waiter
                        .done
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
                    waiter.ready.notify_all();
                } else {
                    drop(state);
                }
                break;
            }
            continue;
        }
        let started_generation = state.invalidation_generation;
        let waiter = Arc::new(GitStatusInFlight {
            done: Mutex::new(false),
            ready: Condvar::new(),
            invalidated: std::sync::atomic::AtomicBool::new(false),
            repo_identity: Mutex::new(None),
        });
        state.in_flight.insert(key.clone(), waiter.clone());
        drop(state);

        // `guard` drops after the publish below (or on probe panic), removing
        // the marker and waking waiters.
        let guard = GitStatusProbeGuard {
            key: key.clone(),
            waiter: waiter.clone(),
        };
        // Record a canonical identity only for a path this runtime is already
        // allowed to inspect. Protected and broker-routed roots stay unresolved
        // here; the broker result supplies their identity after the probe.
        let mut repo_identity = safe_canonical_repo_identity(root);
        let started_without_identity = repo_identity.is_none();
        if let Some(identity) = &repo_identity {
            *waiter
                .repo_identity
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(identity.clone());
        }
        let value = git_status_uncached(snapshot, root);
        if let Some(status) = value.as_ref()
            && let Some(identity) = &status.repo_identity
        {
            repo_identity = Some(identity.clone());
            *waiter
                .repo_identity
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(identity.clone());
        }
        let known_identity = {
            let guard = waiter
                .repo_identity
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.clone()
        };
        let mut state = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let identity_became_known_after_invalidation = stale_probe_must_not_publish(
            started_generation,
            state.invalidation_generation,
            started_without_identity,
            &key,
            known_identity.as_deref(),
            &state.recent_invalidations,
        );
        if identity_became_known_after_invalidation {
            waiter
                .invalidated
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if !waiter
            .invalidated
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            let now = cache_now();
            // Evict expired entries for other roots opportunistically so the
            // map stays bounded by live roots rather than every root seen.
            state.entries.retain(|_, entry| entry.expires_at > now);
            let expires_at = now + GIT_STATUS_CACHE_TTL;
            state.entries.insert(
                key.clone(),
                GitStatusCacheEntry {
                    expires_at,
                    value: value.clone(),
                    repo_identity: repo_identity.clone(),
                },
            );
            if let Some(status) = &value {
                state.entries.insert(
                    status.root.clone(),
                    GitStatusCacheEntry {
                        expires_at,
                        value: Some(status.clone()),
                        repo_identity: repo_identity.clone(),
                    },
                );
            }
        }
        drop(state);
        drop(guard);
        return value;
    }
}

/// Drop cached status evidence for every repository containing `path` (or
/// nested under it) after a successful write. Used by write paths that know
/// the touched filesystem path rather than the repository root.
///
/// Literal prefix matching is not enough when two snapshots name the same
/// repository through different directory symlinks. Entries and in-flight
/// probes that share a canonical repo identity recorded at probe time are
/// dropped together. This does not stat a lexically protected path.
pub fn invalidate_git_status_cache_for_path(path: &Path) {
    invalidate_git_status_cache_matching(path, false);
}

/// Drop cached status evidence for one repository after a successful write.
pub fn invalidate_git_status_cache(root: &Path) {
    invalidate_git_status_cache_matching(root, true);
}

fn invalidate_git_status_cache_matching(path: &Path, exact_root: bool) {
    let Some(cache) = GIT_STATUS_CACHE.get() else {
        return;
    };
    // Canonicalize before taking the process-wide cache lock. A stuck mount
    // must not freeze status probes for every other repository.
    let resolved_identity = safe_canonical_repo_identity(path);
    let mut state = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let key_hit = |key: &Path| {
        if exact_root {
            key == path || key.starts_with(path)
        } else {
            path.starts_with(key) || key.starts_with(path)
        }
    };
    let mut identities = Vec::new();
    if let Some(identity) = resolved_identity {
        identities.push(identity);
    }
    for (key, entry) in &state.entries {
        if key_hit(key) {
            if let Some(identity) = &entry.repo_identity {
                identities.push(identity.clone());
            }
        } else if let Some(identity) = &entry.repo_identity
            && repo_identity_overlaps(path, identity)
        {
            identities.push(identity.clone());
        }
    }
    for (key, in_flight) in &state.in_flight {
        let identity = in_flight
            .repo_identity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if key_hit(key) {
            if let Some(identity) = identity {
                identities.push(identity);
            }
        } else if let Some(identity) = identity
            && repo_identity_overlaps(path, &identity)
        {
            identities.push(identity);
        }
    }
    let matches_identity = |identity: &Path| {
        identities
            .iter()
            .any(|known| repo_identity_overlaps(known, identity))
            || repo_identity_overlaps(path, identity)
    };
    state.entries.retain(|key, entry| {
        if key_hit(key) {
            return false;
        }
        !entry
            .repo_identity
            .as_ref()
            .is_some_and(|identity| matches_identity(identity))
    });
    let mut invalidated_probes = Vec::new();
    for (key, in_flight) in &state.in_flight {
        let identity = in_flight
            .repo_identity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let hit = key_hit(key)
            || identity
                .as_ref()
                .is_some_and(|identity| matches_identity(identity));
        if hit {
            invalidated_probes.push(Arc::clone(in_flight));
        }
    }
    for in_flight in invalidated_probes {
        in_flight
            .invalidated
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let generation = state.invalidation_generation.saturating_add(1);
    state.invalidation_generation = generation;
    state.recent_invalidations.push(RecentInvalidation {
        generation,
        path: path.to_path_buf(),
        identities: identities.clone(),
    });
    let oldest = state
        .invalidation_generation
        .saturating_sub(RECENT_INVALIDATION_LIMIT);
    state
        .recent_invalidations
        .retain(|record| record.generation > oldest);
}

/// Canonical repository identity for a path this process may already inspect.
/// Lexically protected paths and Herdr-managed worktrees are left unresolved:
/// canonicalizing them is the syscall macOS gates behind a consent prompt.
fn safe_canonical_repo_identity(path: &Path) -> Option<PathBuf> {
    if crate::macos_permissions::is_protected_user_path_lexical(path)
        || is_herdr_managed_worktree(path)
        || crate::macos_permissions::is_protected_user_path(path)
    {
        return None;
    }
    #[cfg(test)]
    {
        let delay_ms = CANONICAL_IDENTITY_DELAY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|(delayed, _)| delayed == path)
            .map(|(_, delay)| *delay)
            .unwrap_or(0);
        if delay_ms > 0 {
            thread::sleep(Duration::from_millis(delay_ms));
        }
    }
    std::fs::canonicalize(path).ok()
}

/// A probe that started without a repository identity must not publish a
/// result read before a later invalidation when that invalidation cannot be
/// proven to name a different repository. A full history window is the same
/// answer: the matching record may already have rolled off.
fn stale_probe_must_not_publish(
    started_generation: u64,
    current_generation: u64,
    started_without_identity: bool,
    key: &Path,
    known_identity: Option<&Path>,
    recent: &[RecentInvalidation],
) -> bool {
    if started_without_identity
        && current_generation.saturating_sub(started_generation) > RECENT_INVALIDATION_LIMIT
    {
        return true;
    }
    recent.iter().any(|record| {
        if record.generation <= started_generation {
            return false;
        }
        record.path == key
            || key.starts_with(&record.path)
            || record.path.starts_with(key)
            || known_identity.is_some_and(|identity| {
                record
                    .identities
                    .iter()
                    .any(|known| repo_identity_overlaps(known, identity))
                    || repo_identity_overlaps(&record.path, identity)
            })
            || (started_without_identity && record.identities.is_empty())
    })
}

fn repo_identity_overlaps(path: &Path, identity: &Path) -> bool {
    path == identity || path.starts_with(identity) || identity.starts_with(path)
}

fn git_status_uncached(snapshot: &Value, root: &Path) -> Option<GitStatus> {
    #[cfg(test)]
    {
        *GIT_STATUS_PROBES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(root.to_path_buf())
            .or_insert(0) += 1;
    }
    if should_use_stable_broker(snapshot, root) {
        return broker_git_status(snapshot, root);
    }
    git_status_direct(root).map(|(dirty, changed_files)| GitStatus {
        root: root.to_path_buf(),
        dirty,
        changed_files,
        source: "local_git",
        repo_identity: safe_canonical_repo_identity(root),
    })
}

fn git_status_direct(root: &Path) -> Option<(bool, usize)> {
    let output = run_git(
        root,
        &["status", "--porcelain", "--untracked-files=normal"],
        GIT_STATUS_TIMEOUT,
    )?;
    if !output.success {
        return None;
    }
    let changed_files = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    Some((changed_files > 0, changed_files))
}

fn should_use_stable_broker(snapshot: &Value, path: &Path) -> bool {
    if crate::tcc_broker::is_broker_child_process() {
        return false;
    }
    // Decide from the lexical form first: resolving a protected path to answer
    // "is this protected?" is itself the syscall that parks on a consent prompt.
    if crate::macos_permissions::is_protected_user_path_lexical(path)
        || is_herdr_managed_worktree(path)
        || snapshot_declares_protected_repo_storage(snapshot, path)
    {
        return true;
    }
    // Only a lexically unprotected path may be resolved, so a symlink alias
    // into a protected folder is still routed to the broker without ever
    // parking the caller on the protected target itself.
    crate::macos_permissions::is_protected_user_path(path)
}

fn is_herdr_managed_worktree(path: &Path) -> bool {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .is_some_and(|home| path.starts_with(home.join(".herdr").join("worktrees")))
}

fn snapshot_declares_protected_repo_storage(snapshot: &Value, path: &Path) -> bool {
    array(snapshot, "workspaces").iter().any(|workspace| {
        let Some(worktree) = workspace.get("worktree").and_then(Value::as_object) else {
            return false;
        };
        let checkout = worktree
            .get("checkout_path")
            .and_then(Value::as_str)
            .or_else(|| worktree.get("path").and_then(Value::as_str))
            .map(Path::new);
        if !checkout.is_some_and(|checkout| path.starts_with(checkout)) {
            return false;
        }
        ["repo_key", "repo_root"]
            .into_iter()
            .filter_map(|field| worktree.get(field).and_then(Value::as_str))
            .any(|repo_path| {
                crate::macos_permissions::is_protected_user_path_lexical(Path::new(repo_path))
            })
    })
}

fn broker_git_status(snapshot: &Value, root: &Path) -> Option<GitStatus> {
    let value = crate::tcc_broker::git_status_via_stable_broker(snapshot, root).ok()?;
    broker_git_status_from_value(&value)
}

fn broker_git_status_from_value(value: &Value) -> Option<GitStatus> {
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let display_root = value.get("root").and_then(Value::as_str)?;
    // `repo_identity` is optional. A revision-5 broker omits it; the cache then
    // keeps the display root and does not pretend the alias is canonical.
    // Revision 6 is the first broker that must provide it.
    let minimum_identity_revision = crate::tcc_broker::REPO_IDENTITY_MIN_COMPAT_REVISION;
    let reported_revision = value
        .get("broker_compat_revision")
        .and_then(Value::as_u64)
        .and_then(|revision| u32::try_from(revision).ok());
    let identity_supported =
        reported_revision.is_none_or(|revision| revision >= minimum_identity_revision);
    let repo_identity = identity_supported
        .then(|| {
            value
                .get("repo_identity")
                .and_then(Value::as_str)
                .filter(|identity| !identity.is_empty())
                .map(PathBuf::from)
        })
        .flatten();
    let changed_files = value
        .get("counts")
        .and_then(|counts| counts.get("files"))
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())?;
    Some(GitStatus {
        root: PathBuf::from(display_root),
        dirty: changed_files > 0,
        changed_files,
        source: "tcc_broker",
        repo_identity,
    })
}

struct CommandOutput {
    success: bool,
    stdout: Vec<u8>,
}

fn run_git(cwd: &Path, args: &[&str], timeout: Duration) -> Option<CommandOutput> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    child_process::configure_process_group(&mut command);
    let mut child = command.spawn().ok()?;
    let _registration = child_process::register_owned_child("git-topology", &child);
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;
    let stdout_reader = thread::spawn(move || read_limited(stdout, GIT_OUTPUT_LIMIT));
    let stderr_reader = thread::spawn(move || read_limited(stderr, 64 * 1024));
    let status = match child_process::wait_bounded(&mut child, timeout).ok()? {
        Some(status) => status,
        None => {
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return None;
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "herdr-mcp-projects-{}-{sequence}-{unique}",
            std::process::id()
        ))
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
        assert!(project.git_status_observed);
        assert_eq!(project.git_status_source, Some("local_git"));
        assert_eq!(project.pane_ids, vec!["w1:p1"]);
        assert_eq!(topology.pane_to_workspace["w1:p1"], "w1");

        let routing = derive_routing(&snapshot);
        let routed_project = routing.projects.get(&root).unwrap();
        assert_eq!(routed_project.vcs, Some("git"));
        assert!(routed_project.managed);
        assert!(!routed_project.dirty);
        assert_eq!(routed_project.changed_files, 0);
        assert!(!routed_project.git_status_observed);
        assert_eq!(routed_project.git_status_source, None);
        assert_eq!(routed_project.pane_ids, vec!["w1:p1"]);
        assert_eq!(routing.pane_to_workspace["w1:p1"], "w1");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn routing_handles_non_repo_cwd_without_git_fallback() {
        let root = temp_dir();
        let nested = root.join("plain/nested");
        fs::create_dir_all(&nested).unwrap();
        let snapshot = json!({
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": nested.to_string_lossy()
            }],
            "agents": []
        });
        let routing = derive_routing(&snapshot);
        let project = routing.projects.get(&nested).unwrap();
        assert_eq!(project.vcs, None);
        assert!(!project.managed);
        fs::remove_dir_all(root).unwrap();
    }

    fn git_snapshot(root: &Path) -> Value {
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
    fn concurrent_derives_singleflight_one_git_status_probe() {
        let root = temp_dir();
        fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        let snapshot = git_snapshot(&root);
        invalidate_git_status_cache(&root);
        reset_git_status_probe_count(&root);
        let handles = (0..8)
            .map(|_| {
                let snapshot = snapshot.clone();
                std::thread::spawn(move || derive(&snapshot))
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(
            (1..=2).contains(&git_status_probe_count(&root)),
            "one status probe may resolve the repository identity once"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_status_ttl_expiry_refreshes_the_probe() {
        let root = temp_dir();
        fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        let snapshot = git_snapshot(&root);
        invalidate_git_status_cache(&root);
        reset_git_status_probe_count(&root);
        let first = derive(&snapshot);
        assert!(!first.projects[&root].dirty);
        advance_git_status_clock(GIT_STATUS_CACHE_TTL + Duration::from_millis(1));
        let second = derive(&snapshot);
        assert!(!second.projects[&root].dirty);
        assert_eq!(git_status_probe_count(&root), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_invalidation_makes_next_inspect_see_new_git_status() {
        let root = temp_dir();
        fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        let snapshot = git_snapshot(&root);
        invalidate_git_status_cache(&root);
        let clean = derive(&snapshot);
        assert!(!clean.projects[&root].dirty);
        fs::write(root.join("created-by-exec.txt"), "changed").unwrap();
        invalidate_git_status_cache(&root);
        let refreshed = derive(&snapshot);
        assert!(refreshed.projects[&root].dirty);
        assert_eq!(refreshed.projects[&root].changed_files, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_invalidation_for_path_makes_next_inspect_see_new_git_status() {
        let root = temp_dir();
        let nested = root.join("subdir");
        fs::create_dir_all(&nested).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        let snapshot = git_snapshot(&root);
        invalidate_git_status_cache(&root);
        let clean = derive(&snapshot);
        assert!(!clean.projects[&root].dirty);
        // A write surface that only knows the touched path still drops the
        // repository entry (the repo root is an ancestor of the path).
        let touched = nested.join("created-by-write.txt");
        fs::write(&touched, "changed").unwrap();
        invalidate_git_status_cache_for_path(&touched);
        let refreshed = derive(&snapshot);
        assert!(refreshed.projects[&root].dirty);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_invalidation_leaves_sibling_root_with_shared_name_prefix() {
        let parent = temp_dir();
        let repo = parent.join("repo");
        let sibling = parent.join("repo-backup");
        for root in [&repo, &sibling] {
            fs::create_dir_all(root).unwrap();
            assert!(
                Command::new("git")
                    .args(["init", "-q"])
                    .current_dir(root)
                    .status()
                    .unwrap()
                    .success()
            );
            invalidate_git_status_cache(root);
        }
        derive(&git_snapshot(&repo));
        derive(&git_snapshot(&sibling));
        let sibling_expiry = GIT_STATUS_CACHE
            .get()
            .and_then(|cache| {
                cache
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .entries
                    .get(&sibling)
                    .map(|entry| entry.expires_at)
            })
            .expect("sibling cache entry");
        // Path::starts_with compares whole components, so invalidating a path
        // under `repo` must not evict `repo-backup`'s entry while it is live.
        let touched = repo.join("created-by-write.txt");
        fs::write(&touched, "changed").unwrap();
        invalidate_git_status_cache_for_path(&touched);
        assert!(!git_status_cache_has(&repo));
        if cache_now() < sibling_expiry {
            assert!(
                git_status_cache_has(&sibling),
                "a sibling root sharing a name prefix keeps its cache entry"
            );
        }
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn expired_entries_are_evicted_on_the_next_probe() {
        let root_a = temp_dir();
        let root_b = temp_dir();
        for root in [&root_a, &root_b] {
            fs::create_dir_all(root).unwrap();
            assert!(
                Command::new("git")
                    .args(["init", "-q"])
                    .current_dir(root)
                    .status()
                    .unwrap()
                    .success()
            );
            invalidate_git_status_cache(root);
            reset_git_status_probe_count(root);
        }
        let snapshot_a = git_snapshot(&root_a);
        let snapshot_b = git_snapshot(&root_b);
        derive(&snapshot_a);
        derive(&snapshot_b);
        advance_git_status_clock(GIT_STATUS_CACHE_TTL + Duration::from_millis(1));
        derive(&snapshot_b);
        assert_eq!(git_status_probe_count(&root_b), 2);
        assert!(
            !git_status_cache_has(&root_a),
            "the stale A entry must be swept by B's publish"
        );
        assert!(git_status_cache_has(&root_b));
        fs::remove_dir_all(root_a).unwrap();
        fs::remove_dir_all(root_b).unwrap();
    }

    #[test]
    fn wedged_probe_marker_is_released_and_waiters_recover() {
        let root = temp_dir();
        fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        invalidate_git_status_cache(&root);
        reset_git_status_probe_count(&root);
        // Simulate a probe that died without its cleanup: an orphan marker
        // with no live prober. A waiter must not block on it forever.
        let orphan = Arc::new(GitStatusInFlight {
            done: Mutex::new(false),
            ready: Condvar::new(),
            invalidated: std::sync::atomic::AtomicBool::new(false),
            repo_identity: Mutex::new(None),
        });
        GIT_STATUS_CACHE
            .get_or_init(|| Mutex::new(GitStatusCacheState::default()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .in_flight
            .insert(root.clone(), orphan.clone());
        let snapshot = git_snapshot(&root);
        let handle = std::thread::spawn(move || derive(&snapshot));
        let topology = handle.join().unwrap();
        assert!(topology.projects[&root].git_status_observed);
        assert!(!topology.projects[&root].dirty);
        assert_eq!(git_status_probe_count(&root), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn slow_canonical_lookup_does_not_block_another_repository() {
        let blocked = temp_dir();
        let other = temp_dir();
        for root in [&blocked, &other] {
            fs::create_dir_all(root).unwrap();
            assert!(
                Command::new("git")
                    .args(["init", "-q"])
                    .current_dir(root)
                    .status()
                    .unwrap()
                    .success()
            );
            invalidate_git_status_cache(root);
            reset_git_status_probe_count(root);
        }
        set_canonical_identity_delay_for_test(&blocked, Duration::from_millis(400));
        let blocked_root = blocked.clone();
        let started = Instant::now();
        let handle = thread::spawn(move || invalidate_git_status_cache(&blocked_root));
        thread::sleep(Duration::from_millis(50));
        derive(&git_snapshot(&other));
        let other_started = Instant::now();
        // The second inspect is a cache hit. Its elapsed time is the lock wait,
        // not another git-status probe.
        derive(&git_snapshot(&other));
        let other_elapsed = other_started.elapsed();
        handle.join().unwrap();
        set_canonical_identity_delay_for_test(&blocked, Duration::ZERO);
        assert!(
            other_elapsed < Duration::from_millis(250),
            "another repository waited {other_elapsed:?} behind a slow canonical lookup; wall {:?}",
            started.elapsed()
        );
        assert_eq!(git_status_probe_count(&other), 1);
        fs::remove_dir_all(blocked).unwrap();
        fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn broker_alias_identity_invalidates_the_other_protected_alias() {
        let alias_a = PathBuf::from("/protected/alias-a");
        let alias_b = PathBuf::from("/protected/alias-b");
        let real = PathBuf::from("/private/real-repo");
        let cache = GIT_STATUS_CACHE.get_or_init(|| Mutex::new(GitStatusCacheState::default()));
        let waiter = Arc::new(GitStatusInFlight {
            done: Mutex::new(false),
            ready: Condvar::new(),
            invalidated: std::sync::atomic::AtomicBool::new(false),
            repo_identity: Mutex::new(None),
        });
        {
            let mut state = cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.entries.insert(
                alias_b.clone(),
                GitStatusCacheEntry {
                    expires_at: cache_now() + GIT_STATUS_CACHE_TTL + Duration::from_secs(30),
                    value: Some(GitStatus {
                        root: alias_b.clone(),
                        dirty: false,
                        changed_files: 0,
                        source: "tcc_broker",
                        repo_identity: Some(real.clone()),
                    }),
                    repo_identity: Some(real.clone()),
                },
            );
            state.in_flight.insert(alias_a.clone(), Arc::clone(&waiter));
        }
        *waiter
            .repo_identity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(real.clone());
        invalidate_git_status_cache_for_path(&alias_a);
        assert!(
            waiter
                .invalidated
                .load(std::sync::atomic::Ordering::Relaxed),
            "an in-flight probe that already learned the broker identity must be invalidated"
        );
        assert!(
            !git_status_cache_has(&alias_b),
            "alias B must drop when alias A names the same broker-validated repository"
        );
        cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .in_flight
            .remove(&alias_a);
    }

    #[test]
    fn unknown_broker_probe_does_not_publish_after_another_alias_write() {
        let alias_a = PathBuf::from("/protected/unknown-a");
        let alias_b = PathBuf::from("/protected/unknown-b");
        let real = PathBuf::from("/private/unknown-real");
        let cache = GIT_STATUS_CACHE.get_or_init(|| Mutex::new(GitStatusCacheState::default()));
        let started_generation = {
            let mut state = cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let generation = state.invalidation_generation;
            state.in_flight.insert(
                alias_a.clone(),
                Arc::new(GitStatusInFlight {
                    done: Mutex::new(false),
                    ready: Condvar::new(),
                    invalidated: std::sync::atomic::AtomicBool::new(false),
                    repo_identity: Mutex::new(None),
                }),
            );
            generation
        };
        invalidate_git_status_cache_for_path(&alias_b);
        let (current_generation, recent) = {
            let state = cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                state.invalidation_generation,
                state.recent_invalidations.clone(),
            )
        };
        assert!(
            stale_probe_must_not_publish(
                started_generation,
                current_generation,
                true,
                &alias_a,
                Some(real.as_path()),
                &recent,
            ),
            "an unresolved protected invalidation must block the other alias"
        );
        let mut state = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.in_flight.remove(&alias_a);
        state.entries.remove(&alias_a);
        state.entries.remove(&alias_b);
    }

    #[test]
    fn unknown_identity_probe_refuses_publish_after_invalidation_history_overflows() {
        let alias_a = PathBuf::from("/protected/overflow-a");
        let alias_b = PathBuf::from("/protected/overflow-b");
        let real = PathBuf::from("/private/overflow-real");
        let cache = GIT_STATUS_CACHE.get_or_init(|| Mutex::new(GitStatusCacheState::default()));
        let started_generation = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .invalidation_generation;
        invalidate_git_status_cache_for_path(&alias_b);
        let unrelated = temp_dir();
        fs::create_dir_all(&unrelated).unwrap();
        for _ in 0..=RECENT_INVALIDATION_LIMIT {
            invalidate_git_status_cache(&unrelated);
        }
        let (current_generation, recent) = {
            let state = cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                state.invalidation_generation,
                state.recent_invalidations.clone(),
            )
        };
        assert!(current_generation.saturating_sub(started_generation) > RECENT_INVALIDATION_LIMIT);
        assert!(
            !recent.iter().any(|record| record.path == alias_b),
            "the matching invalidation must have rolled out of the 64-record window"
        );
        assert!(
            stale_probe_must_not_publish(
                started_generation,
                current_generation,
                true,
                &alias_a,
                Some(real.as_path()),
                &recent,
            ),
            "a probe cannot publish once the invalidation history no longer proves safety"
        );
        fs::remove_dir_all(unrelated).unwrap();
    }

    #[test]
    fn revision_five_broker_status_keeps_display_root_without_repo_identity() {
        let alias = PathBuf::from("/protected/legacy-alias");
        let value = json!({
            "ok": true,
            "root": alias.to_string_lossy(),
            "counts": {"files": 0}
        });
        let status = broker_git_status_from_value(&value).unwrap();
        assert_eq!(status.root, alias);
        assert_eq!(status.repo_identity, None);
        let current = json!({
            "ok": true,
            "root": alias.to_string_lossy(),
            "repo_identity": "/private/legacy-real",
            "counts": {"files": 1}
        });
        let identified = broker_git_status_from_value(&current).unwrap();
        assert_eq!(identified.root, alias);
        assert_eq!(
            identified.repo_identity.as_deref(),
            Some(Path::new("/private/legacy-real"))
        );
        assert!(identified.dirty);
    }

    #[cfg(unix)]
    #[test]
    fn subdirectory_broker_status_keeps_the_git_root_for_routing() {
        use std::os::unix::fs::symlink;

        let parent = temp_dir();
        let real = parent.join("real");
        let nested = real.join("src");
        fs::create_dir_all(&nested).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&real)
                .status()
                .unwrap()
                .success()
        );
        let alias = parent.join("alias");
        symlink(&real, &alias).unwrap();
        let nested_alias = alias.join("src");
        let canonical_root = fs::canonicalize(&real).unwrap();
        let value = json!({
            "ok": true,
            "root": canonical_root.to_string_lossy(),
            "repo_identity": canonical_root.to_string_lossy(),
            "counts": {"files": 0}
        });
        let status = broker_git_status_from_value(&value).unwrap();
        assert_eq!(status.root, canonical_root);
        assert_ne!(status.root, nested_alias);
        assert_eq!(
            status.repo_identity.as_deref(),
            Some(canonical_root.as_path())
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn routing_trusts_declared_worktree_without_touching_the_filesystem() {
        let root = PathBuf::from("/Users/example/Documents/does-not-need-to-exist");
        let nested = root.join("nested");
        let snapshot = json!({
            "workspaces": [{
                "workspace_id": "w1",
                "worktree": {
                    "checkout_path": root.to_string_lossy(),
                    "repo_root": root.to_string_lossy(),
                    "repo_key": "/Users/example/Documents/does-not-need-to-exist/.git"
                }
            }],
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": nested.to_string_lossy()
            }],
            "agents": []
        });

        let topology = derive_routing(&snapshot);
        let project = topology.projects.get(&root).unwrap();
        assert_eq!(project.vcs, Some("git"));
        assert!(project.managed);
        assert!(!project.git_status_observed);
        assert_eq!(project.cwds, vec![nested]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn declared_protected_repo_storage_routes_linked_checkout_to_broker() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let checkout = PathBuf::from("/tmp/herdr-linked-checkout-not-present");
        let repo_root = home.join("Documents").join("repo-not-present");
        let snapshot = json!({
            "workspaces": [{
                "workspace_id": "w1",
                "worktree": {
                    "checkout_path": checkout.to_string_lossy(),
                    "repo_key": repo_root.join(".git").to_string_lossy(),
                    "repo_root": repo_root.to_string_lossy()
                }
            }]
        });
        assert!(snapshot_declares_protected_repo_storage(
            &snapshot, &checkout
        ));
    }

    #[test]
    fn herdr_managed_worktree_root_discovery_does_not_require_broker() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let checkout = home
            .join(".herdr")
            .join("worktrees")
            .join("repo")
            .join("feature-not-present");
        assert!(is_herdr_managed_worktree(&checkout));
        assert!(!should_use_broker_for_toplevel(&json!({}), &checkout));
    }
}
