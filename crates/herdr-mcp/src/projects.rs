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
    /// Canonical path of the probed repository. Entries keyed under different
    /// aliases (symlinked checkouts, `/var` vs `/private/var`) share one
    /// identity so a write through any alias invalidates all of them.
    repo_identity: Option<PathBuf>,
}

/// A probe currently running for one root. `invalidated` is set when a write
/// under that root — or under any alias sharing its repo identity — lands
/// while the probe is in flight, so the stale result is never published.
struct GitStatusInFlight {
    done: Mutex<bool>,
    ready: Condvar,
    invalidated: std::sync::atomic::AtomicBool,
    repo_identity: Option<PathBuf>,
    /// `GitStatusCacheState::invalidation_epoch` at registration. Waiters
    /// with no provable identity (protected or unresolvable roots) may only
    /// publish while the epoch is unchanged — any invalidation during the
    /// probe could be a write through an alias the runtime cannot see, so a
    /// conservative epoch check replaces the unanswerable identity match.
    started_epoch: u64,
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
    /// Bumped on every cache invalidation. Identity-less in-flight probes
    /// compare `started_epoch` against this before publishing (see
    /// `GitStatusInFlight`).
    invalidation_epoch: u64,
}

static GIT_STATUS_CACHE: OnceLock<Mutex<GitStatusCacheState>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static DERIVE_ROUTING_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
static GIT_STATUS_PROBES: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

/// Test-only per-root probe latency: `git_status_uncached` sleeps this long
/// before running the probe so a test can invalidate mid-flight.
#[cfg(test)]
static GIT_STATUS_PROBE_DELAY_MS: OnceLock<Mutex<HashMap<PathBuf, std::time::Duration>>> =
    OnceLock::new();

/// Park the next probe for `root` by `delay`. Pass `Duration::ZERO` (or drop
/// the entry) to release; tests clear the entry after the interleave ends.
#[cfg(test)]
pub(crate) fn set_git_status_probe_delay(root: &Path, delay: Option<std::time::Duration>) {
    let delays = GIT_STATUS_PROBE_DELAY_MS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = delays
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match delay {
        Some(value) => {
            map.insert(root.to_path_buf(), value);
        }
        None => {
            map.remove(root);
        }
    }
}

#[cfg(test)]
pub(crate) fn reset_git_status_probe_count(root: &Path) {
    if let Some(probes) = GIT_STATUS_PROBES.get() {
        probes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(root);
    }
}

/// Count live cache entries whose key shares a path prefix with `root`,
/// regardless of expiry. Tests assert broker write invalidation
/// deterministically: expired-but-kept entries keep counting, so a retained
/// entry stays `>= 1` even when a route call outlasts the TTL, while a
/// dropped entry reads `0`. Scoping by prefix keeps parallel tests' roots
/// from polluting the count.
#[cfg(test)]
pub(crate) fn git_status_cache_entry_count_for(root: &Path) -> usize {
    let identity = std::fs::canonicalize(root).ok();
    GIT_STATUS_CACHE
        .get()
        .map(|cache| {
            cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entries
                .iter()
                .filter(|(key, entry)| {
                    root.starts_with(key.as_path())
                        || key.starts_with(root)
                        || (identity.is_some() && entry.repo_identity == identity)
                })
                .count()
        })
        .unwrap_or_default()
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
    root: PathBuf,
    dirty: bool,
    changed_files: usize,
    source: &'static str,
    /// Canonical repository identity when the probe source can prove it —
    /// the broker supplies one for protected roots it can see and the
    /// runtime cannot. `None` keeps invalidation on path prefixes only.
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
        let state = cache
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
                    // Poison the evicted waiter too: its late result must be
                    // refused at the publish fence even if it raced a new
                    // probe's registration (Codex 7035056472 / Claude
                    // 7035902838 P1).
                    waiter
                        .invalidated
                        .store(true, std::sync::atomic::Ordering::Relaxed);
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
        drop(state);

        // Resolve the caller-visible path identity OUTSIDE the lock:
        // `repo_identity_for` may block up to ~500ms on macOS
        // (bounded_real_path worker round-trip) and must not stall every
        // cache reader or invalidation.
        let probe_identity = repo_identity_for(&key);
        let mut state = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Re-check under the lock: a concurrent probe may have published or
        // registered while the identity resolved above. Both cases fold back
        // into the top of the loop, which returns the fresh entry or joins
        // the existing waiter. Any invalidation that ran meanwhile is also
        // covered: it removed the entry and bumped `invalidation_epoch`.
        if let Some(entry) = state.entries.get(&key)
            && entry.expires_at > cache_now()
        {
            return entry.value.clone();
        }
        if state.in_flight.contains_key(&key) {
            drop(state);
            continue;
        }
        let waiter = Arc::new(GitStatusInFlight {
            done: Mutex::new(false),
            ready: Condvar::new(),
            invalidated: std::sync::atomic::AtomicBool::new(false),
            repo_identity: probe_identity,
            started_epoch: state.invalidation_epoch,
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
        // This waiter may have been evicted by a wait-timeout — or poisoned
        // directly — while the probe ran; it may publish only while it is
        // still the registered in-flight probe for this key AND unflagged
        // (Codex 7035056472 / Claude 7035902838 P1). Generation fence: an
        // identity-less probe cannot be matched to any write's repo
        // identity, so it may only publish when NO invalidation at all ran
        // during the probe — the write could have landed through an alias
        // the runtime cannot see (Codex 7030506844 P1). Probes with a
        // provable identity rely on the direct `invalidated` flag instead:
        // unrelated writes must not churn them.
        let registered = state
            .in_flight
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &waiter));
        let stale = !registered
            || waiter
                .invalidated
                .load(std::sync::atomic::Ordering::Relaxed)
            || (waiter.repo_identity.is_none() && state.invalidation_epoch != waiter.started_epoch);
        if !stale {
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
                    // Prefer the status's repo identity (broker-supplied for
                    // protected roots the runtime may not resolve); the cwd
                    // identity is the fallback so a subdirectory probe keeps
                    // its own alias identity for prefix overlap matching.
                    repo_identity: value
                        .as_ref()
                        .and_then(|status| status.repo_identity.clone())
                        .or_else(|| waiter.repo_identity.clone()),
                },
            );
            if let Some(status) = &value {
                state.entries.insert(
                    status.root.clone(),
                    GitStatusCacheEntry {
                        expires_at,
                        value: Some(status.clone()),
                        repo_identity: status
                            .repo_identity
                            .clone()
                            .or_else(|| waiter.repo_identity.clone()),
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
    state.invalidation_epoch += 1;
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

/// Drop cached status evidence after a write when the caller knows the
/// touched repository's caller-visible `root` and, when available, its
/// canonical `repo_identity`. A key or in-flight probe is dropped when it
/// shares a path prefix with `root` OR shares a canonical identity prefix
/// with `repo_identity`. Identities overlap bidirectionally because a probe
/// keyed on a subdirectory records that subdirectory's canonical path while
/// a write reports the repository root — both spellings of the same repo
/// must still match (#1023, Claude 7011094552). Entries recorded without an
/// identity keep the prefix-only behavior.
pub fn invalidate_git_status_cache_matching(root: &Path, repo_identity: Option<&Path>) {
    let Some(cache) = GIT_STATUS_CACHE.get() else {
        return;
    };
    let identity = repo_identity.map(PathBuf::from);
    let matches = |key: &Path, entry_identity: Option<&PathBuf>| {
        if root.starts_with(key) || key.starts_with(root) {
            return true;
        }
        let (Some(identity), Some(entry_identity)) = (&identity, entry_identity) else {
            return false;
        };
        entry_identity.starts_with(identity) || identity.starts_with(entry_identity)
    };
    let mut state = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.invalidation_epoch += 1;
    state
        .entries
        .retain(|key, entry| !matches(key, entry.repo_identity.as_ref()));
    for (key, in_flight) in &state.in_flight {
        if matches(key, in_flight.repo_identity.as_ref()) {
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
    state.invalidation_epoch += 1;
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

/// Drop ALL cached status evidence and poison every in-flight probe. Bounded
/// and strictly conservative: used when a write completed but the responder
/// could not report the touched repository (an old broker response without
/// `repo_identity` — Codex 7030506844 P1). Any cache state retained past
/// that point could alias the write's root, so nothing is trusted.
pub fn invalidate_git_status_cache_all() {
    let Some(cache) = GIT_STATUS_CACHE.get() else {
        return;
    };
    let mut state = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.invalidation_epoch += 1;
    state.entries.clear();
    for in_flight in state.in_flight.values() {
        in_flight
            .invalidated
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
fn git_status_uncached(snapshot: &Value, root: &Path) -> Option<GitStatus> {
    #[cfg(test)]
    let delay = GIT_STATUS_PROBE_DELAY_MS
        .get()
        .and_then(|delays| delays.lock().ok().and_then(|map| map.get(root).copied()));
    let result = if should_use_stable_broker(snapshot, root) {
        broker_git_status(snapshot, root)
    } else {
        git_status_direct(root).map(|(dirty, changed_files)| GitStatus {
            root: root.to_path_buf(),
            dirty,
            changed_files,
            source: "local_git",
            repo_identity: repo_identity_for(root),
        })
    };
    #[cfg(test)]
    {
        *GIT_STATUS_PROBES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(root.to_path_buf())
            .or_insert(0) += 1;
        // Controlled latency AFTER the probe computed: a test parks the
        // prober on its way to publish so it can invalidate mid-flight and
        // still distinguish the probe's (already captured) result from a
        // fresh re-probe. The publish fences exist for exactly this
        // interleave (#1023).
        if let Some(delay) = delay {
            std::thread::sleep(delay);
        }
    }
    result
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

/// Canonical repository identity for a probed root, without letting a
/// consent-gated path park the caller or probing what the runtime must not
/// touch (#1023: the unconditional `std::fs::canonicalize` here violated
/// `runtime_must_not_probe` on protected paths). Protected or unresolvable
/// paths return `None`; the broker-supplied identity on the status covers
/// them, and without one the entry keeps prefix-only matching.
fn repo_identity_for(root: &Path) -> Option<PathBuf> {
    if crate::macos_permissions::runtime_must_not_probe(root) {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        match crate::macos_permissions::bounded_real_path(root) {
            crate::macos_permissions::BoundedRealPath::Resolved(path) => Some(path),
            _ => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::canonicalize(root).ok()
    }
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
        // The broker child resolves the canonical root where the runtime may
        // not probe; trust its `repo_identity` verbatim so a write through
        // another alias of this repository can invalidate this entry.
        repo_identity: value
            .get("repo_identity")
            .and_then(Value::as_str)
            .map(PathBuf::from),
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

    fn temp_dir() -> PathBuf {
        // pid + atomic + nanos: parallel tests in this and other binaries must
        // never collide on the same fixture directory (Claude 7035902838).
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
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
        assert_eq!(git_status_probe_count(&root), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_status_ttl_expiry_refreshes_the_probe() {
        // Shared-cache invalidations from parallel tests must not poison this
        // probe count (pi 7035463441).
        let _env_guard = crate::test_env::lock();
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
    fn expired_entries_are_evicted_on_the_next_probe() {
        // The clear-all fallback poisons unrelated in-flight probes; hold the
        // shared-cache lock so this probe count cannot be disturbed (pi
        // 7035463441).
        let _env_guard = crate::test_env::lock();
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
            repo_identity: None,
            started_epoch: 0,
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

    /// user operator | Given a probe wedged past the wait bound, a second
    /// caller that takes over, and a write during the takeover | When the
    /// evicted probe finally returns | Then its stale result is refused and
    /// the invalidation wins — no entry is backfilled (Codex 7035056472 /
    /// Claude 7035902838 P1).
    #[test]
    fn evicted_probe_result_is_refused_after_a_write() {
        let _env_guard = crate::test_env::lock();
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

        // T1 probes slowly; the main caller waits past the 50ms test bound,
        // evicts T1's marker, and becomes the replacement prober.
        reset_git_status_probe_count(&root);
        set_git_status_probe_delay(&root, Some(Duration::from_millis(200)));
        let snapshot = git_snapshot(&root);
        let first = std::thread::spawn({
            let snapshot = snapshot.clone();
            move || derive(&snapshot)
        });
        // Barrier: the first probe must actually be parked inside
        // git_status_uncached before the takeover attempt starts.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while git_status_probe_count(&root) == 0 {
            assert!(std::time::Instant::now() < deadline, "probe never ran");
            std::thread::yield_now();
        }
        // This derive waits on T1, times out at the test bound, evicts the
        // marker, and starts its own delayed probe.
        let second = std::thread::spawn({
            let snapshot = snapshot.clone();
            move || derive(&snapshot)
        });
        // Barrier: the takeover probe registered (count 2) before the write
        // lands, so the invalidation runs strictly between takeover and its
        // publish.
        while git_status_probe_count(&root) < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "takeover probe never ran"
            );
            std::thread::yield_now();
        }
        // The write lands while BOTH probes are parked post-computation.
        std::fs::write(root.join("written-after-probe.txt"), "dirty\n").unwrap();
        invalidate_git_status_cache(&root);
        first.join().unwrap();
        second.join().unwrap();
        set_git_status_probe_delay(&root, None);

        // Distinguishing evidence: every parked probe captured the CLEAN
        // status before the write, so any dirty result now observable can
        // only come from a fresh re-probe — had an evicted or invalidated
        // probe published, the cache would read clean forever.
        let latest = derive(&snapshot);
        assert!(
            latest.projects[&root].dirty,
            "the post-write state is observable — no stale clean publish survived: {latest:?}"
        );
        assert!(
            git_status_probe_count(&root) >= 3,
            "the refused publishes must have forced a fresh probe: {}",
            git_status_probe_count(&root)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identity_less_probe_never_publishes_across_a_write() {
        let _env_guard = crate::test_env::lock();
        // Codex 7030506844 P1: a probe whose repo identity cannot be proven
        // (protected root, or any path the runtime cannot resolve) must not
        // publish pre-write state once ANY invalidation ran mid-flight — the
        // write could have landed through an alias nobody can see. Here the
        // "write" reports an unrelated root: neither path nor identity
        // matches the probe's key, so only the epoch fence can stop it.
        // The snapshot declares a worktree so the project is real, and the
        // probe count proves the interleave actually ran (Claude 7035902838:
        // the previous form could pass vacuously).
        let gone = temp_dir().join("identity-less-probe");
        let unrelated = temp_dir().join("unrelated-write");
        fs::create_dir_all(&unrelated).unwrap();
        let snapshot = json!({
            "workspaces": [{
                "workspace_id": "w1",
                "worktree": {"checkout_path": gone.to_string_lossy()}
            }],
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": gone.to_string_lossy()
            }],
            "agents": []
        });
        reset_git_status_probe_count(&gone);
        set_git_status_probe_delay(&gone, Some(Duration::from_millis(400)));
        let probe_snapshot = snapshot.clone();
        let gone_probe = gone.clone();
        let handle = std::thread::spawn(move || derive(&probe_snapshot));
        // Explicit barrier: spin until the prober has registered AND parked
        // inside the delayed probe — only then invalidate, so the order
        // register → invalidate → resume → refuse is deterministic, not
        // timing luck.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while git_status_probe_count(&gone) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the prober never reached git_status_uncached"
            );
            std::thread::yield_now();
        }
        invalidate_git_status_cache_matching(&unrelated, None);
        handle.join().unwrap();
        set_git_status_probe_delay(&gone, None);
        assert_eq!(
            git_status_cache_entry_count_for(&gone_probe),
            0,
            "an identity-less probe must not publish a result gathered while \
             a write could have landed elsewhere (stale publish would be \
             indistinguishable from the pre-write state it replaces)"
        );
        fs::remove_dir_all(unrelated.parent().unwrap()).unwrap_or_default();
    }

    #[test]
    fn identified_probe_survives_an_unrelated_write() {
        let _env_guard = crate::test_env::lock();
        // The fence's inverse: a probe whose repo identity IS provable keeps
        // its result when the invalidation cannot match it — unrelated writes
        // must not churn healthy probes.
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
        let unrelated = temp_dir().join("unrelated-write-b");
        fs::create_dir_all(&unrelated).unwrap();
        let snapshot = git_snapshot(&root);
        set_git_status_probe_delay(&root, Some(Duration::from_millis(400)));
        let probe_snapshot = snapshot.clone();
        let handle = std::thread::spawn(move || derive(&probe_snapshot));
        std::thread::sleep(Duration::from_millis(50));
        invalidate_git_status_cache_matching(&unrelated, None);
        handle.join().unwrap();
        set_git_status_probe_delay(&root, None);
        assert!(
            git_status_cache_has(&root),
            "a probe with a provable identity publishes despite an unrelated write"
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(unrelated).unwrap();
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
