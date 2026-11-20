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
thread_local! {
    /// Per-test-thread offset: advancing the clock in one test must not
    /// expire cache entries another concurrently running test just wrote.
    static CACHE_CLOCK_OFFSET_MS: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

/// The caches' shared logical clock, used ONLY for read-side freshness
/// checks: `entry.expires_at > cache_now()`. Tests shift their own thread's
/// offset forward instead of sleeping so TTL expiry is deterministic.
/// Stored deadlines and eviction sweeps use the real `Instant::now()` —
/// entries are comparable across threads that way, so a test whose clock is
/// far ahead can never sweep entries another test just wrote into the
/// shared maps. Shared by the git-status, routing-topology, and
/// validated-roots caches.
pub(crate) fn cache_now() -> Instant {
    #[cfg(test)]
    {
        Instant::now()
            + Duration::from_millis(CACHE_CLOCK_OFFSET_MS.with(|offset| offset.get()) as u64)
    }
    #[cfg(not(test))]
    {
        Instant::now()
    }
}

#[cfg(test)]
pub(crate) fn advance_cache_clock(by: Duration) {
    CACHE_CLOCK_OFFSET_MS.with(|offset| offset.set(offset.get() + by.as_millis() as i64));
}

/// The calling test thread's clock offset. `git_statuses` propagates it
/// into its scoped worker threads so a shifted clock stays consistent
/// across one test's own probe fan-out.
#[cfg(test)]
fn cache_clock_offset() -> i64 {
    CACHE_CLOCK_OFFSET_MS.with(|offset| offset.get())
}

#[cfg(test)]
fn set_cache_clock_offset(offset: i64) {
    CACHE_CLOCK_OFFSET_MS.with(|slot| slot.set(offset));
}

struct GitStatusCacheEntry {
    expires_at: Instant,
    value: Option<GitStatus>,
}

/// A probe currently running for one root. `invalidated` is set when a write
/// under that root lands while the probe is in flight, so the stale result is
/// never published to the cache.
struct GitStatusInFlight {
    done: Mutex<bool>,
    ready: Condvar,
    invalidated: std::sync::atomic::AtomicBool,
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
}

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

/// Every snapshot input [`derive_routing`] reads, extracted cheaply. Two
/// snapshots with the same fingerprint produce an identical routing
/// topology, so hot callers can reuse one `Arc<ProjectTopology>` across
/// scroll churn that only rewrites unrelated pane fields (scroll offsets,
/// output text, cursor state).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RoutingFingerprint {
    workspaces: Vec<(String, PathBuf)>,
    pane_workspaces: Vec<(String, String)>,
    pane_cwds: Vec<(String, PathBuf)>,
}

/// Extract the routing fingerprint: declared workspace roots plus every
/// pane/agent's workspace binding and cwd, in sorted order.
pub(crate) fn routing_fingerprint(snapshot: &Value) -> RoutingFingerprint {
    let mut workspaces = declared_workspace_roots(snapshot)
        .into_iter()
        .collect::<Vec<_>>();
    workspaces.sort();
    let mut pane_workspaces = pane_workspaces(snapshot).into_iter().collect::<Vec<_>>();
    pane_workspaces.sort();
    let pane_cwds = pane_cwds(snapshot).into_iter().collect::<Vec<_>>();
    RoutingFingerprint {
        workspaces,
        pane_workspaces,
        pane_cwds,
    }
}

/// Bounded so alternating snapshot shapes cannot grow memory without limit.
pub(crate) const ROUTING_CACHE_CAP: usize = 32;

/// How long a cached routing topology or validated-roots entry stays fresh.
/// Matches the git-status cache's order of magnitude: long enough to absorb
/// scroll churn inside one request burst (validate + busy-check land
/// microseconds apart), short enough that a `git init` in a pane cwd or a
/// renamed worktree — neither of which changes the fingerprint — becomes
/// visible well under a second.
pub(crate) const ROUTING_CACHE_TTL: Duration = Duration::from_millis(250);

struct RoutingCacheEntry {
    expires_at: Instant,
    topology: Arc<ProjectTopology>,
}

/// Entries that carry an expiry so [`routing_cache_insert_bounded`] can
/// evict them. Implemented by the topology and validated-roots caches.
pub(crate) trait TimedCacheEntry {
    fn expires_at(&self) -> Instant;
}

impl TimedCacheEntry for RoutingCacheEntry {
    fn expires_at(&self) -> Instant {
        self.expires_at
    }
}

/// Bounded insert shared by the routing caches: evict expired entries
/// first, then apply the full-cap flush, so accumulated stale fingerprints
/// cannot wipe entries that are still live for other callers. The sweep
/// compares real deadlines (`Instant::now()`), never the test-shifted
/// logical clock — see [`cache_now`].
pub(crate) fn routing_cache_insert_bounded<K, V>(map: &mut HashMap<K, V>, key: K, value: V)
where
    K: Eq + std::hash::Hash,
    V: TimedCacheEntry,
{
    let now = Instant::now();
    map.retain(|_, entry| entry.expires_at() > now);
    if map.len() >= ROUTING_CACHE_CAP {
        map.clear();
    }
    map.insert(key, value);
}

static ROUTING_TOPOLOGY_CACHE: OnceLock<Mutex<HashMap<RoutingFingerprint, RoutingCacheEntry>>> =
    OnceLock::new();

/// [`derive_routing`] memoized on the routing fingerprint for
/// [`ROUTING_CACHE_TTL`]. High-frequency callers (exec/fs gates, worktree
/// scans) hit this during scroll churn and skip the full topology walk —
/// including the protected-cwd fallback into the stable-broker `git status`
/// probe.
///
/// The fingerprint covers every input the derivation reads, so a new
/// workspace or a changed pane/agent cwd misses immediately. Changes the
/// fingerprint cannot see (a `git init` inside an existing cwd, a renamed
/// repo, a swapped symlink) surface after the TTL. Per-request target-path
/// checks (canonicalize, secret-path denial, symlink escapes) are downstream
/// of the topology and stay live on every call.
///
/// The derivation runs *outside* the cache mutex: a slow protected-cwd probe
/// must not serialize callers of unrelated fingerprints behind the lock. Two
/// racing missers may derive the same deterministic topology twice; the
/// loser discards its copy when the winner's entry is still fresh.
pub fn derive_routing_cached(snapshot: &Value) -> Arc<ProjectTopology> {
    let fingerprint = routing_fingerprint(snapshot);
    let cache = ROUTING_TOPOLOGY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = guard.get(&fingerprint)
            && entry.expires_at > cache_now()
        {
            return Arc::clone(&entry.topology);
        }
    }
    let topology = Arc::new(derive_routing(snapshot));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let now = cache_now();
    if let Some(entry) = guard.get(&fingerprint)
        && entry.expires_at > now
    {
        return Arc::clone(&entry.topology);
    }
    // Deadlines are stored on the real clock so entries stay comparable
    // across threads (see `cache_now`); the read-side checks above use the
    // logical clock so tests can force a miss by advancing it.
    routing_cache_insert_bounded(
        &mut guard,
        fingerprint,
        RoutingCacheEntry {
            expires_at: Instant::now() + ROUTING_CACHE_TTL,
            topology: Arc::clone(&topology),
        },
    );
    topology
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
    root: PathBuf,
    dirty: bool,
    changed_files: usize,
    source: &'static str,
}

fn git_statuses(snapshot: &Value, roots: &[PathBuf]) -> HashMap<PathBuf, GitStatus> {
    thread::scope(|scope| {
        #[cfg(test)]
        let clock_offset = cache_clock_offset();
        let jobs = roots
            .iter()
            .map(|root| {
                let root = root.clone();
                scope.spawn(move || {
                    #[cfg(test)]
                    set_cache_clock_offset(clock_offset);
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
        let waiter = Arc::new(GitStatusInFlight {
            done: Mutex::new(false),
            ready: Condvar::new(),
            invalidated: std::sync::atomic::AtomicBool::new(false),
        });
        state.in_flight.insert(key.clone(), waiter.clone());
        drop(state);

        // `guard` drops after the publish below (or on probe panic), removing
        // the marker and waking waiters.
        let guard = GitStatusProbeGuard {
            key: key.clone(),
            waiter: waiter.clone(),
        };
        let value = git_status_uncached(snapshot, root);
        let mut state = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !waiter
            .invalidated
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            // Deadlines are stored on the real clock so entries stay
            // comparable across threads (see `cache_now`); evict expired
            // entries opportunistically so the map stays bounded by live
            // roots rather than every root seen.
            let now = Instant::now();
            state.entries.retain(|_, entry| entry.expires_at > now);
            let expires_at = now + GIT_STATUS_CACHE_TTL;
            state.entries.insert(
                key.clone(),
                GitStatusCacheEntry {
                    expires_at,
                    value: value.clone(),
                },
            );
            if let Some(status) = &value {
                state.entries.insert(
                    status.root.clone(),
                    GitStatusCacheEntry {
                        expires_at,
                        value: Some(status.clone()),
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
pub fn invalidate_git_status_cache_for_path(path: &Path) {
    let Some(cache) = GIT_STATUS_CACHE.get() else {
        return;
    };
    let mut state = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state
        .entries
        .retain(|key, _| !(path.starts_with(key) || key.starts_with(path)));
    for (key, in_flight) in &state.in_flight {
        if path.starts_with(key) || key.starts_with(path) {
            in_flight
                .invalidated
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Drop cached status evidence for one repository after a successful write.
pub fn invalidate_git_status_cache(root: &Path) {
    let Some(cache) = GIT_STATUS_CACHE.get() else {
        return;
    };
    let mut state = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state
        .entries
        .retain(|key, _| key != root && !key.starts_with(root));
    for (key, in_flight) in &state.in_flight {
        if key == root || key.starts_with(root) {
            in_flight
                .invalidated
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
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
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let resolved_root = value.get("root").and_then(Value::as_str)?;
    let changed_files = value
        .get("counts")
        .and_then(|counts| counts.get("files"))
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())?;
    Some(GitStatus {
        root: PathBuf::from(resolved_root),
        dirty: changed_files > 0,
        changed_files,
        source: "tcc_broker",
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
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_DIR_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        // Timestamp alone collides at microsecond-granularity clocks when
        // two calls land in the same tick; the atomic sequence + pid keep
        // every call unique regardless of clock granularity.
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_TEMP_DIR_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "herdr-mcp-projects-{}-{timestamp}-{sequence}",
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
        assert_eq!(git_status_probe_count(&root), 1);
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
        advance_cache_clock(GIT_STATUS_CACHE_TTL + Duration::from_millis(1));
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
        assert!(git_status_cache_has(&sibling));
        // Path::starts_with compares whole components, so invalidating a path
        // under `repo` must not evict `repo-backup`'s entry.
        let touched = repo.join("created-by-write.txt");
        fs::write(&touched, "changed").unwrap();
        invalidate_git_status_cache_for_path(&touched);
        assert!(!git_status_cache_has(&repo));
        assert!(
            git_status_cache_has(&sibling),
            "a sibling root sharing a name prefix keeps its cache entry"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn stale_entries_are_reprobed_instead_of_reused() {
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
        advance_cache_clock(GIT_STATUS_CACHE_TTL + Duration::from_millis(1));
        derive(&snapshot_b);
        assert_eq!(git_status_probe_count(&root_b), 2);
        // A's entry is equally stale: its next read re-probes rather than
        // reusing the cached value. (The publish sweep itself is internal
        // memory hygiene; the externally visible contract is the re-probe.)
        derive(&snapshot_a);
        assert_eq!(git_status_probe_count(&root_a), 2);
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

    #[test]
    fn routing_fingerprint_ignores_scroll_churn() {
        let base = json!({
            "workspaces": [{"workspace_id": "w1", "worktree": {"checkout_path": "/repo"}}],
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": "/repo/sub", "scrollback_offset": 0}],
            "agents": [{"pane_id": "w1:p2", "workspace_id": "w1", "cwd": "/repo", "status": "busy"}]
        });
        let mut churned = base.clone();
        churned["panes"][0]["scrollback_offset"] = json!(9000);
        churned["panes"][0]["output_tail"] = json!("new terminal text");
        churned["agents"][0]["status"] = json!("idle");
        assert_eq!(routing_fingerprint(&base), routing_fingerprint(&churned));
    }

    #[test]
    fn routing_fingerprint_changes_on_workspace_or_cwd_change() {
        let base = json!({
            "workspaces": [{"workspace_id": "w1", "worktree": {"checkout_path": "/repo"}}],
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": "/repo/sub"}],
            "agents": []
        });
        let mut new_workspace = base.clone();
        new_workspace["workspaces"]
            .as_array_mut()
            .unwrap()
            .push(json!({"workspace_id": "w2", "worktree": {"path": "/other"}}));
        assert_ne!(
            routing_fingerprint(&base),
            routing_fingerprint(&new_workspace)
        );

        let mut moved_pane = base.clone();
        moved_pane["panes"][0]["cwd"] = json!("/elsewhere");
        assert_ne!(routing_fingerprint(&base), routing_fingerprint(&moved_pane));

        let mut rebound_pane = base.clone();
        rebound_pane["panes"][0]["workspace_id"] = json!("w2");
        assert_ne!(
            routing_fingerprint(&base),
            routing_fingerprint(&rebound_pane)
        );
    }

    #[test]
    fn derive_routing_cached_reuses_topology_across_churn_and_misses_on_change() {
        // Unique cwds give this test a private fingerprint, so no cache
        // clear is needed and concurrent tests cannot disturb it.
        let root = temp_dir();
        fs::create_dir_all(&root).unwrap();
        let other = temp_dir();
        fs::create_dir_all(&other).unwrap();
        let snapshot = json!({
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}],
            "agents": []
        });
        let mut churned = snapshot.clone();
        churned["panes"][0]["scrollback_offset"] = json!(12_345);

        reset_derive_routing_call_count();

        let first = derive_routing_cached(&snapshot);
        let second = derive_routing_cached(&churned);
        assert!(
            Arc::ptr_eq(&first, &second),
            "scroll churn must reuse the cached topology"
        );
        assert_eq!(
            derive_routing_call_count(),
            1,
            "cached hits skip the full derivation"
        );

        let mut moved = snapshot.clone();
        moved["panes"][0]["cwd"] = json!(other.to_string_lossy());
        let third = derive_routing_cached(&moved);
        assert!(!Arc::ptr_eq(&first, &third), "a cwd change must re-derive");
        assert_eq!(derive_routing_call_count(), 2);

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn derive_routing_cached_re_derives_after_ttl_expiry() {
        let root = temp_dir();
        fs::create_dir_all(&root).unwrap();
        let snapshot = json!({
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}],
            "agents": []
        });

        reset_derive_routing_call_count();

        let first = derive_routing_cached(&snapshot);
        advance_cache_clock(ROUTING_CACHE_TTL + Duration::from_millis(1));
        let second = derive_routing_cached(&snapshot);
        assert!(
            !Arc::ptr_eq(&first, &second),
            "an expired entry must re-derive so fingerprint-invisible fs changes surface"
        );
        assert_eq!(derive_routing_call_count(), 2);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_insert_evicts_expired_before_cap_flush() {
        // Private map + probe entries: the flush semantics are pinned
        // without touching the shared caches, so parallel tests writing
        // fresh entries cannot interfere.
        struct ProbeEntry {
            expires_at: Instant,
            payload: Arc<()>,
        }
        impl TimedCacheEntry for ProbeEntry {
            fn expires_at(&self) -> Instant {
                self.expires_at
            }
        }

        let now = Instant::now();
        let live_expiry = now + Duration::from_secs(60);
        let mut map: HashMap<u32, ProbeEntry> = HashMap::new();
        let live_payload = Arc::new(());
        map.insert(
            0,
            ProbeEntry {
                expires_at: live_expiry,
                payload: Arc::clone(&live_payload),
            },
        );
        for index in 1..=(ROUTING_CACHE_CAP + 8) {
            map.insert(
                index as u32,
                ProbeEntry {
                    expires_at: now,
                    payload: Arc::new(()),
                },
            );
        }

        routing_cache_insert_bounded(
            &mut map,
            u32::MAX,
            ProbeEntry {
                expires_at: live_expiry,
                payload: Arc::new(()),
            },
        );
        assert_eq!(
            map.len(),
            2,
            "expired backfill must be evicted, not flushed"
        );
        assert!(
            Arc::ptr_eq(&live_payload, &map.get(&0).unwrap().payload),
            "still-live entries survive an expired-only overflow"
        );

        // A map that is genuinely full of fresh entries still flushes.
        map.clear();
        for index in 0..ROUTING_CACHE_CAP {
            map.insert(
                index as u32,
                ProbeEntry {
                    expires_at: live_expiry,
                    payload: Arc::new(()),
                },
            );
        }
        routing_cache_insert_bounded(
            &mut map,
            u32::MAX,
            ProbeEntry {
                expires_at: live_expiry,
                payload: Arc::new(()),
            },
        );
        assert_eq!(
            map.len(),
            1,
            "full-cap flush still applies when nothing expired"
        );
    }
}
