use crate::cli::HerdrSupervisorCommand;
#[cfg(target_os = "macos")]
use crate::paths::RuntimePaths;
#[cfg(any(target_os = "macos", test))]
use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "macos", test))]
use serde_json::Value;
use serde_json::json;
use std::process::ExitCode;

pub(crate) const LABEL: &str = crate::instance::DEFAULT_HERDR_SUPERVISOR_LABEL;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct InstallState {
    pub(crate) present: bool,
    pub(crate) loaded: bool,
}

pub(crate) fn run(command: HerdrSupervisorCommand) -> Result<ExitCode, String> {
    #[cfg(target_os = "macos")]
    {
        platform::run(command)
    }
    #[cfg(not(target_os = "macos"))]
    {
        match command {
            HerdrSupervisorCommand::Status => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "supported": false,
                        "platform": std::env::consts::OS,
                        "label": LABEL,
                    }))
                    .map_err(|error| error.to_string())?
                );
                Ok(ExitCode::SUCCESS)
            }
            _ => Err("Herdr dependency supervisor is currently macOS-only".to_owned()),
        }
    }
}

pub(crate) fn ensure_installed_for_service() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if RuntimePaths::discover()?.instance.is_named() {
            return Ok(());
        }
        platform::ensure_installed()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

pub(crate) fn capture_install_state_for_service() -> Result<InstallState, String> {
    #[cfg(target_os = "macos")]
    {
        if RuntimePaths::discover()?.instance.is_named() {
            return Ok(InstallState::default());
        }
        platform::install_state()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(InstallState::default())
    }
}

pub(crate) fn preflight_install_for_service() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if RuntimePaths::discover()?.instance.is_named() {
            return Ok(());
        }
        platform::preflight_install()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

pub(crate) fn restore_install_state_for_service(state: InstallState) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if RuntimePaths::discover()?.instance.is_named() {
            return Ok(());
        }
        platform::restore_install_state(state)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = state;
        Ok(())
    }
}

pub(crate) fn runtime_binary_supports_supervisor(binary: &std::path::Path) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        platform::binary_supports_supervisor(binary)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = binary;
        Ok(false)
    }
}

pub(crate) fn reconcile_after_service_rollback() -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        if RuntimePaths::discover()?.instance.is_named() {
            return Ok(false);
        }
        platform::reconcile_after_service_rollback()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(false)
    }
}

pub(crate) fn doctor_line() -> String {
    #[cfg(target_os = "macos")]
    {
        platform::doctor_line()
    }
    #[cfg(not(target_os = "macos"))]
    {
        format!(
            "LAYER herdr-supervisor not-applicable platform={}",
            std::env::consts::OS
        )
    }
}

pub(crate) fn remove_for_service() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if RuntimePaths::discover()?.instance.is_named() {
            return Ok(());
        }
        platform::uninstall(false).map(|_| ())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SupervisorState {
    schema_version: u32,
    desired_running: bool,
    health_state: String,
    last_reason: Option<String>,
    failure_streak: u32,
    attempts_total: u64,
    last_transition_at_ms: u64,
    last_attempt_at_ms: Option<u64>,
    next_attempt_at_ms: Option<u64>,
    owned_pid: Option<u32>,
}

#[cfg(any(target_os = "macos", test))]
impl Default for SupervisorState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            desired_running: true,
            health_state: "unknown".to_owned(),
            last_reason: None,
            failure_streak: 0,
            attempts_total: 0,
            last_transition_at_ms: now_ms(),
            last_attempt_at_ms: None,
            next_attempt_at_ms: None,
            owned_pid: None,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
impl SupervisorState {
    fn transition(&mut self, state: &str, reason: Option<String>) {
        if self.health_state != state || self.last_reason != reason {
            self.last_transition_at_ms = now_ms();
        }
        self.health_state = state.to_owned();
        self.last_reason = reason;
    }

    fn as_json(&self) -> Value {
        json!({
            "schema_version": self.schema_version,
            "desired_running": self.desired_running,
            "health_state": self.health_state,
            "last_reason": self.last_reason,
            "failure_streak": self.failure_streak,
            "attempts_total": self.attempts_total,
            "last_transition_at_ms": self.last_transition_at_ms,
            "last_attempt_at_ms": self.last_attempt_at_ms,
            "next_attempt_at_ms": self.next_attempt_at_ms,
            "owned_pid": self.owned_pid,
        })
    }
}

#[cfg(any(target_os = "macos", test))]
fn recovery_delay_ms(failure_streak: u32) -> u64 {
    const DELAYS: &[u64] = &[1_000, 2_000, 5_000, 15_000, 30_000, 60_000];
    DELAYS[(failure_streak as usize).min(DELAYS.len() - 1)]
}

#[cfg(any(target_os = "macos", test))]
fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use crate::child_process;
    use crate::herdr::HerdrClient;
    use plist::{Dictionary, Value as PlistValue};
    use std::env;
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    const DAEMON_POLL: Duration = Duration::from_secs(1);
    const LAUNCHD_THROTTLE_SECONDS: i64 = 10;
    const RPC_TIMEOUT: Duration = Duration::from_millis(1500);
    const STARTUP_BUDGET: Duration = Duration::from_secs(12);
    const STARTUP_POLL: Duration = Duration::from_millis(250);
    const RESTORE_BLOCKED_COOLDOWN_MS: u64 = 5 * 60 * 1000;
    const OWNED_UNHEALTHY_RECYCLE_THRESHOLD: u32 = 3;

    struct SupervisorPaths {
        runtime: RuntimePaths,
        home: PathBuf,
        plist: PathBuf,
        state: PathBuf,
        lock: PathBuf,
        log: PathBuf,
        herdr_log: PathBuf,
        current_binary: PathBuf,
    }

    impl SupervisorPaths {
        fn discover() -> Result<Self, String> {
            let runtime = RuntimePaths::discover()?;
            if runtime.instance.is_named() {
                return Err(
                    "Herdr dependency supervisor is owned only by the default production instance"
                        .to_owned(),
                );
            }
            let home = env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| "cannot determine HOME for Herdr supervisor".to_owned())?;
            let current_binary = runtime.config_dir.join("runtime/current/herdr-mcp");
            Ok(Self {
                plist: home
                    .join("Library/LaunchAgents")
                    .join(format!("{LABEL}.plist")),
                state: runtime.config_dir.join("herdr-supervisor-state.json"),
                lock: runtime.config_dir.join("herdr-supervisor.lock"),
                log: runtime.config_dir.join("herdr-supervisor.log"),
                herdr_log: runtime.config_dir.join("herdr-supervisor-herdr.log"),
                runtime,
                home,
                current_binary,
            })
        }

        fn socket(&self) -> Result<&Path, String> {
            self.runtime
                .herdr_socket
                .as_deref()
                .ok_or_else(|| "Herdr supervisor requires a local Unix socket".to_owned())
        }
    }

    struct RecoveryLock(File);

    impl RecoveryLock {
        fn acquire(path: &Path) -> Result<Option<Self>, String> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path)
                .map_err(|error| format!("cannot open supervisor lock: {error}"))?;
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result == 0 {
                Ok(Some(Self(file)))
            } else if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(format!(
                    "cannot lock Herdr supervisor: {}",
                    std::io::Error::last_os_error()
                ))
            }
        }
    }

    impl Drop for RecoveryLock {
        fn drop(&mut self) {
            let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
    }

    #[derive(Debug)]
    struct HealthProbe {
        state: &'static str,
        reason: Option<String>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum InstallDisposition {
        NoopLoaded,
        WritePlist,
    }

    pub(super) fn install_disposition(
        present: bool,
        owned: bool,
        loaded: bool,
        config_matches: bool,
    ) -> Result<InstallDisposition, String> {
        if present && !owned {
            return Err("existing Herdr supervisor plist is not owned by herdr-mcp".to_owned());
        }
        if loaded {
            if !present {
                return Err(
                    "Herdr supervisor is loaded without its owned plist; refusing live replacement"
                        .to_owned(),
                );
            }
            if !config_matches {
                return Err(
                    "loaded Herdr supervisor configuration differs from the requested configuration; refusing bootout because it may own a live Herdr child"
                        .to_owned(),
                );
            }
            return Ok(InstallDisposition::NoopLoaded);
        }
        Ok(InstallDisposition::WritePlist)
    }

    struct ReconcileResult {
        value: Value,
        child: Option<Child>,
    }

    impl ReconcileResult {
        fn state(state: &SupervisorState) -> Self {
            Self {
                value: state.as_json(),
                child: None,
            }
        }
    }

    pub(super) fn doctor_line() -> String {
        let paths = match SupervisorPaths::discover() {
            Ok(paths) => paths,
            Err(error) => return format!("LAYER herdr-supervisor unavailable reason={error}"),
        };
        let present = paths.plist.exists();
        let loaded = is_loaded().unwrap_or(false);
        let state = load_state(&paths).unwrap_or_default();
        format!(
            "LAYER herdr-supervisor owned present={} loaded={} desired_running={} state={} failures={} attempts={}",
            present,
            loaded,
            state.desired_running,
            state.health_state,
            state.failure_streak,
            state.attempts_total
        )
    }

    pub(super) fn run(command: HerdrSupervisorCommand) -> Result<ExitCode, String> {
        match command {
            HerdrSupervisorCommand::Status => {
                let paths = SupervisorPaths::discover()?;
                let state = load_state(&paths)?;
                let loaded = is_loaded()?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "supported": true,
                        "label": LABEL,
                        "plist": paths.plist,
                        "present": paths.plist.exists(),
                        "loaded": loaded,
                        "socket": paths.socket()?,
                        "state": state.as_json(),
                    }))
                    .map_err(|error| error.to_string())?
                );
                Ok(ExitCode::SUCCESS)
            }
            HerdrSupervisorCommand::Install => {
                ensure_installed()?;
                Ok(ExitCode::SUCCESS)
            }
            HerdrSupervisorCommand::Uninstall => {
                uninstall(true)?;
                Ok(ExitCode::SUCCESS)
            }
            HerdrSupervisorCommand::Enable => {
                let paths = SupervisorPaths::discover()?;
                let mut state = load_state(&paths)?;
                state.desired_running = true;
                state.failure_streak = 0;
                state.next_attempt_at_ms = None;
                state.transition("unknown", Some("enabled_by_user".to_owned()));
                save_state(&paths, &state)?;
                Ok(ExitCode::SUCCESS)
            }
            HerdrSupervisorCommand::Disable => {
                let paths = SupervisorPaths::discover()?;
                let mut state = load_state(&paths)?;
                state.desired_running = false;
                state.next_attempt_at_ms = None;
                state.transition("user_stopped", Some("disabled_by_user".to_owned()));
                save_state(&paths, &state)?;
                Ok(ExitCode::SUCCESS)
            }
            HerdrSupervisorCommand::Start => {
                let paths = SupervisorPaths::discover()?;
                let mut state = load_state(&paths)?;
                state.desired_running = true;
                state.failure_streak = 0;
                state.next_attempt_at_ms = None;
                state.transition("unknown", Some("start_requested_by_user".to_owned()));
                save_state(&paths, &state)?;
                Ok(ExitCode::SUCCESS)
            }
            HerdrSupervisorCommand::Stop => {
                if env::var_os("HERDR_MCP_EXEC_ID").is_some() {
                    return Err("Herdr supervisor stop cannot run inside a managed herdr_exec session; run it from an independent terminal so stopping Herdr cannot sever its own control path".to_owned());
                }
                let paths = SupervisorPaths::discover()?;
                let mut state = load_state(&paths)?;
                state.desired_running = false;
                state.next_attempt_at_ms = None;
                state.transition("user_stopped", Some("stop_requested_by_user".to_owned()));
                save_state(&paths, &state)?;
                stop_server_explicit(&paths)?;
                Ok(ExitCode::SUCCESS)
            }
            HerdrSupervisorCommand::Run => {
                run_daemon()?;
                Ok(ExitCode::SUCCESS)
            }
            HerdrSupervisorCommand::RunOnce => {
                run_once()?;
                Ok(ExitCode::SUCCESS)
            }
        }
    }

    pub(super) fn ensure_installed() -> Result<(), String> {
        ensure_plist(true)
    }

    pub(super) fn preflight_install() -> Result<(), String> {
        let paths = SupervisorPaths::discover()?;
        let desired = encode_plist(&paths)?;
        let present = paths.plist.exists();
        let owned = !present || owned_plist(&paths)?;
        let loaded = is_loaded()?;
        let config_matches = present
            && fs::read(&paths.plist)
                .map(|bytes| bytes == desired)
                .unwrap_or(false);
        install_disposition(present, owned, loaded, config_matches).map(|_| ())
    }

    pub(super) fn install_state() -> Result<InstallState, String> {
        let paths = SupervisorPaths::discover()?;
        let present = paths.plist.exists();
        let loaded = is_loaded()?;
        if present && !owned_plist(&paths)? {
            return Err(format!(
                "existing {} is not owned by herdr-mcp; refusing lifecycle mutation",
                paths.plist.display()
            ));
        }
        if loaded && !present {
            return Err(
                "Herdr supervisor is loaded without its owned plist; ownership cannot be proven"
                    .to_owned(),
            );
        }
        Ok(InstallState { present, loaded })
    }

    pub(super) fn restore_install_state(state: InstallState) -> Result<(), String> {
        match (state.present, state.loaded) {
            (false, false) => uninstall(false).map(|_| ()),
            (true, false) => ensure_plist(false),
            (true, true) => ensure_plist(true),
            (false, true) => Err(
                "cannot restore an impossible Herdr supervisor state: loaded without plist"
                    .to_owned(),
            ),
        }
    }

    pub(super) fn reconcile_after_service_rollback() -> Result<bool, String> {
        if runtime_supports_supervisor()? {
            ensure_installed()?;
            Ok(true)
        } else {
            uninstall(false)?;
            Ok(false)
        }
    }

    fn ensure_plist(load: bool) -> Result<(), String> {
        let paths = SupervisorPaths::discover()?;
        fs::create_dir_all(&paths.runtime.config_dir)
            .map_err(|error| format!("cannot create supervisor config dir: {error}"))?;
        fs::create_dir_all(
            paths
                .plist
                .parent()
                .ok_or_else(|| "supervisor plist has no parent".to_owned())?,
        )
        .map_err(|error| format!("cannot create LaunchAgents directory: {error}"))?;
        if !paths.current_binary.exists() {
            return Err(format!(
                "cannot install Herdr supervisor before runtime/current exists: {}",
                paths.current_binary.display()
            ));
        }
        let desired = encode_plist(&paths)?;
        let present = paths.plist.exists();
        let owned = !present || owned_plist(&paths)?;
        let loaded = is_loaded()?;
        let config_matches = present
            && fs::read(&paths.plist)
                .map(|bytes| bytes == desired)
                .unwrap_or(false);
        if install_disposition(present, owned, loaded, config_matches)?
            == InstallDisposition::NoopLoaded
        {
            return Ok(());
        }
        if !paths.state.exists() {
            save_state(&paths, &SupervisorState::default())?;
        }
        atomic_write(&paths.plist, &desired, 0o600)?;
        if load {
            launchctl(&[
                "bootstrap",
                &launch_domain(),
                paths
                    .plist
                    .to_str()
                    .ok_or_else(|| "supervisor plist path is not UTF-8".to_owned())?,
            ])?;
        }
        Ok(())
    }

    fn runtime_supports_supervisor() -> Result<bool, String> {
        let paths = SupervisorPaths::discover()?;
        if !paths.current_binary.exists() {
            return Ok(false);
        }
        binary_supports_supervisor(&paths.current_binary)
    }

    pub(super) fn binary_supports_supervisor(binary: &Path) -> Result<bool, String> {
        let mut command = Command::new(binary);
        command.arg("--help");
        let Some(output) =
            child_process::run_bounded_output(&mut command, Duration::from_secs(2), 128 * 1024)
                .map_err(|error| format!("cannot probe runtime capabilities: {error}"))?
        else {
            return Err("runtime capability probe timed out after 2000ms".to_owned());
        };
        if !output.status.success() || output.truncated {
            return Err(format!(
                "runtime capability probe failed with status {}",
                output.status
            ));
        }
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok(help_declares_supervisor_command(&text))
    }

    pub(super) fn help_declares_supervisor_command(text: &str) -> bool {
        text.lines().any(|line| {
            line.trim_start()
                .starts_with("herdr-mcp herdr-supervisor <")
        })
    }

    pub(super) fn uninstall(user_requested: bool) -> Result<Value, String> {
        let paths = SupervisorPaths::discover()?;
        let present = paths.plist.exists();
        let loaded = is_loaded()?;
        if present && !owned_plist(&paths)? {
            return Err(format!(
                "existing {} is not owned by herdr-mcp; refusing removal",
                paths.plist.display()
            ));
        }
        if loaded && !present {
            return Err(
                "Herdr supervisor is loaded without its owned plist; refusing unverified bootout"
                    .to_owned(),
            );
        }
        if loaded {
            launchctl(&["bootout", &launch_domain_job()])?;
        }
        if paths.plist.exists() {
            fs::remove_file(&paths.plist)
                .map_err(|error| format!("cannot remove supervisor plist: {error}"))?;
        }
        let mut state = load_state(&paths)?;
        if user_requested {
            state.desired_running = false;
            state.transition(
                "user_stopped",
                Some("supervisor_uninstalled_by_user".to_owned()),
            );
            save_state(&paths, &state)?;
        }
        Ok(json!({"ok": true, "label": LABEL, "removed": true}))
    }

    fn run_once() -> Result<Value, String> {
        let paths = SupervisorPaths::discover()?;
        let Some(_lock) = RecoveryLock::acquire(&paths.lock)? else {
            return Ok(json!({"ok": true, "state": "recovery_in_progress"}));
        };
        let result = reconcile_once(&paths)?;
        Ok(result.value)
    }

    fn run_daemon() -> Result<(), String> {
        let paths = SupervisorPaths::discover()?;
        let mut owned_child: Option<Child> = None;
        loop {
            if let Some(child) = owned_child.as_mut()
                && let Some(status) = child
                    .try_wait()
                    .map_err(|error| format!("cannot inspect supervised Herdr child: {error}"))?
            {
                let mut state = load_state(&paths)?;
                state.owned_pid = None;
                if status.success() {
                    if state.desired_running {
                        state.desired_running = false;
                        state.next_attempt_at_ms = None;
                        state.transition(
                            "user_stopped",
                            Some("supervised Herdr exited cleanly".to_owned()),
                        );
                    }
                } else if state.desired_running {
                    state.failure_streak = state.failure_streak.saturating_add(1);
                    state.next_attempt_at_ms =
                        Some(now_ms() + recovery_delay_ms(state.failure_streak));
                    state.transition(
                        "server_not_running",
                        Some(format!("supervised Herdr exited unexpectedly: {status}")),
                    );
                }
                save_state(&paths, &state)?;
                owned_child = None;
            }

            if !load_state(&paths)?.desired_running {
                thread::sleep(DAEMON_POLL);
                continue;
            }

            if let Some(_lock) = RecoveryLock::acquire(&paths.lock)? {
                let result = reconcile_once(&paths)?;
                if owned_child.is_none() {
                    owned_child = result.child;
                }
            }

            if let Some(child) = owned_child.as_mut() {
                let mut state = load_state(&paths)?;
                let recycle_owned = state.desired_running
                    && state.failure_streak >= OWNED_UNHEALTHY_RECYCLE_THRESHOLD
                    && matches!(
                        state.health_state.as_str(),
                        "ping_timeout"
                            | "ping_unavailable"
                            | "snapshot_timeout"
                            | "snapshot_unavailable"
                            | "server_not_running"
                    );
                if recycle_owned {
                    let pid = child.id();
                    child_process::terminate_and_reap(child);
                    owned_child = None;
                    state.owned_pid = None;
                    state.next_attempt_at_ms =
                        Some(now_ms() + recovery_delay_ms(state.failure_streak));
                    state.transition(
                        "server_not_running",
                        Some(format!(
                            "recycled unhealthy supervisor-owned Herdr pid={pid} after {} failures",
                            state.failure_streak
                        )),
                    );
                    save_state(&paths, &state)?;
                }
            }
            thread::sleep(DAEMON_POLL);
        }
    }

    fn reconcile_once(paths: &SupervisorPaths) -> Result<ReconcileResult, String> {
        let mut state = load_state(paths)?;
        if state.owned_pid.is_some_and(|pid| !process_alive(pid)) {
            state.owned_pid = None;
        }
        let probe = probe_herdr(paths.socket()?);
        if probe.state == "healthy" {
            state.failure_streak = 0;
            state.next_attempt_at_ms = None;
            state.transition("healthy", None);
            save_state(paths, &state)?;
            return Ok(ReconcileResult::state(&state));
        }
        if !state.desired_running {
            state.transition("user_stopped", probe.reason);
            save_state(paths, &state)?;
            return Ok(ReconcileResult::state(&state));
        }
        if state.health_state == "session_restore_blocked" {
            state.last_reason = probe.reason;
            save_state(paths, &state)?;
            return Ok(ReconcileResult::state(&state));
        }
        let now = now_ms();
        if state
            .next_attempt_at_ms
            .is_some_and(|deadline| now < deadline)
        {
            state.transition("recovery_cooldown", probe.reason);
            save_state(paths, &state)?;
            return Ok(ReconcileResult::state(&state));
        }

        if state.owned_pid.is_some_and(process_alive) {
            state.failure_streak = state.failure_streak.saturating_add(1);
            state.next_attempt_at_ms = Some(now + recovery_delay_ms(state.failure_streak));
            state.transition(probe.state, probe.reason);
            save_state(paths, &state)?;
            return Ok(ReconcileResult::state(&state));
        }

        let herdr_bin = discover_herdr_binary(paths)?;
        let server_running = server_running_status(&herdr_bin, paths.socket()?)?;
        if matches!(probe.state, "socket_missing" | "server_not_running")
            && server_running == Some(false)
        {
            if probe.state == "server_not_running" {
                remove_stale_socket(paths.socket()?)?;
            }
            return start_server(paths, &herdr_bin, &mut state);
        }
        if probe.state == "socket_missing" && server_running == Some(true) {
            state.failure_streak = state.failure_streak.saturating_add(1);
            state.next_attempt_at_ms = Some(now + recovery_delay_ms(state.failure_streak));
            state.transition(
                "socket_missing",
                Some(
                    "Herdr reports running but its socket is not ready; refusing duplicate start"
                        .to_owned(),
                ),
            );
            save_state(paths, &state)?;
            return Ok(ReconcileResult::state(&state));
        }
        if probe.state == "socket_missing" && server_running.is_none() {
            state.failure_streak = state.failure_streak.saturating_add(1);
            state.next_attempt_at_ms = Some(now + recovery_delay_ms(state.failure_streak));
            state.transition(
                "server_not_running",
                Some(
                    "Herdr server status is unavailable; refusing an unverified duplicate start"
                        .to_owned(),
                ),
            );
            save_state(paths, &state)?;
            return Ok(ReconcileResult::state(&state));
        }

        state.failure_streak = state.failure_streak.saturating_add(1);
        state.next_attempt_at_ms = Some(now + recovery_delay_ms(state.failure_streak));
        state.transition(probe.state, probe.reason);
        save_state(paths, &state)?;
        Ok(ReconcileResult::state(&state))
    }

    fn start_server(
        paths: &SupervisorPaths,
        herdr_bin: &Path,
        state: &mut SupervisorState,
    ) -> Result<ReconcileResult, String> {
        state.attempts_total = state.attempts_total.saturating_add(1);
        state.last_attempt_at_ms = Some(now_ms());
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&paths.herdr_log)
            .map_err(|error| format!("cannot open supervisor Herdr log: {error}"))?;
        let err_log = log
            .try_clone()
            .map_err(|error| format!("cannot clone supervisor Herdr log: {error}"))?;
        let mut command = Command::new(herdr_bin);
        command
            .arg("server")
            .env("HERDR_SOCKET_PATH", paths.socket()?)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err_log));
        child_process::configure_process_group(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot start Herdr server: {error}"))?;
        state.owned_pid = Some(child.id());
        state.transition("starting", Some("supervisor_started_herdr".to_owned()));
        save_state(paths, state)?;

        let started = Instant::now();
        let mut last_probe = HealthProbe {
            state: "socket_missing",
            reason: None,
        };
        while started.elapsed() < STARTUP_BUDGET {
            if let Ok(Some(status)) = child.try_wait() {
                state.owned_pid = None;
                state.failure_streak = state.failure_streak.saturating_add(1);
                state.next_attempt_at_ms = Some(now_ms() + recovery_delay_ms(state.failure_streak));
                state.transition(
                    "server_not_running",
                    Some(format!("supervised Herdr exited with {status}")),
                );
                save_state(paths, state)?;
                return Ok(ReconcileResult::state(state));
            }
            last_probe = probe_herdr(paths.socket()?);
            if last_probe.state == "healthy" {
                state.failure_streak = 0;
                state.next_attempt_at_ms = None;
                state.transition("healthy", None);
                save_state(paths, state)?;
                return Ok(ReconcileResult {
                    value: state.as_json(),
                    child: Some(child),
                });
            }
            thread::sleep(STARTUP_POLL);
        }

        state.failure_streak = state.failure_streak.saturating_add(1);
        if matches!(
            last_probe.state,
            "snapshot_timeout" | "snapshot_unavailable"
        ) {
            state.next_attempt_at_ms = Some(now_ms() + RESTORE_BLOCKED_COOLDOWN_MS);
            state.transition(
                "session_restore_blocked",
                Some(format!(
                    "Herdr started but session snapshot never became ready: {}",
                    last_probe
                        .reason
                        .unwrap_or_else(|| last_probe.state.to_owned())
                )),
            );
        } else {
            state.next_attempt_at_ms = Some(now_ms() + recovery_delay_ms(state.failure_streak));
            state.transition(last_probe.state, last_probe.reason);
        }
        save_state(paths, state)?;
        Ok(ReconcileResult {
            value: state.as_json(),
            child: Some(child),
        })
    }

    fn stop_server_explicit(paths: &SupervisorPaths) -> Result<(), String> {
        let herdr_bin = discover_herdr_binary(paths)?;
        let mut command = Command::new(herdr_bin);
        command
            .args(["server", "stop"])
            .env("HERDR_SOCKET_PATH", paths.socket()?);
        match child_process::run_bounded_output(&mut command, Duration::from_secs(3), 16 * 1024)
            .map_err(|error| format!("cannot run Herdr server stop: {error}"))?
        {
            Some(output) if output.status.success() => Ok(()),
            Some(output) => Err(format!(
                "Herdr server stop failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            None => Err("Herdr server stop timed out after 3000ms".to_owned()),
        }
    }

    fn probe_herdr(socket: &Path) -> HealthProbe {
        if !socket.exists() {
            return HealthProbe {
                state: "socket_missing",
                reason: Some(format!("{}: No such file or directory", socket.display())),
            };
        }
        let client = HerdrClient::new(socket);
        if let Err(error) = client.call_with_timeout("ping", json!({}), RPC_TIMEOUT) {
            return HealthProbe {
                state: if error.code == "timeout" {
                    "ping_timeout"
                } else if error.code == "socket_missing" {
                    "socket_missing"
                } else if error.code == "connection_refused" {
                    "server_not_running"
                } else {
                    "ping_unavailable"
                },
                reason: Some(error.to_string()),
            };
        }
        match client.call_with_timeout("session.snapshot", json!({}), RPC_TIMEOUT) {
            Ok(_) => HealthProbe {
                state: "healthy",
                reason: None,
            },
            Err(error) => HealthProbe {
                state: if error.code == "timeout" {
                    "snapshot_timeout"
                } else {
                    "snapshot_unavailable"
                },
                reason: Some(error.to_string()),
            },
        }
    }

    fn remove_stale_socket(socket: &Path) -> Result<(), String> {
        let metadata = fs::symlink_metadata(socket)
            .map_err(|error| format!("cannot inspect stale Herdr socket: {error}"))?;
        if !metadata.file_type().is_socket() {
            return Err(format!(
                "refusing stale-socket cleanup because {} is not a Unix socket",
                socket.display()
            ));
        }
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(format!(
                "refusing stale-socket cleanup because {} is not owned by the current user",
                socket.display()
            ));
        }
        match std::os::unix::net::UnixStream::connect(socket) {
            Ok(_) => Err(format!(
                "refusing stale-socket cleanup because {} accepted a connection",
                socket.display()
            )),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                if error.kind() == std::io::ErrorKind::NotFound {
                    return Ok(());
                }
                fs::remove_file(socket).map_err(|remove_error| {
                    format!(
                        "cannot remove verified stale Herdr socket {}: {remove_error}",
                        socket.display()
                    )
                })
            }
            Err(error) => Err(format!(
                "refusing stale-socket cleanup for {} after unexpected connect error: {error}",
                socket.display()
            )),
        }
    }

    fn process_alive(pid: u32) -> bool {
        let result = unsafe { libc::kill(pid as i32, 0) };
        if result == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn server_running_status(herdr_bin: &Path, socket: &Path) -> Result<Option<bool>, String> {
        let mut command = Command::new(herdr_bin);
        command
            .args(["status", "server", "--json"])
            .env("HERDR_SOCKET_PATH", socket);
        let Some(output) =
            child_process::run_bounded_output(&mut command, Duration::from_secs(2), 32 * 1024)
                .map_err(|error| format!("cannot query Herdr server status: {error}"))?
        else {
            return Ok(None);
        };
        if !output.status.success() || output.truncated {
            return Ok(None);
        }
        let value: Value = match serde_json::from_slice(&output.stdout) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        Ok(value.get("running").and_then(Value::as_bool))
    }

    fn discover_herdr_binary(paths: &SupervisorPaths) -> Result<PathBuf, String> {
        let candidates = [
            env::var_os("HERDR_BIN").map(PathBuf::from),
            Some(paths.home.join(".local/bin/herdr")),
            Some(PathBuf::from("/opt/homebrew/bin/herdr")),
            Some(PathBuf::from("/usr/local/bin/herdr")),
        ];
        candidates
            .into_iter()
            .flatten()
            .find(|path| {
                path.metadata().is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .ok_or_else(|| {
                "cannot locate executable Herdr binary for dependency recovery".to_owned()
            })
    }

    fn load_state(paths: &SupervisorPaths) -> Result<SupervisorState, String> {
        match fs::read(&paths.state) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("cannot parse Herdr supervisor state: {error}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(SupervisorState::default())
            }
            Err(error) => Err(format!("cannot read Herdr supervisor state: {error}")),
        }
    }

    fn save_state(paths: &SupervisorPaths, state: &SupervisorState) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| format!("cannot encode Herdr supervisor state: {error}"))?;
        atomic_write(&paths.state, &bytes, 0o600)
    }

    fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent", path.display()))?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let tmp = parent.join(format!(
            ".{}.tmp-{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("state"),
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(mode)
            .open(&tmp)
            .map_err(|error| format!("cannot write {}: {error}", tmp.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("cannot write {}: {error}", tmp.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync {}: {error}", tmp.display()))?;
        fs::rename(&tmp, path)
            .map_err(|error| format!("cannot activate {}: {error}", path.display()))
    }

    fn encode_plist(paths: &SupervisorPaths) -> Result<Vec<u8>, String> {
        let mut dict = Dictionary::new();
        dict.insert("Label".to_owned(), PlistValue::String(LABEL.to_owned()));
        dict.insert(
            "ProgramArguments".to_owned(),
            PlistValue::Array(vec![
                PlistValue::String(paths.current_binary.to_string_lossy().into_owned()),
                PlistValue::String("herdr-supervisor".to_owned()),
                PlistValue::String("run".to_owned()),
            ]),
        );
        dict.insert("RunAtLoad".to_owned(), PlistValue::Boolean(true));
        dict.insert("KeepAlive".to_owned(), PlistValue::Boolean(true));
        dict.insert(
            "ThrottleInterval".to_owned(),
            PlistValue::Integer(LAUNCHD_THROTTLE_SECONDS.into()),
        );
        dict.insert(
            "ProcessType".to_owned(),
            PlistValue::String("Background".to_owned()),
        );
        dict.insert(
            "StandardOutPath".to_owned(),
            PlistValue::String(paths.log.to_string_lossy().into_owned()),
        );
        dict.insert(
            "StandardErrorPath".to_owned(),
            PlistValue::String(paths.log.to_string_lossy().into_owned()),
        );
        let mut env_dict = Dictionary::new();
        env_dict.insert(
            "HOME".to_owned(),
            PlistValue::String(paths.home.to_string_lossy().into_owned()),
        );
        env_dict.insert(
            "HERDR_MCP_CONFIG_DIR".to_owned(),
            PlistValue::String(paths.runtime.config_dir.to_string_lossy().into_owned()),
        );
        env_dict.insert(
            "HERDR_SOCKET_PATH".to_owned(),
            PlistValue::String(paths.socket()?.to_string_lossy().into_owned()),
        );
        env_dict.insert(
            "PATH".to_owned(),
            PlistValue::String(format!(
                "{}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
                paths.home.display()
            )),
        );
        dict.insert(
            "EnvironmentVariables".to_owned(),
            PlistValue::Dictionary(env_dict),
        );
        let mut bytes = Vec::new();
        plist::to_writer_xml(&mut bytes, &PlistValue::Dictionary(dict))
            .map_err(|error| format!("cannot encode Herdr supervisor plist: {error}"))?;
        Ok(bytes)
    }

    fn owned_plist(paths: &SupervisorPaths) -> Result<bool, String> {
        let value = PlistValue::from_file(&paths.plist)
            .map_err(|error| format!("cannot parse existing supervisor plist: {error}"))?;
        let Some(dict) = value.as_dictionary() else {
            return Ok(false);
        };
        if dict.get("Label").and_then(PlistValue::as_string) != Some(LABEL) {
            return Ok(false);
        }
        let Some(args) = dict.get("ProgramArguments").and_then(PlistValue::as_array) else {
            return Ok(false);
        };
        let args = args
            .iter()
            .filter_map(PlistValue::as_string)
            .collect::<Vec<_>>();
        Ok(args
            == [
                paths.current_binary.to_string_lossy().as_ref(),
                "herdr-supervisor",
                "run",
            ])
    }

    fn launch_domain() -> String {
        format!("gui/{}", unsafe { libc::geteuid() })
    }

    fn launch_domain_job() -> String {
        format!("{}/{}", launch_domain(), LABEL)
    }

    fn is_loaded() -> Result<bool, String> {
        let mut command = Command::new("/bin/launchctl");
        command.args(["print", &launch_domain_job()]);
        match child_process::run_bounded_output(&mut command, Duration::from_secs(2), 16 * 1024)
            .map_err(|error| format!("cannot inspect Herdr supervisor LaunchAgent: {error}"))?
        {
            Some(output) => Ok(output.status.success()),
            None => Err("launchctl print timed out after 2000ms".to_owned()),
        }
    }

    fn launchctl(args: &[&str]) -> Result<(), String> {
        let mut command = Command::new("/bin/launchctl");
        command.args(args);
        match child_process::run_bounded_output(&mut command, Duration::from_secs(5), 32 * 1024)
            .map_err(|error| format!("cannot run launchctl: {error}"))?
        {
            Some(output) if output.status.success() => Ok(()),
            Some(output) => Err(format!(
                "launchctl {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            None => Err(format!("launchctl {} timed out", args.join(" "))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_capability_requires_an_explicit_help_command() {
        assert!(platform::help_declares_supervisor_command(
            "Advanced:\n  herdr-mcp herdr-supervisor <install|status>\n"
        ));
        assert!(!platform::help_declares_supervisor_command(
            "This build documents herdr-supervisor recovery but has no command.\n"
        ));
    }

    #[test]
    fn loaded_supervisor_install_is_noop_only_when_configuration_matches() {
        use platform::{InstallDisposition, install_disposition};

        assert_eq!(
            install_disposition(true, true, true, true).unwrap(),
            InstallDisposition::NoopLoaded
        );
        assert!(install_disposition(true, true, true, false).is_err());
        assert!(install_disposition(false, true, true, false).is_err());
        assert!(install_disposition(true, false, false, false).is_err());
        assert_eq!(
            install_disposition(false, true, false, false).unwrap(),
            InstallDisposition::WritePlist
        );
    }

    #[test]
    fn recovery_backoff_is_bounded() {
        assert_eq!(recovery_delay_ms(0), 1_000);
        assert_eq!(recovery_delay_ms(1), 2_000);
        assert_eq!(recovery_delay_ms(2), 5_000);
        assert_eq!(recovery_delay_ms(5), 60_000);
        assert_eq!(recovery_delay_ms(100), 60_000);
    }

    #[test]
    fn user_stop_state_is_persistent_and_explicit() {
        let mut state = SupervisorState {
            desired_running: false,
            ..SupervisorState::default()
        };
        state.transition("user_stopped", Some("disabled_by_user".to_owned()));
        let encoded = serde_json::to_vec(&state).unwrap();
        let decoded: SupervisorState = serde_json::from_slice(&encoded).unwrap();
        assert!(!decoded.desired_running);
        assert_eq!(decoded.health_state, "user_stopped");
    }
}
