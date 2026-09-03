use std::io::Read;
use std::process::Stdio;
use std::process::{Child, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const TERMINATE_GRACE: Duration = Duration::from_millis(250);

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
    run_bounded_output_inner(command, timeout, max_bytes, true)
}

fn run_bounded_output_inner(
    command: &mut Command,
    timeout: Duration,
    max_bytes: usize,
    track: bool,
) -> std::io::Result<Option<BoundedOutput>> {
    let kind = command
        .get_program()
        .to_string_lossy()
        .rsplit('/')
        .next()
        .unwrap_or("child")
        .to_owned();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(command);
    let mut child = command.spawn()?;
    let _registration = track.then(|| register_owned_child(&kind, &child));
    let mut stdout = child.stdout.take().expect("piped stdout must be present");
    let mut stderr = child.stderr.take().expect("piped stderr must be present");
    let stdout_reader = thread::spawn(move || read_capped_and_drain(&mut stdout, max_bytes));
    let stderr_reader = thread::spawn(move || read_capped_and_drain(&mut stderr, max_bytes));
    let status = wait_bounded(&mut child, timeout)?;
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
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if started.elapsed() >= timeout {
            terminate_and_reap(child);
            return Ok(None);
        }
        thread::sleep(POLL_INTERVAL);
    }
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
        let _ = child.wait();
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
        let pid = child.id();
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
        let output =
            run_bounded_output_inner(&mut command, Duration::from_secs(1), 16 * 1024, false)
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
}
