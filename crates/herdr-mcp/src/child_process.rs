use std::io::Read;
use std::process::Stdio;
use std::process::{Child, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const TERMINATE_GRACE: Duration = Duration::from_millis(250);
/// Bounded exponential micro-backoff for child exit polling. Short commands
/// finish within the first few sub-millisecond steps instead of paying a fixed
/// 10ms floor; long-running children settle at the `POLL_INTERVAL` cap.
const POLL_BACKOFF: [Duration; 6] = [
    Duration::from_micros(250),
    Duration::from_micros(500),
    Duration::from_millis(1),
    Duration::from_millis(2),
    Duration::from_millis(5),
    POLL_INTERVAL,
];
/// A child reaped by its spawning call within this window cannot outlive a
/// daemon restart, so it is never written to the on-disk orphan registry.
///
/// Trade-off (#1050 review): if the daemon dies inside this window, the child
/// was never recorded, so boot-time orphan reaping cannot find it. The window
/// is deliberately tiny (25 ms) and only covers commands that finish almost
/// immediately; anything longer is registered before the daemon could next
/// crash on a request path.
const REGISTRATION_GRACE: Duration = Duration::from_millis(25);

#[cfg(target_os = "macos")]
const REGISTRY_ENV: &str = "HERDR_MCP_CHILD_REGISTRY";
#[cfg(target_os = "macos")]
const REGISTRY_FILE: &str = "child-process-registry.json";
#[cfg(target_os = "macos")]
const REGISTRY_LOCK_FILE: &str = "child-process-registry.lock";
#[cfg(target_os = "macos")]
const REAP_EVIDENCE_FILE: &str = "child-process-reap-last.json";
#[cfg(target_os = "macos")]
const MAX_CONFIRMED_ORPHAN_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Put a direct tool child in its own process group so timeout/cancellation can
/// reap descendants as well as the immediate process.
pub(crate) fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

#[derive(Debug)]
pub(crate) struct BoundedOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) truncated: bool,
}

pub(crate) fn run_bounded_output(
    command: &mut Command,
    timeout: Duration,
    max_bytes: usize,
) -> std::io::Result<Option<BoundedOutput>> {
    let kind = command
        .get_program()
        .to_string_lossy()
        .rsplit('/')
        .next()
        .unwrap_or("child")
        .to_owned();
    run_bounded_output_inner(
        command,
        timeout,
        max_bytes,
        Some((REGISTRATION_GRACE, |child: &Child| {
            register_owned_child(&kind, child)
        })),
    )
}

/// Spawn `command` and collect bounded output. When `registration` is set,
/// the child is recorded as an owned orphan candidate only if it is still
/// running after the grace window, so transient commands never pay for the
/// identity probe or the registry lock and rewrite.
fn run_bounded_output_inner<R>(
    command: &mut Command,
    timeout: Duration,
    max_bytes: usize,
    registration: Option<(Duration, impl FnOnce(&Child) -> R)>,
) -> std::io::Result<Option<BoundedOutput>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(command);
    let started = Instant::now();
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout must be present");
    let mut stderr = child.stderr.take().expect("piped stderr must be present");
    let stdout_reader = thread::spawn(move || read_capped_and_drain(&mut stdout, max_bytes));
    let stderr_reader = thread::spawn(move || read_capped_and_drain(&mut stderr, max_bytes));
    let wait = match registration {
        Some((grace, register)) => wait_with_deferred_registration(
            &SystemClock,
            &mut child,
            started,
            timeout,
            grace,
            |child| child.try_wait(),
            register,
        ),
        None => {
            let mut backoff = PollBackoff::default();
            poll_until(&SystemClock, started + timeout, &mut backoff, || {
                child.try_wait()
            })
            .map(|status| (status, None))
        }
    };
    let (status, _registration) = match wait {
        Ok(result) => result,
        Err(error) => {
            terminate_and_reap(&mut child);
            return Err(error);
        }
    };
    if status.is_none() {
        terminate_and_reap(&mut child);
    }
    let (stdout, stdout_truncated) = stdout_reader.join().unwrap_or_default();
    let (stderr, stderr_truncated) = stderr_reader.join().unwrap_or_default();
    Ok(status.map(|status| BoundedOutput {
        status,
        stdout,
        stderr,
        truncated: stdout_truncated || stderr_truncated,
    }))
}

fn read_capped_and_drain(reader: &mut impl Read, max_bytes: usize) -> (Vec<u8>, bool) {
    let mut retained = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if retained.len() < max_bytes {
                    let keep = count.min(max_bytes - retained.len());
                    retained.extend_from_slice(&buffer[..keep]);
                    truncated |= keep < count;
                } else {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (retained, truncated)
}

/// Wait for a child for at most `timeout`. A timed-out child is terminated and
/// synchronously reaped before this function returns.
pub(crate) fn wait_bounded(
    child: &mut Child,
    timeout: Duration,
) -> std::io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    let mut backoff = PollBackoff::default();
    match poll_until(&SystemClock, deadline, &mut backoff, || child.try_wait()) {
        Ok(Some(status)) => Ok(Some(status)),
        Ok(None) => {
            terminate_and_reap(child);
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

trait PollClock {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

struct SystemClock;

impl PollClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

#[derive(Debug, Default)]
struct PollBackoff {
    step: usize,
}

impl PollBackoff {
    fn next_delay(&mut self) -> Duration {
        let delay = POLL_BACKOFF[self.step];
        self.step = (self.step + 1).min(POLL_BACKOFF.len() - 1);
        delay
    }
}

/// Poll until `poll` yields a value or `deadline` passes. The first poll is
/// immediate; each later sleep follows `POLL_BACKOFF`, truncated so the loop
/// never sleeps past the deadline.
fn poll_until<T>(
    clock: &impl PollClock,
    deadline: Instant,
    backoff: &mut PollBackoff,
    mut poll: impl FnMut() -> std::io::Result<Option<T>>,
) -> std::io::Result<Option<T>> {
    loop {
        if let Some(value) = poll()? {
            return Ok(Some(value));
        }
        let now = clock.now();
        if now >= deadline {
            return Ok(None);
        }
        clock.sleep(backoff.next_delay().min(deadline - now));
    }
}

/// Wait for `state` to finish within `timeout` of `started`, invoking
/// `register` once only if it is still running after `grace`. The returned
/// registration must outlive any termination of the timed-out child.
fn wait_with_deferred_registration<S, T, R>(
    clock: &impl PollClock,
    state: &mut S,
    started: Instant,
    timeout: Duration,
    grace: Duration,
    mut poll: impl FnMut(&mut S) -> std::io::Result<Option<T>>,
    register: impl FnOnce(&S) -> R,
) -> std::io::Result<(Option<T>, Option<R>)> {
    let deadline = started + timeout;
    let mut backoff = PollBackoff::default();
    let grace_deadline = started + grace.min(timeout);
    if let Some(value) = poll_until(clock, grace_deadline, &mut backoff, || poll(state))? {
        return Ok((Some(value), None));
    }
    if clock.now() >= deadline {
        return Ok((None, None));
    }
    // The grace boundary can land on a child that just exited: poll once more
    // before writing the registry entry, so a dead pid is never recorded and
    // later "reaped" on boot (#1050 review).
    if let Some(value) = poll(state)? {
        return Ok((Some(value), None));
    }
    let registration = register(state);
    let value = poll_until(clock, deadline, &mut backoff, || poll(state))?;
    Ok((value, Some(registration)))
}

/// Terminate only the exact process group created for this child, then reap the
/// immediate child. This intentionally never uses broad process-name matching.
pub(crate) fn terminate_and_reap(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        let term_result = unsafe { libc::kill(-pid, libc::SIGTERM) };
        if term_result != 0 {
            let _ = child.kill();
        }
        let deadline = Instant::now() + TERMINATE_GRACE;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
                _ => break,
            }
        }
        let kill_result = unsafe { libc::kill(-pid, libc::SIGKILL) };
        if kill_result != 0 {
            let _ = child.kill();
        }
        // A process wedged in an uninterruptible syscall cannot be reaped even
        // after SIGKILL, so the final wait is bounded too: blocking here would
        // park the caller forever.
        let reaped_by = Instant::now() + TERMINATE_GRACE;
        while Instant::now() < reaped_by {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => thread::sleep(POLL_INTERVAL),
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(target_os = "macos")]
mod registry {
    use super::*;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::env;
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(test)]
    thread_local! {
        static TEST_REGISTRY_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct ChildRecord {
        pid: u32,
        pgid: u32,
        parent_pid: u32,
        kind: String,
        command: String,
        process_start: String,
        spawned_at_ms: u64,
    }

    #[derive(Debug, Default, Serialize, Deserialize)]
    struct ChildRegistry {
        schema_version: u32,
        records: Vec<ChildRecord>,
    }

    #[derive(Debug, Clone)]
    struct ProcessIdentity {
        ppid: u32,
        pgid: u32,
        process_start: String,
        command: String,
    }

    #[derive(Debug, Clone, Serialize)]
    struct ReapDecision {
        pid: u32,
        kind: String,
        command: String,
        age_ms: u64,
        action: String,
        reason: String,
    }

    pub(crate) struct Registration {
        pid: Option<u32>,
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            if let Some(pid) = self.pid.take() {
                let _ = mutate_registry(|registry| {
                    registry.records.retain(|record| record.pid != pid);
                });
            }
        }
    }

    struct RegistryLock(File);

    impl RegistryLock {
        fn acquire(path: &Path) -> Result<Self, String> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("cannot create child registry dir: {error}"))?;
            }
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(path)
                .map_err(|error| format!("cannot open child registry lock: {error}"))?;
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if result == 0 {
                Ok(Self(file))
            } else {
                Err(format!(
                    "cannot lock child registry: {}",
                    std::io::Error::last_os_error()
                ))
            }
        }
    }

    impl Drop for RegistryLock {
        fn drop(&mut self) {
            let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
    }

    pub(super) fn register(kind: &str, child: &Child) -> Registration {
        register_pid(kind, child.id())
    }

    pub(super) fn register_pid(kind: &str, pid: u32) -> Registration {
        if !registry_enabled() {
            return Registration { pid: None };
        }
        let Some(identity) = inspect_process(pid) else {
            return Registration { pid: None };
        };
        if identity.ppid != std::process::id() || identity.pgid != pid {
            return Registration { pid: None };
        }
        let record = ChildRecord {
            pid,
            pgid: identity.pgid,
            parent_pid: std::process::id(),
            kind: sanitize_kind(kind),
            command: identity.command,
            process_start: identity.process_start,
            spawned_at_ms: now_ms(),
        };
        if mutate_registry(|registry| {
            registry.records.retain(|existing| existing.pid != pid);
            registry.records.push(record);
        })
        .is_ok()
        {
            Registration { pid: Some(pid) }
        } else {
            Registration { pid: None }
        }
    }

    pub(super) fn reap_on_boot() -> String {
        if !registry_enabled() {
            return "LAYER child-process registry=disabled".to_owned();
        }
        let Some(dir) = registry_dir() else {
            return "LAYER child-process registry=unavailable".to_owned();
        };
        let now = now_ms();
        let mut decisions = Vec::new();
        let result = mutate_registry(|registry| {
            let mut retained = Vec::new();
            for record in registry.records.drain(..) {
                let age_ms = now.saturating_sub(record.spawned_at_ms);
                if !process_alive(record.pid) {
                    decisions.push(ReapDecision {
                        pid: record.pid,
                        kind: record.kind,
                        command: record.command,
                        age_ms,
                        action: "removed_stale_record".to_owned(),
                        reason: "process_already_exited".to_owned(),
                    });
                    continue;
                }
                let identity = inspect_process(record.pid);
                let parent_alive = process_alive(record.parent_pid);
                let decision = ownership_reason(&record, identity.as_ref(), parent_alive, now);
                match decision {
                    Ok(()) => {
                        if terminate_orphan_group(record.pid) {
                            decisions.push(ReapDecision {
                                pid: record.pid,
                                kind: record.kind,
                                command: record.command,
                                age_ms,
                                action: "reaped".to_owned(),
                                reason: "confirmed_owned_orphan".to_owned(),
                            });
                        } else {
                            decisions.push(ReapDecision {
                                pid: record.pid,
                                kind: record.kind.clone(),
                                command: record.command.clone(),
                                age_ms,
                                action: "refused".to_owned(),
                                reason: "confirmed_identity_but_process_did_not_exit".to_owned(),
                            });
                            retained.push(record);
                        }
                    }
                    Err(reason) => {
                        decisions.push(ReapDecision {
                            pid: record.pid,
                            kind: record.kind.clone(),
                            command: record.command.clone(),
                            age_ms,
                            action: "refused".to_owned(),
                            reason,
                        });
                        retained.push(record);
                    }
                }
            }
            registry.records = retained;
        });
        if let Err(error) = result {
            return format!("LAYER child-process registry=error detail={error}");
        }
        let reaped = decisions
            .iter()
            .filter(|item| item.action == "reaped")
            .count();
        let refused = decisions
            .iter()
            .filter(|item| item.action == "refused")
            .count();
        let stale = decisions
            .iter()
            .filter(|item| item.action == "removed_stale_record")
            .count();
        let evidence = json!({
            "schema_version": 1,
            "at_ms": now,
            "reaped": reaped,
            "refused": refused,
            "stale_records_removed": stale,
            "decisions": decisions,
        });
        let _ = atomic_write_json(&dir.join(REAP_EVIDENCE_FILE), &evidence);
        format!(
            "LAYER child-process registry=ready boot_reaped={reaped} boot_refused={refused} stale_records_removed={stale}"
        )
    }

    pub(super) fn doctor_line() -> String {
        if !registry_enabled() {
            return "LAYER child-process registry=disabled".to_owned();
        }
        let active = load_registry().map_or(0, |registry| registry.records.len());
        let last = registry_dir()
            .and_then(|dir| fs::read(dir.join(REAP_EVIDENCE_FILE)).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let reaped = last
            .as_ref()
            .and_then(|value| value.get("reaped"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let refused = last
            .as_ref()
            .and_then(|value| value.get("refused"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        format!(
            "LAYER child-process registry=ready active={} last_boot_reaped={} last_boot_refused={}",
            active, reaped, refused
        )
    }

    fn ownership_reason(
        record: &ChildRecord,
        identity: Option<&ProcessIdentity>,
        parent_alive: bool,
        now: u64,
    ) -> Result<(), String> {
        if parent_alive {
            return Err("recorded_parent_still_alive".to_owned());
        }
        if now < record.spawned_at_ms {
            return Err("record_timestamp_in_future".to_owned());
        }
        if now - record.spawned_at_ms > MAX_CONFIRMED_ORPHAN_AGE_MS {
            return Err("record_too_old_for_automatic_reap".to_owned());
        }
        let Some(identity) = identity else {
            return Err("process_identity_unavailable".to_owned());
        };
        if identity.ppid != 1 {
            return Err(format!("unexpected_parent_pid_{}", identity.ppid));
        }
        if record.pgid != record.pid || identity.pgid != record.pid {
            return Err("process_group_identity_mismatch".to_owned());
        }
        if identity.process_start != record.process_start {
            return Err("process_start_identity_mismatch".to_owned());
        }
        if identity.command != record.command {
            return Err("process_command_identity_mismatch".to_owned());
        }
        Ok(())
    }

    fn inspect_process(pid: u32) -> Option<ProcessIdentity> {
        let mut command = Command::new("/bin/ps");
        // Keep lstart fields stable across workstation locales.
        command.env("LC_ALL", "C");
        command.args([
            "-p",
            &pid.to_string(),
            "-o",
            "ppid=",
            "-o",
            "pgid=",
            "-o",
            "lstart=",
            "-o",
            "comm=",
        ]);
        let output = run_bounded_output_inner(
            &mut command,
            Duration::from_secs(1),
            16 * 1024,
            None::<(Duration, fn(&Child))>,
        )
        .ok()
        .flatten()?;
        if !output.status.success() || output.truncated {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let parts = text.split_whitespace().collect::<Vec<_>>();
        if parts.len() < 8 {
            return None;
        }
        let ppid = parts[0].parse().ok()?;
        let pgid = parts[1].parse().ok()?;
        let process_start = parts[2..7].join(" ");
        let command = parts[7..].join(" ");
        Some(ProcessIdentity {
            ppid,
            pgid,
            process_start,
            command,
        })
    }

    fn process_alive(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        let result = unsafe { libc::kill(pid as i32, 0) };
        if result == 0 {
            true
        } else {
            std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
    }

    fn terminate_orphan_group(pid: u32) -> bool {
        if unsafe { libc::kill(-(pid as i32), libc::SIGTERM) } != 0 {
            return !process_alive(pid);
        }
        let deadline = Instant::now() + TERMINATE_GRACE;
        while process_alive(pid) && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
        if !process_alive(pid) {
            return true;
        }
        let _ = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        let deadline = Instant::now() + TERMINATE_GRACE;
        while process_alive(pid) && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
        !process_alive(pid)
    }

    fn registry_enabled() -> bool {
        #[cfg(test)]
        if TEST_REGISTRY_DIR.with(|slot| slot.borrow().is_some()) {
            return true;
        }
        env::var(REGISTRY_ENV).ok().as_deref() == Some("1") && registry_dir().is_some()
    }

    fn registry_dir() -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(path) = TEST_REGISTRY_DIR.with(|slot| slot.borrow().clone()) {
            return Some(path);
        }
        env::var_os("HERDR_MCP_CONFIG_DIR")
            .or_else(|| env::var_os("HERDR_MCP_STATE_DIR"))
            .map(PathBuf::from)
    }

    fn registry_path() -> Option<PathBuf> {
        registry_dir().map(|dir| dir.join(REGISTRY_FILE))
    }

    fn lock_path() -> Option<PathBuf> {
        registry_dir().map(|dir| dir.join(REGISTRY_LOCK_FILE))
    }

    fn mutate_registry(mutator: impl FnOnce(&mut ChildRegistry)) -> Result<(), String> {
        let path = registry_path().ok_or_else(|| "child registry path unavailable".to_owned())?;
        let lock_path =
            lock_path().ok_or_else(|| "child registry lock path unavailable".to_owned())?;
        let _lock = RegistryLock::acquire(&lock_path)?;
        let mut registry = load_registry_from(&path)?;
        mutator(&mut registry);
        registry.schema_version = 1;
        atomic_write_json(&path, &registry)
    }

    fn load_registry() -> Result<ChildRegistry, String> {
        let path = registry_path().ok_or_else(|| "child registry path unavailable".to_owned())?;
        load_registry_from(&path)
    }

    fn load_registry_from(path: &Path) -> Result<ChildRegistry, String> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("cannot parse child process registry: {error}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ChildRegistry {
                schema_version: 1,
                records: Vec::new(),
            }),
            Err(error) => Err(format!("cannot read child process registry: {error}")),
        }
    }

    fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent", path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create child process registry dir: {error}"))?;
        let bytes = serde_json::to_vec_pretty(value)
            .map_err(|error| format!("cannot encode child process registry: {error}"))?;
        let tmp = parent.join(format!(
            ".{}.tmp-{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("child-registry"),
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|error| format!("cannot write {}: {error}", tmp.display()))?;
        file.write_all(&bytes)
            .map_err(|error| format!("cannot write {}: {error}", tmp.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync {}: {error}", tmp.display()))?;
        fs::rename(&tmp, path)
            .map_err(|error| format!("cannot activate {}: {error}", path.display()))
    }

    fn sanitize_kind(kind: &str) -> String {
        let filtered = kind
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
            .take(48)
            .collect::<String>();
        if filtered.is_empty() {
            "child".to_owned()
        } else {
            filtered
        }
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn record() -> ChildRecord {
            ChildRecord {
                pid: 1234,
                pgid: 1234,
                parent_pid: 1111,
                kind: "rg".to_owned(),
                command: "/opt/homebrew/bin/rg".to_owned(),
                process_start: "Sat Aug 29 11:00:00 2026".to_owned(),
                spawned_at_ms: 10_000,
            }
        }

        fn identity() -> ProcessIdentity {
            ProcessIdentity {
                ppid: 1,
                pgid: 1234,
                process_start: "Sat Aug 29 11:00:00 2026".to_owned(),
                command: "/opt/homebrew/bin/rg".to_owned(),
            }
        }

        #[test]
        fn confirmed_orphan_requires_all_identity_signals() {
            assert!(ownership_reason(&record(), Some(&identity()), false, 20_000).is_ok());
            assert_eq!(
                ownership_reason(&record(), Some(&identity()), true, 20_000).unwrap_err(),
                "recorded_parent_still_alive"
            );
            let mut wrong = identity();
            wrong.process_start = "Sat Aug 29 12:00:00 2026".to_owned();
            assert_eq!(
                ownership_reason(&record(), Some(&wrong), false, 20_000).unwrap_err(),
                "process_start_identity_mismatch"
            );
            let mut wrong = identity();
            wrong.command = "/usr/bin/git".to_owned();
            assert_eq!(
                ownership_reason(&record(), Some(&wrong), false, 20_000).unwrap_err(),
                "process_command_identity_mismatch"
            );
        }

        #[test]
        fn old_registry_record_is_diagnostics_only() {
            let now = 10_000 + MAX_CONFIRMED_ORPHAN_AGE_MS + 1;
            assert_eq!(
                ownership_reason(&record(), Some(&identity()), false, now).unwrap_err(),
                "record_too_old_for_automatic_reap"
            );
        }

        #[test]
        fn boot_reaper_reaps_only_a_confirmed_orphan() {
            let root = env::temp_dir().join(format!(
                "herdr-mcp-child-registry-{}-{}",
                std::process::id(),
                now_ms()
            ));
            fs::create_dir_all(&root).unwrap();
            TEST_REGISTRY_DIR.with(|slot| *slot.borrow_mut() = Some(root.clone()));

            let script = r#"import os, subprocess
p = subprocess.Popen(['/bin/sleep', '30'], preexec_fn=lambda: os.setpgid(0, 0), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
print(p.pid, flush=True)
"#;
            let mut launcher = Command::new("/usr/bin/python3");
            launcher
                .args(["-c", script])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let launcher = launcher.spawn().unwrap();
            let launcher_pid = launcher.id();
            let output = launcher.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "launcher stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
            let orphan_pid: u32 = String::from_utf8(output.stdout)
                .unwrap()
                .trim()
                .parse()
                .unwrap();

            let identity = (0..40)
                .find_map(|_| {
                    let identity = inspect_process(orphan_pid)?;
                    if identity.ppid == 1 && identity.pgid == orphan_pid {
                        Some(identity)
                    } else {
                        thread::sleep(Duration::from_millis(25));
                        None
                    }
                })
                .expect("orphan must be reparented to launchd/init");
            assert!(!process_alive(launcher_pid));
            mutate_registry(|registry| {
                registry.records.push(ChildRecord {
                    pid: orphan_pid,
                    pgid: orphan_pid,
                    parent_pid: launcher_pid,
                    kind: "test-orphan".to_owned(),
                    command: identity.command.clone(),
                    process_start: identity.process_start.clone(),
                    spawned_at_ms: now_ms(),
                });
            })
            .unwrap();

            let report = reap_on_boot();
            assert!(report.contains("boot_reaped=1"), "{report}");
            for _ in 0..40 {
                if !process_alive(orphan_pid) {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
            assert!(!process_alive(orphan_pid));
            assert!(load_registry().unwrap().records.is_empty());

            TEST_REGISTRY_DIR.with(|slot| *slot.borrow_mut() = None);
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn transient_command_skips_registry_and_long_command_registers() {
            let root = env::temp_dir().join(format!(
                "herdr-mcp-child-registry-defer-{}-{}",
                std::process::id(),
                now_ms()
            ));
            fs::create_dir_all(&root).unwrap();
            TEST_REGISTRY_DIR.with(|slot| *slot.borrow_mut() = Some(root.clone()));
            let registry = root.join(REGISTRY_FILE);

            let mut command = Command::new("/usr/bin/true");
            let output = run_bounded_output(&mut command, Duration::from_secs(5), 1024)
                .unwrap()
                .unwrap();
            assert!(output.status.success());
            assert!(!registry.exists(), "transient child touched the registry");

            let mut command = Command::new("/bin/sleep");
            command.arg("0.3");
            let output = run_bounded_output(&mut command, Duration::from_secs(5), 1024)
                .unwrap()
                .unwrap();
            assert!(output.status.success());
            assert!(registry.exists(), "long child was never registered");
            assert!(load_registry().unwrap().records.is_empty());

            TEST_REGISTRY_DIR.with(|slot| *slot.borrow_mut() = None);
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) use registry::Registration as OwnedChildRegistration;

#[cfg(not(target_os = "macos"))]
pub(crate) struct OwnedChildRegistration;

#[cfg(target_os = "macos")]
pub(crate) fn register_owned_child(kind: &str, child: &Child) -> OwnedChildRegistration {
    registry::register(kind, child)
}

#[cfg(target_os = "macos")]
pub(crate) fn register_owned_pid(kind: &str, pid: u32) -> OwnedChildRegistration {
    registry::register_pid(kind, pid)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn register_owned_child(_kind: &str, _child: &Child) -> OwnedChildRegistration {
    OwnedChildRegistration
}

pub(crate) fn reap_confirmed_orphans_on_boot() -> String {
    #[cfg(target_os = "macos")]
    {
        registry::reap_on_boot()
    }
    #[cfg(not(target_os = "macos"))]
    {
        format!(
            "LAYER child-process registry=not-applicable platform={}",
            std::env::consts::OS
        )
    }
}

pub(crate) fn doctor_line() -> String {
    #[cfg(target_os = "macos")]
    {
        registry::doctor_line()
    }
    #[cfg(not(target_os = "macos"))]
    {
        format!(
            "LAYER child-process registry=not-applicable platform={}",
            std::env::consts::OS
        )
    }
}

#[cfg(test)]
mod poll_tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct FakeClock {
        now: Cell<Instant>,
        sleeps: RefCell<Vec<Duration>>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: Cell::new(Instant::now()),
                sleeps: RefCell::new(Vec::new()),
            }
        }

        fn sleeps(&self) -> Vec<Duration> {
            self.sleeps.borrow().clone()
        }
    }

    impl PollClock for FakeClock {
        fn now(&self) -> Instant {
            self.now.get()
        }

        fn sleep(&self, duration: Duration) {
            self.sleeps.borrow_mut().push(duration);
            self.now.set(self.now.get() + duration);
        }
    }

    fn us(value: u64) -> Duration {
        Duration::from_micros(value)
    }

    #[test]
    fn polling_backs_off_exponentially_and_never_sleeps_past_the_deadline() {
        let clock = FakeClock::new();
        let deadline = clock.now() + Duration::from_millis(30);
        let mut backoff = PollBackoff::default();
        let result =
            poll_until(&clock, deadline, &mut backoff, || Ok(None::<()>)).expect("poll result");
        assert!(result.is_none());
        assert_eq!(
            clock.sleeps(),
            vec![
                us(250),
                us(500),
                us(1_000),
                us(2_000),
                us(5_000),
                us(10_000),
                us(10_000),
                us(1_250),
            ]
        );
        assert_eq!(clock.now(), deadline);
    }

    #[test]
    fn short_child_finishes_without_a_ten_millisecond_floor_or_registration() {
        let clock = FakeClock::new();
        let started = clock.now();
        let mut polls = 0_u32;
        let mut registrations = 0_u32;
        let (status, registration) = wait_with_deferred_registration(
            &clock,
            &mut polls,
            started,
            Duration::from_secs(5),
            REGISTRATION_GRACE,
            |polls| {
                *polls += 1;
                Ok((*polls == 3).then_some("exited"))
            },
            |_| registrations += 1,
        )
        .expect("wait result");
        assert_eq!(status, Some("exited"));
        assert!(registration.is_none());
        assert_eq!(registrations, 0);
        assert_eq!(clock.now() - started, us(750));
    }

    #[test]
    fn long_child_is_registered_once_after_the_grace_window() {
        let clock = FakeClock::new();
        let started = clock.now();
        let registered_at = Cell::new(None);
        let exit_at = started + Duration::from_millis(60);
        let (status, registration) = wait_with_deferred_registration(
            &clock,
            &mut (),
            started,
            Duration::from_secs(5),
            REGISTRATION_GRACE,
            |_| Ok((clock.now() >= exit_at).then_some("exited")),
            |_| {
                assert!(registered_at.get().is_none(), "registered twice");
                registered_at.set(Some(clock.now()));
                "registration"
            },
        )
        .expect("wait result");
        assert_eq!(status, Some("exited"));
        assert_eq!(registration, Some("registration"));
        assert_eq!(registered_at.get(), Some(started + REGISTRATION_GRACE));
        assert!(clock.sleeps().iter().all(|sleep| *sleep <= POLL_INTERVAL));
    }

    #[test]
    fn timeout_inside_the_grace_window_skips_registration() {
        let clock = FakeClock::new();
        let started = clock.now();
        let mut registrations = 0_u32;
        let (status, registration) = wait_with_deferred_registration(
            &clock,
            &mut (),
            started,
            Duration::from_millis(5),
            REGISTRATION_GRACE,
            |_| Ok(None::<()>),
            |_| registrations += 1,
        )
        .expect("wait result");
        assert!(status.is_none());
        assert!(registration.is_none());
        assert_eq!(registrations, 0);
        assert_eq!(clock.now(), started + Duration::from_millis(5));
    }

    /// user never gets a dead child registered | Given a child whose exit is
    /// only visible on the poll that runs after the registration grace window |
    /// When the waiter re-checks liveness before writing the orphan registry |
    /// Then nothing is registered and the exit status is returned (#1050 review).
    #[test]
    fn child_exiting_at_the_grace_boundary_is_not_registered() {
        // The exit has to land *after* the last poll inside `poll_until` and on
        // the pre-register check, otherwise this only re-tests the grace loop.
        // Discover the loop's poll count instead of hardcoding it, so a backoff
        // change keeps the test pointed at the same window.
        let grace_polls = {
            let probe = FakeClock::new();
            let started = probe.now();
            let mut backoff = PollBackoff::default();
            let mut polls = 0_u32;
            poll_until(&probe, started + REGISTRATION_GRACE, &mut backoff, || {
                polls += 1;
                Ok(None::<u8>)
            })
            .expect("probe poll");
            polls
        };
        assert!(grace_polls > 0, "the grace window must poll at least once");

        let clock = FakeClock::new();
        let started = clock.now();
        let mut polls = 0_u32;
        let mut registrations = 0_u32;
        let (status, registration) = wait_with_deferred_registration(
            &clock,
            &mut (),
            started,
            Duration::from_secs(5),
            REGISTRATION_GRACE,
            |_| {
                polls += 1;
                if polls > grace_polls {
                    Ok(Some(7_u8))
                } else {
                    Ok(None)
                }
            },
            |_| registrations += 1,
        )
        .expect("wait result");
        assert_eq!(
            polls,
            grace_polls + 1,
            "the exit must be observed by the pre-register liveness poll"
        );
        assert_eq!(status, Some(7));
        assert!(
            registration.is_none(),
            "a child that exited before registration must not be recorded"
        );
        assert_eq!(registrations, 0);
    }

    #[test]
    fn timeout_after_registration_keeps_the_registration_for_cleanup() {
        let clock = FakeClock::new();
        let started = clock.now();
        let (status, registration) = wait_with_deferred_registration(
            &clock,
            &mut (),
            started,
            Duration::from_millis(100),
            REGISTRATION_GRACE,
            |_| Ok(None::<()>),
            |_| "registration",
        )
        .expect("wait result");
        assert!(status.is_none());
        assert_eq!(registration, Some("registration"));
        assert_eq!(clock.now(), started + Duration::from_millis(100));
    }

    #[test]
    fn poll_error_is_returned_without_registering() {
        let clock = FakeClock::new();
        let started = clock.now();
        let mut registrations = 0_u32;
        let error = wait_with_deferred_registration(
            &clock,
            &mut (),
            started,
            Duration::from_secs(1),
            REGISTRATION_GRACE,
            |_| Err::<Option<()>, _>(std::io::Error::other("wait failed")),
            |_| registrations += 1,
        )
        .expect_err("poll error must propagate");
        assert_eq!(error.to_string(), "wait failed");
        assert_eq!(registrations, 0);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn bounded_output_captures_without_leaking_pipe_backpressure() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf out; printf err >&2"]);
        let output = run_bounded_output(&mut command, Duration::from_secs(1), 1024)
            .unwrap()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
        assert!(!output.truncated);
    }

    #[test]
    fn timeout_terminates_and_reaps_child_group() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id();

        let status = wait_bounded(&mut child, Duration::from_millis(20)).unwrap();
        assert!(status.is_none());
        assert!(unsafe { libc::kill(pid as i32, 0) } != 0);
    }

    fn run_counting_registrations(
        script: &str,
        timeout: Duration,
        grace: Duration,
    ) -> (Option<BoundedOutput>, Vec<u32>) {
        let registered = std::cell::RefCell::new(Vec::new());
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        let output = run_bounded_output_inner(
            &mut command,
            timeout,
            1024,
            Some((grace, |child: &Child| {
                registered.borrow_mut().push(child.id());
            })),
        )
        .unwrap();
        (output, registered.into_inner())
    }

    #[test]
    fn transient_child_exiting_within_grace_is_never_registered() {
        let (output, registered) = run_counting_registrations(
            "printf ok",
            Duration::from_secs(10),
            Duration::from_secs(5),
        );
        let output = output.expect("short child must finish");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"ok");
        assert!(registered.is_empty(), "registered={registered:?}");
    }

    #[test]
    fn long_child_is_registered_while_running_and_output_is_kept() {
        let (output, registered) = run_counting_registrations(
            "sleep 0.3; printf done",
            Duration::from_secs(10),
            Duration::from_millis(25),
        );
        let output = output.expect("long child must finish before timeout");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"done");
        assert_eq!(registered.len(), 1, "registered={registered:?}");
    }

    #[test]
    fn registered_child_that_times_out_is_terminated_with_its_group() {
        let (output, registered) = run_counting_registrations(
            "sleep 30 & wait",
            Duration::from_millis(200),
            Duration::from_millis(25),
        );
        assert!(output.is_none());
        let [pid] = registered[..] else {
            panic!("registered={registered:?}");
        };
        // The orphaned grandchild is reaped by init asynchronously.
        let group_gone = (0..100).any(|_| {
            let gone = unsafe { libc::kill(-(pid as i32), 0) } != 0;
            if !gone {
                thread::sleep(Duration::from_millis(10));
            }
            gone
        });
        assert!(group_gone, "process group {pid} still has members");
    }
}
