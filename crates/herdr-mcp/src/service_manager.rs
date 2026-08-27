//! macOS service manager for the Rust local runtime.
//!
//! The production launchd label intentionally stays `dev.herdr-mcp.server` so
//! callers and the browser extension do not learn a second service identity.
//! A Rust install is content-addressed under `runtime/generations/` and launchd
//! points at the stable `runtime/current/herdr-mcp` path. Replacing an existing
//! Node service is explicit (`service install --adopt-node`) and transactional:
//! the old server/watchdog plists and generation pointer are restored if the
//! Rust service cannot bootstrap and pass `/health`.

use crate::cli::ServiceCommand;
use std::process::ExitCode;

pub fn run(command: ServiceCommand) -> Result<ExitCode, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = command;
        Err("service_manager_currently_requires_macos".to_owned())
    }

    #[cfg(target_os = "macos")]
    {
        macos::run(command)
    }
}

/// Read-only service ownership snapshot for `doctor`. Never mutates launchd.
pub fn doctor_status() -> Result<serde_json::Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        Ok(serde_json::json!({
            "ok": false,
            "implementation": "unsupported",
            "detail": "service manager currently requires macOS",
        }))
    }

    #[cfg(target_os = "macos")]
    {
        macos::doctor_status()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use crate::paths::RuntimePaths;
    use crate::state_store::{RuntimeGenerationRecord, ServiceRollbackRecord, StateStore};
    use crate::user_cli;
    use plist::{Dictionary, Value as PlistValue};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::env;
    use std::ffi::OsStr;
    use std::fs::{self, File, OpenOptions};
    use std::io::{Cursor, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
    use std::os::unix::process::CommandExt;
    use std::path::{Component, Path, PathBuf};
    use std::process::{Child, Command, Output, Stdio};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    const SERVICE_LABEL: &str = "dev.herdr-mcp.server";
    const WATCHDOG_LABEL: &str = "dev.herdr-mcp.watchdog";
    /// Periodic Rust-era health sidecar. Deliberately distinct from the legacy
    /// Node `dev.herdr-mcp.watchdog` identity reserved for adoption/rollback.
    const HEALTH_WATCHDOG_LABEL: &str = "dev.herdr-mcp.health-watchdog";
    const SERVICE_IMPL: &str = "rust-v1";
    const DEFAULT_PORT: u16 = 8772;
    const HEALTH_BUDGET: Duration = Duration::from_secs(10);
    const LAUNCHD_ABSENT_BUDGET: Duration = Duration::from_secs(2);
    const LAUNCHD_BOOTOUT_BUDGET: Duration = Duration::from_secs(10);
    const LAUNCHD_RECOVERY_BUDGET: Duration = Duration::from_secs(15);
    const GUARDIAN_HANDSHAKE_BUDGET: Duration = Duration::from_secs(5);
    const GUARDIAN_EXIT_BUDGET: Duration = Duration::from_secs(2);
    const GUARDIAN_MAX_LIFETIME: Duration = Duration::from_secs(300);
    const GUARDIAN_PARENT_FD: i32 = 198;
    const GUARDIAN_LOCK_FD: i32 = 199;
    const BOOTSTRAP_RETRY_DELAYS: [Duration; 4] = [
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_millis(1000),
        Duration::from_millis(2000),
    ];

    #[derive(Debug, Clone)]
    struct ServicePaths {
        home: PathBuf,
        config_dir: PathBuf,
        source_binary: PathBuf,
        runtime_root: PathBuf,
        generations_dir: PathBuf,
        current_link: PathBuf,
        current_binary: PathBuf,
        plist: PathBuf,
        watchdog_plist: PathBuf,
        health_watchdog_plist: PathBuf,
        backups_dir: PathBuf,
        guardians_dir: PathBuf,
        mutation_lock: PathBuf,
        extension_socket: PathBuf,
        herdr_socket: PathBuf,
        log_path: PathBuf,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ServiceKind {
        Missing,
        Rust,
        Node,
        Other,
    }

    fn service_kind_name(kind: &ServiceKind) -> &'static str {
        match kind {
            ServiceKind::Missing => "missing",
            ServiceKind::Rust => "rust",
            ServiceKind::Node => "node",
            ServiceKind::Other => "other",
        }
    }

    #[derive(Debug, Clone)]
    struct ServiceDescriptor {
        kind: ServiceKind,
        env: BTreeMap<String, String>,
    }

    #[derive(Debug, Clone)]
    struct PreparedGeneration {
        generation_id: String,
        sha256: String,
        binary: PathBuf,
    }

    #[derive(Debug)]
    struct RollbackState {
        server_plist: Option<Vec<u8>>,
        server_was_loaded: bool,
        watchdog_plist: Option<Vec<u8>>,
        watchdog_was_loaded: bool,
        previous_current: Option<PathBuf>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum GuardianMode {
        Install,
        Rollback,
    }

    impl GuardianMode {
        fn as_str(self) -> &'static str {
            match self {
                Self::Install => "install",
                Self::Rollback => "rollback",
            }
        }

        fn parse(value: &str) -> Result<Self, String> {
            match value {
                "install" => Ok(Self::Install),
                "rollback" => Ok(Self::Rollback),
                _ => Err(format!("invalid guardian mode {value}")),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct GuardianRecord {
        transaction_id: String,
        mode: GuardianMode,
        state: String,
        parent_pid: u32,
        created_at: i64,
        rollback_id: Option<String>,
        candidate_generation_id: Option<String>,
        server_plist_backup: Option<String>,
        watchdog_plist_backup: Option<String>,
        previous_current_target: Option<String>,
        server_was_loaded: bool,
        watchdog_was_loaded: bool,
        detail: Option<String>,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum GuardianDecision {
        Exit(&'static str),
        Recover,
        Refuse(String),
    }

    struct ServiceMutationLock {
        file: File,
    }

    struct GuardianHandle {
        paths: ServicePaths,
        transaction_id: String,
        signal: Option<File>,
        child: Child,
    }

    pub(super) fn run(command: ServiceCommand) -> Result<ExitCode, String> {
        if service_command_requires_independent_process(&command)
            && env::var_os("HERDR_MCP_EXEC_ID").is_some()
        {
            return Err(
                "service mutations cannot run inside a managed herdr_exec session; run the command from an independent terminal so restarting dev.herdr-mcp.server cannot terminate its own upgrade transaction"
                    .to_owned(),
            );
        }
        let paths = ServicePaths::discover()?;
        let mutation_lock = if service_command_requires_mutation_lock(&command) {
            Some(ServiceMutationLock::acquire(&paths)?)
        } else {
            None
        };
        let result = match command {
            ServiceCommand::Install { adopt_node } => install(
                &paths,
                adopt_node,
                mutation_lock
                    .as_ref()
                    .expect("install must hold the service mutation lock"),
            )?,
            ServiceCommand::Status => status(&paths)?,
            ServiceCommand::Start => start(&paths)?,
            ServiceCommand::Stop => stop(&paths)?,
            ServiceCommand::Restart => restart(&paths)?,
            ServiceCommand::Rollback => rollback(
                &paths,
                mutation_lock
                    .as_ref()
                    .expect("rollback must hold the service mutation lock"),
            )?,
            ServiceCommand::Uninstall => uninstall(&paths)?,
            ServiceCommand::Guardian {
                transaction_id,
                parent_pid,
            } => guardian(&paths, &transaction_id, parent_pid)?,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&result)
                .map_err(|error| format!("cannot encode service result: {error}"))?
        );
        Ok(if result.get("ok").and_then(Value::as_bool) == Some(true) {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        })
    }

    fn service_command_requires_independent_process(command: &ServiceCommand) -> bool {
        !matches!(command, ServiceCommand::Status)
    }

    fn service_command_requires_mutation_lock(command: &ServiceCommand) -> bool {
        !matches!(
            command,
            ServiceCommand::Status | ServiceCommand::Guardian { .. }
        )
    }

    impl ServicePaths {
        fn discover() -> Result<Self, String> {
            let home = env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| "cannot determine user home directory".to_owned())?;
            let runtime = RuntimePaths::discover()?;
            let source_binary = env::current_exe()
                .map_err(|error| format!("cannot locate current herdr-mcp binary: {error}"))?;
            let herdr_socket = runtime
                .herdr_socket
                .ok_or_else(|| "service manager requires a Herdr Unix socket path".to_owned())?;
            Ok(Self::for_values(
                home,
                runtime.config_dir,
                source_binary,
                herdr_socket,
            ))
        }

        fn for_values(
            home: PathBuf,
            config_dir: PathBuf,
            source_binary: PathBuf,
            herdr_socket: PathBuf,
        ) -> Self {
            let runtime_root = config_dir.join("runtime");
            let generations_dir = runtime_root.join("generations");
            let current_link = runtime_root.join("current");
            let current_binary = current_link.join("herdr-mcp");
            Self {
                plist: home
                    .join("Library")
                    .join("LaunchAgents")
                    .join(format!("{SERVICE_LABEL}.plist")),
                watchdog_plist: home
                    .join("Library")
                    .join("LaunchAgents")
                    .join(format!("{WATCHDOG_LABEL}.plist")),
                health_watchdog_plist: home
                    .join("Library")
                    .join("LaunchAgents")
                    .join(format!("{HEALTH_WATCHDOG_LABEL}.plist")),
                backups_dir: config_dir.join("backups"),
                guardians_dir: config_dir.join("guardians"),
                mutation_lock: config_dir.join("service-mutation.lock"),
                extension_socket: config_dir.join("extension.sock"),
                log_path: config_dir.join("server.log"),
                home,
                config_dir,
                source_binary,
                runtime_root,
                generations_dir,
                current_link,
                current_binary,
                herdr_socket,
            }
        }
    }

    impl ServiceMutationLock {
        fn acquire(paths: &ServicePaths) -> Result<Self, String> {
            ensure_secure_dir(&paths.config_dir)?;
            if let Ok(metadata) = fs::symlink_metadata(&paths.mutation_lock)
                && metadata.file_type().is_symlink()
            {
                return Err("service mutation lock must not be a symlink".to_owned());
            }
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&paths.mutation_lock)
                .map_err(|error| format!("cannot open service mutation lock: {error}"))?;
            fs::set_permissions(&paths.mutation_lock, fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("cannot secure service mutation lock: {error}"))?;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(format!(
                    "another service mutation is in progress: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self { file })
        }

        fn fd(&self) -> i32 {
            self.file.as_raw_fd()
        }
    }

    impl GuardianHandle {
        fn finish(&mut self, state: &str, detail: &str) -> Result<(), String> {
            if !transition_guardian_state(
                &self.paths,
                &self.transaction_id,
                &["watching"],
                state,
                Some(detail),
            )? {
                return Err(format!(
                    "guardian transaction {} was no longer watching during settlement",
                    self.transaction_id
                ));
            }
            self.signal.take();
            let deadline = Instant::now() + GUARDIAN_EXIT_BUDGET;
            loop {
                if self
                    .child
                    .try_wait()
                    .map_err(|error| format!("cannot inspect guardian child: {error}"))?
                    .is_some()
                {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    terminate_guardian_child(&mut self.child)?;
                    return Err(
                        "guardian child required forced termination after transaction settlement"
                            .to_owned(),
                    );
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
    }

    fn valid_guardian_transaction_id(value: &str) -> bool {
        (12..=96).contains(&value.len())
            && value.starts_with("gtx-")
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }

    fn new_guardian_transaction_id() -> Result<String, String> {
        let random = secure_token_hex()?;
        Ok(format!(
            "gtx-{}-{}-{}",
            now_ms_i64(),
            std::process::id(),
            &random[..12]
        ))
    }

    fn guardian_dir(paths: &ServicePaths, transaction_id: &str) -> Result<PathBuf, String> {
        if !valid_guardian_transaction_id(transaction_id) {
            return Err("invalid guardian transaction id".to_owned());
        }
        Ok(paths.guardians_dir.join(transaction_id))
    }

    fn guardian_record_path(paths: &ServicePaths, transaction_id: &str) -> Result<PathBuf, String> {
        Ok(guardian_dir(paths, transaction_id)?.join("transaction.json"))
    }

    fn guardian_binary_path(paths: &ServicePaths, transaction_id: &str) -> Result<PathBuf, String> {
        Ok(guardian_dir(paths, transaction_id)?.join("guardian-herdr-mcp"))
    }

    fn guardian_log_path(paths: &ServicePaths, transaction_id: &str) -> Result<PathBuf, String> {
        Ok(guardian_dir(paths, transaction_id)?.join("guardian.log"))
    }

    fn guardian_state_lock_path(
        paths: &ServicePaths,
        transaction_id: &str,
    ) -> Result<PathBuf, String> {
        Ok(guardian_dir(paths, transaction_id)?.join("transaction.lock"))
    }

    fn guardian_record_value(record: &GuardianRecord) -> Value {
        json!({
            "schema_version": 1,
            "transaction_id": record.transaction_id,
            "mode": record.mode.as_str(),
            "state": record.state,
            "parent_pid": record.parent_pid,
            "created_at": record.created_at,
            "rollback_id": record.rollback_id,
            "candidate_generation_id": record.candidate_generation_id,
            "server_plist_backup": record.server_plist_backup,
            "watchdog_plist_backup": record.watchdog_plist_backup,
            "previous_current_target": record.previous_current_target,
            "server_was_loaded": record.server_was_loaded,
            "watchdog_was_loaded": record.watchdog_was_loaded,
            "detail": record.detail,
        })
    }

    fn decode_guardian_record(value: &Value) -> Result<GuardianRecord, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "guardian transaction must be a JSON object".to_owned())?;
        let allowed = [
            "schema_version",
            "transaction_id",
            "mode",
            "state",
            "parent_pid",
            "created_at",
            "rollback_id",
            "candidate_generation_id",
            "server_plist_backup",
            "watchdog_plist_backup",
            "previous_current_target",
            "server_was_loaded",
            "watchdog_was_loaded",
            "detail",
        ];
        if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
            return Err(format!("guardian transaction contains unknown field {key}"));
        }
        if object.get("schema_version").and_then(Value::as_u64) != Some(1) {
            return Err("guardian transaction schema_version must be 1".to_owned());
        }
        let required_string = |key: &str| -> Result<String, String> {
            let value = object
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("guardian transaction {key} must be a string"))?;
            if value.len() > 4096 {
                return Err(format!("guardian transaction {key} is too long"));
            }
            Ok(value.to_owned())
        };
        let optional_string = |key: &str| -> Result<Option<String>, String> {
            match object.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(value)) if value.len() <= 4096 => Ok(Some(value.clone())),
                _ => Err(format!(
                    "guardian transaction {key} must be null or a bounded string"
                )),
            }
        };
        let transaction_id = required_string("transaction_id")?;
        if !valid_guardian_transaction_id(&transaction_id) {
            return Err("guardian transaction id is invalid".to_owned());
        }
        let mode = GuardianMode::parse(&required_string("mode")?)?;
        let state = required_string("state")?;
        if !matches!(
            state.as_str(),
            "armed"
                | "watching"
                | "committed"
                | "parent_recovered"
                | "aborted"
                | "recovering"
                | "recovered"
                | "recovery_failed"
                | "expired_parent_alive"
                | "observed_committed"
                | "observed_parent_recovered"
        ) {
            return Err(format!("guardian transaction state {state} is invalid"));
        }
        let parent_pid = object
            .get("parent_pid")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| "guardian transaction parent_pid is invalid".to_owned())?;
        let created_at = object
            .get("created_at")
            .and_then(Value::as_i64)
            .ok_or_else(|| "guardian transaction created_at is invalid".to_owned())?;
        let server_was_loaded = object
            .get("server_was_loaded")
            .and_then(Value::as_bool)
            .ok_or_else(|| "guardian transaction server_was_loaded is invalid".to_owned())?;
        let watchdog_was_loaded = object
            .get("watchdog_was_loaded")
            .and_then(Value::as_bool)
            .ok_or_else(|| "guardian transaction watchdog_was_loaded is invalid".to_owned())?;
        Ok(GuardianRecord {
            transaction_id,
            mode,
            state,
            parent_pid,
            created_at,
            rollback_id: optional_string("rollback_id")?,
            candidate_generation_id: optional_string("candidate_generation_id")?,
            server_plist_backup: optional_string("server_plist_backup")?,
            watchdog_plist_backup: optional_string("watchdog_plist_backup")?,
            previous_current_target: optional_string("previous_current_target")?,
            server_was_loaded,
            watchdog_was_loaded,
            detail: optional_string("detail")?,
        })
    }

    fn write_guardian_record(paths: &ServicePaths, record: &GuardianRecord) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&guardian_record_value(record))
            .map_err(|error| format!("cannot encode guardian transaction: {error}"))?;
        atomic_write(
            &guardian_record_path(paths, &record.transaction_id)?,
            &bytes,
            0o600,
        )
    }

    fn read_guardian_record(
        paths: &ServicePaths,
        transaction_id: &str,
    ) -> Result<GuardianRecord, String> {
        let bytes =
            read_optional_bounded(&guardian_record_path(paths, transaction_id)?, 64 * 1024)?
                .ok_or_else(|| format!("guardian transaction {transaction_id} is missing"))?;
        let value = serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| format!("cannot parse guardian transaction: {error}"))?;
        decode_guardian_record(&value)
    }

    fn with_guardian_state_lock<T, F>(
        paths: &ServicePaths,
        transaction_id: &str,
        operation: F,
    ) -> Result<T, String>
    where
        F: FnOnce() -> Result<T, String>,
    {
        let lock_path = guardian_state_lock_path(paths, transaction_id)?;
        if let Ok(metadata) = fs::symlink_metadata(&lock_path)
            && metadata.file_type().is_symlink()
        {
            return Err("guardian transaction lock must not be a symlink".to_owned());
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)
            .map_err(|error| format!("cannot open guardian transaction lock: {error}"))?;
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("cannot secure guardian transaction lock: {error}"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(format!(
                "cannot lock guardian transaction: {}",
                std::io::Error::last_os_error()
            ));
        }
        let result = operation();
        let unlock = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
        if unlock != 0 && result.is_ok() {
            return Err(format!(
                "cannot unlock guardian transaction: {}",
                std::io::Error::last_os_error()
            ));
        }
        result
    }

    fn transition_guardian_state(
        paths: &ServicePaths,
        transaction_id: &str,
        expected: &[&str],
        state: &str,
        detail: Option<&str>,
    ) -> Result<bool, String> {
        with_guardian_state_lock(paths, transaction_id, || {
            let mut record = read_guardian_record(paths, transaction_id)?;
            if !expected.contains(&record.state.as_str()) {
                return Ok(false);
            }
            record.state = state.to_owned();
            record.detail = detail.map(|value| value.chars().take(512).collect());
            write_guardian_record(paths, &record)?;
            Ok(true)
        })
    }

    fn update_guardian_state(
        paths: &ServicePaths,
        transaction_id: &str,
        state: &str,
        detail: Option<&str>,
    ) -> Result<(), String> {
        with_guardian_state_lock(paths, transaction_id, || {
            let mut record = read_guardian_record(paths, transaction_id)?;
            record.state = state.to_owned();
            record.detail = detail.map(|value| value.chars().take(512).collect());
            write_guardian_record(paths, &record)
        })
    }

    fn terminate_guardian_child(child: &mut Child) -> Result<(), String> {
        if child
            .try_wait()
            .map_err(|error| format!("cannot inspect guardian child: {error}"))?
            .is_some()
        {
            return Ok(());
        }
        if let Err(error) = child.kill() {
            if child
                .try_wait()
                .map_err(|wait_error| {
                    format!(
                        "cannot terminate guardian child: {error}; cannot inspect raced exit: {wait_error}"
                    )
                })?
                .is_some()
            {
                return Ok(());
            }
            return Err(format!("cannot terminate guardian child: {error}"));
        }
        child
            .wait()
            .map_err(|error| format!("cannot reap terminated guardian child: {error}"))?;
        Ok(())
    }

    fn abort_guardian_startup(
        paths: &ServicePaths,
        transaction_id: &str,
        child: &mut Child,
        write_signal: &mut Option<File>,
        detail: &str,
    ) -> Result<(), String> {
        let state_fence = match transition_guardian_state(
            paths,
            transaction_id,
            &["armed", "watching"],
            "aborted",
            Some(detail),
        ) {
            Ok(true) => Ok(()),
            Ok(false) => match read_guardian_record(paths, transaction_id) {
                Ok(current) if current.state == "aborted" => Ok(()),
                Ok(current) => Err(format!(
                    "guardian startup abort found unexpected transaction state {}",
                    current.state
                )),
                Err(error) => Err(format!(
                    "guardian startup abort could not read transaction after lost state race: {error}"
                )),
            },
            Err(error) => Err(format!(
                "guardian startup abort could not fence transaction state: {error}"
            )),
        };
        let child_termination = terminate_guardian_child(child);
        if child_termination.is_ok() {
            write_signal.take();
        } else if let Some(signal) = write_signal.take() {
            // Do not deliver POLLHUP while a child that could not be reaped may
            // still be running. The CLI error path exits shortly afterward.
            std::mem::forget(signal);
        }
        match (state_fence, child_termination) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(state_error), Ok(())) => Err(state_error),
            (Ok(()), Err(child_error)) => Err(child_error),
            (Err(state_error), Err(child_error)) => Err(format!(
                "{state_error}; guardian child termination also failed: {child_error}"
            )),
        }
    }

    fn guardian_pipe() -> Result<(File, File), String> {
        let mut fds = [0_i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(format!(
                "cannot create guardian parent pipe: {}",
                std::io::Error::last_os_error()
            ));
        }
        let read = unsafe { File::from_raw_fd(fds[0]) };
        let write = unsafe { File::from_raw_fd(fds[1]) };
        Ok((read, write))
    }

    fn arm_guardian(
        paths: &ServicePaths,
        mutation_lock: &ServiceMutationLock,
        mut record: GuardianRecord,
    ) -> Result<GuardianHandle, String> {
        ensure_secure_dir(&paths.guardians_dir)?;
        let directory = guardian_dir(paths, &record.transaction_id)?;
        fs::create_dir(&directory).map_err(|error| {
            format!(
                "cannot create guardian transaction directory {}: {error}",
                directory.display()
            )
        })?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure guardian transaction directory: {error}"))?;
        record.state = "armed".to_owned();
        record.detail = Some("guardian snapshot persisted before service mutation".to_owned());
        write_guardian_record(paths, &record)?;

        let binary = guardian_binary_path(paths, &record.transaction_id)?;
        atomic_copy_executable(&paths.source_binary, &binary)?;
        let log_path = guardian_log_path(paths, &record.transaction_id)?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)
            .map_err(|error| format!("cannot open guardian log: {error}"))?;
        fs::set_permissions(&log_path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("cannot secure guardian log: {error}"))?;
        let stderr = log
            .try_clone()
            .map_err(|error| format!("cannot clone guardian log handle: {error}"))?;
        let (read_signal, write_signal) = guardian_pipe()?;
        let read_fd = read_signal.as_raw_fd();
        let write_fd = write_signal.as_raw_fd();
        let lock_fd = mutation_lock.fd();
        if [read_fd, write_fd, lock_fd]
            .iter()
            .any(|fd| matches!(*fd, GUARDIAN_PARENT_FD | GUARDIAN_LOCK_FD))
        {
            return Err("guardian reserved file descriptor collision".to_owned());
        }

        let mut command = Command::new(&binary);
        command
            .args([
                "service",
                "__guardian",
                "--transaction",
                &record.transaction_id,
                "--parent-pid",
                &record.parent_pid.to_string(),
            ])
            .env_clear()
            .env("HOME", &paths.home)
            .env("HERDR_MCP_CONFIG_DIR", &paths.config_dir)
            .env("HERDR_SOCKET_PATH", &paths.herdr_socket)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr));
        if let Some(path) = env::var_os("PATH") {
            command.env("PATH", path);
        }
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(read_fd, GUARDIAN_PARENT_FD) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(lock_fd, GUARDIAN_LOCK_FD) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(write_fd);
                libc::close(read_fd);
                libc::close(lock_fd);
                Ok(())
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = transition_guardian_state(
                    paths,
                    &record.transaction_id,
                    &["armed"],
                    "aborted",
                    Some("guardian child could not start"),
                );
                return Err(format!("cannot start service guardian: {error}"));
            }
        };
        drop(read_signal);

        let mut write_signal = Some(write_signal);
        let deadline = Instant::now() + GUARDIAN_HANDSHAKE_BUDGET;
        loop {
            let current = match read_guardian_record(paths, &record.transaction_id) {
                Ok(current) => current,
                Err(error) => {
                    let cleanup = abort_guardian_startup(
                        paths,
                        &record.transaction_id,
                        &mut child,
                        &mut write_signal,
                        "guardian transaction became unreadable during handshake",
                    );
                    return Err(match cleanup {
                        Ok(()) => format!("cannot read guardian handshake state: {error}"),
                        Err(cleanup_error) => format!(
                            "cannot read guardian handshake state: {error}; startup cleanup failed: {cleanup_error}"
                        ),
                    });
                }
            };
            if current.state == "watching" {
                return Ok(GuardianHandle {
                    paths: paths.clone(),
                    transaction_id: record.transaction_id,
                    signal: write_signal.take(),
                    child,
                });
            }
            let child_status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    let cleanup = abort_guardian_startup(
                        paths,
                        &record.transaction_id,
                        &mut child,
                        &mut write_signal,
                        "guardian child status became unreadable during handshake",
                    );
                    return Err(match cleanup {
                        Ok(()) => format!("cannot inspect guardian startup: {error}"),
                        Err(cleanup_error) => format!(
                            "cannot inspect guardian startup: {error}; startup cleanup failed: {cleanup_error}"
                        ),
                    });
                }
            };
            if let Some(status) = child_status {
                let cleanup = abort_guardian_startup(
                    paths,
                    &record.transaction_id,
                    &mut child,
                    &mut write_signal,
                    "guardian child exited before handshake",
                );
                return Err(match cleanup {
                    Ok(()) => format!("service guardian exited before handshake with {status}"),
                    Err(cleanup_error) => format!(
                        "service guardian exited before handshake with {status}; startup cleanup failed: {cleanup_error}"
                    ),
                });
            }
            if Instant::now() >= deadline {
                let cleanup = abort_guardian_startup(
                    paths,
                    &record.transaction_id,
                    &mut child,
                    &mut write_signal,
                    "guardian child handshake timed out",
                );
                return Err(match cleanup {
                    Ok(()) => format!(
                        "service guardian did not confirm watching state within {GUARDIAN_HANDSHAKE_BUDGET:?}"
                    ),
                    Err(cleanup_error) => format!(
                        "service guardian did not confirm watching state within {GUARDIAN_HANDSHAKE_BUDGET:?}; startup cleanup failed: {cleanup_error}"
                    ),
                });
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn guardian_fd_is_open(fd: i32) -> bool {
        (unsafe { libc::fcntl(fd, libc::F_GETFD) }) != -1
    }

    fn set_guardian_fd_cloexec(fd: i32) -> Result<(), String> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags == -1 {
            return Err(format!(
                "cannot read guardian fd {fd} flags: {}",
                std::io::Error::last_os_error()
            ));
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
            return Err(format!(
                "cannot mark guardian fd {fd} close-on-exec: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    fn wait_for_guardian_parent_signal(fd: i32, budget: Duration) -> Result<bool, String> {
        let deadline = Instant::now() + budget;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            let remaining = deadline.saturating_duration_since(now);
            let timeout = remaining.min(Duration::from_millis(250)).as_millis() as i32;
            let mut pollfd = libc::pollfd {
                fd,
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut pollfd, 1, timeout) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("guardian parent pipe poll failed: {error}"));
            }
            if result == 0 {
                continue;
            }
            if pollfd.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(true);
            }
            if pollfd.revents & libc::POLLIN != 0 {
                let mut byte = [0_u8; 1];
                let read = unsafe { libc::read(fd, byte.as_mut_ptr().cast(), 1) };
                if read == 0 {
                    return Ok(true);
                }
                if read < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::Interrupted {
                        return Err(format!("guardian parent pipe read failed: {error}"));
                    }
                }
            }
        }
    }

    fn guardian_decision(
        record: &GuardianRecord,
        rollback: Option<&ServiceRollbackRecord>,
        active: Option<&RuntimeGenerationRecord>,
    ) -> GuardianDecision {
        match record.state.as_str() {
            "committed" | "observed_committed" => return GuardianDecision::Exit("committed"),
            "parent_recovered" | "observed_parent_recovered" | "recovered" => {
                return GuardianDecision::Exit("parent_recovered");
            }
            "aborted" => return GuardianDecision::Exit("aborted"),
            "armed" | "watching" => {}
            other => {
                return GuardianDecision::Refuse(format!(
                    "guardian refuses parent-exit recovery from transaction state {other}"
                ));
            }
        }
        match record.mode {
            GuardianMode::Install => {
                let candidate_active = active.map(|generation| generation.generation_id.as_str())
                    == record.candidate_generation_id.as_deref();
                if let Some(rollback_id) = record.rollback_id.as_deref() {
                    let Some(rollback) = rollback else {
                        return GuardianDecision::Refuse(format!(
                            "guardian install rollback {rollback_id} is missing"
                        ));
                    };
                    if rollback.rollback_id != rollback_id {
                        return GuardianDecision::Refuse(
                            "guardian install rollback identity mismatch".to_owned(),
                        );
                    }
                    match rollback.state.as_str() {
                        "ready" if candidate_active => GuardianDecision::Exit("committed"),
                        "ready" => GuardianDecision::Refuse(
                            "guardian install found ready rollback without active candidate generation"
                                .to_owned(),
                        ),
                        "auto_rolled_back" | "aborted" if !candidate_active => {
                            GuardianDecision::Exit("parent_recovered")
                        }
                        "auto_rolled_back" | "aborted" => GuardianDecision::Refuse(
                            "guardian install found recovered rollback while candidate is still active"
                                .to_owned(),
                        ),
                        "prepared" | "rollback_failed" if !candidate_active => {
                            GuardianDecision::Recover
                        }
                        "prepared" | "rollback_failed" => GuardianDecision::Refuse(
                            "guardian install found uncommitted rollback state with active candidate generation"
                                .to_owned(),
                        ),
                        other => GuardianDecision::Refuse(format!(
                            "guardian install refuses rollback state {other}"
                        )),
                    }
                } else if candidate_active {
                    GuardianDecision::Exit("committed")
                } else {
                    GuardianDecision::Recover
                }
            }
            GuardianMode::Rollback => {
                let Some(rollback_id) = record.rollback_id.as_deref() else {
                    return GuardianDecision::Refuse(
                        "guardian rollback transaction is missing rollback identity".to_owned(),
                    );
                };
                let Some(rollback) = rollback else {
                    return GuardianDecision::Refuse(format!(
                        "guardian rollback {rollback_id} is missing"
                    ));
                };
                if rollback.rollback_id != rollback_id {
                    return GuardianDecision::Refuse(
                        "guardian rollback identity mismatch".to_owned(),
                    );
                }
                let candidate_active = active.map(|generation| generation.generation_id.as_str())
                    == record.candidate_generation_id.as_deref();
                match rollback.state.as_str() {
                    "consumed" => {
                        let committed_active = if rollback.source_kind == "rust" {
                            let expected = rollback
                                .previous_current_target
                                .as_deref()
                                .and_then(|target| generation_id_from_target(Path::new(target)));
                            active.map(|generation| generation.generation_id.as_str()) == expected
                        } else {
                            active.is_none()
                        };
                        if committed_active {
                            GuardianDecision::Exit("committed")
                        } else {
                            GuardianDecision::Refuse(
                                "guardian rollback found consumed ledger with unexpected active generation"
                                    .to_owned(),
                            )
                        }
                    }
                    "ready" if candidate_active => GuardianDecision::Exit("parent_recovered"),
                    "ready" => GuardianDecision::Refuse(
                        "guardian rollback found ready ledger without the original active generation"
                            .to_owned(),
                    ),
                    "consuming" | "rollback_failed" if candidate_active => {
                        GuardianDecision::Recover
                    }
                    "consuming" | "rollback_failed" => GuardianDecision::Refuse(
                        "guardian rollback found unfinished ledger after active generation changed"
                            .to_owned(),
                    ),
                    other => GuardianDecision::Refuse(format!(
                        "guardian rollback refuses ledger state {other}"
                    )),
                }
            }
        }
    }

    fn guardian_quiesce_label(label: &str) -> Result<(), String> {
        guardian_quiesce_with(
            || is_loaded(label),
            || wait_launchd_absent(label, LAUNCHD_RECOVERY_BUDGET),
            || request_bootout(label),
        )
    }

    fn guardian_quiesce_with<Loaded, Wait, Stop>(
        mut loaded: Loaded,
        mut wait: Wait,
        mut stop: Stop,
    ) -> Result<(), String>
    where
        Loaded: FnMut() -> bool,
        Wait: FnMut() -> Result<(), String>,
        Stop: FnMut() -> Result<bool, String>,
    {
        if !loaded() {
            return Ok(());
        }
        if wait().is_ok() {
            return Ok(());
        }
        if stop()? {
            wait()?;
        }
        Ok(())
    }

    fn guardian_restore_known_good(
        paths: &ServicePaths,
        record: &GuardianRecord,
    ) -> Result<(), String> {
        let server_bytes = record
            .server_plist_backup
            .as_deref()
            .map(|path| read_owned_backup(paths, path))
            .transpose()?;
        let server = describe_service(server_bytes.as_deref(), paths)?;
        if server_bytes.is_some() && !matches!(server.kind, ServiceKind::Rust | ServiceKind::Node) {
            return Err("guardian server backup is not an owned Rust/Node service".to_owned());
        }
        if record.server_was_loaded && server_bytes.is_none() {
            return Err("guardian expected a loaded server but has no server backup".to_owned());
        }
        let watchdog_bytes = record
            .watchdog_plist_backup
            .as_deref()
            .map(|path| read_owned_backup(paths, path))
            .transpose()?;
        if watchdog_bytes.is_some() && !watchdog_is_legacy_owned(watchdog_bytes.as_deref())? {
            return Err("guardian watchdog backup is not legacy herdr-mcp watchdog".to_owned());
        }
        if record.watchdog_was_loaded && watchdog_bytes.is_none() {
            return Err(
                "guardian expected a loaded watchdog but has no watchdog backup".to_owned(),
            );
        }
        let previous_target = record.previous_current_target.as_deref().map(PathBuf::from);
        if previous_target
            .as_deref()
            .is_some_and(|target| !is_owned_generation_target(target))
        {
            return Err("guardian previous runtime/current target is not managed".to_owned());
        }

        guardian_quiesce_label(WATCHDOG_LABEL)?;
        guardian_quiesce_label(SERVICE_LABEL)?;
        match server_bytes.as_deref() {
            Some(bytes) => atomic_write(&paths.plist, bytes, 0o600)?,
            None => remove_regular_file(&paths.plist)?,
        }
        restore_current(paths, previous_target.as_deref())?;
        match watchdog_bytes.as_deref() {
            Some(bytes) => atomic_write(&paths.watchdog_plist, bytes, 0o600)?,
            None => remove_regular_file(&paths.watchdog_plist)?,
        }
        if record.server_was_loaded {
            bootstrap_with_retry(&paths.plist, SERVICE_LABEL)?;
            wait_for_service_health(&server, DEFAULT_PORT)?;
        }
        if record.watchdog_was_loaded {
            bootstrap_with_retry(&paths.watchdog_plist, WATCHDOG_LABEL)?;
        }
        Ok(())
    }

    fn guardian_after_parent_exit(
        paths: &ServicePaths,
        transaction_id: &str,
    ) -> Result<Value, String> {
        let record = read_guardian_record(paths, transaction_id)?;
        let store = StateStore::open_in_dir(&paths.config_dir, "state")?;
        let rollback = record
            .rollback_id
            .as_deref()
            .map(|id| store.service_rollback_by_id(id))
            .transpose()?
            .flatten();
        let active = store.active_runtime_generation()?;
        match guardian_decision(&record, rollback.as_ref(), active.as_ref()) {
            GuardianDecision::Exit(reason) => {
                let state = if reason == "committed" {
                    "observed_committed"
                } else if reason == "parent_recovered" {
                    "observed_parent_recovered"
                } else {
                    "aborted"
                };
                update_guardian_state(paths, transaction_id, state, Some(reason))?;
                Ok(json!({"ok": true, "guardian": state, "transaction_id": transaction_id}))
            }
            GuardianDecision::Refuse(reason) => Err(reason),
            GuardianDecision::Recover => {
                update_guardian_state(
                    paths,
                    transaction_id,
                    "recovering",
                    Some("parent signal closed before transaction settlement"),
                )?;
                guardian_restore_known_good(paths, &record)?;
                if let Some(rollback_id) = record.rollback_id.as_deref() {
                    store.recover_service_rollback_after_guardian(
                        rollback_id,
                        record.mode.as_str(),
                        now_ms_i64(),
                    )?;
                }
                store.record_service_event(
                    "guardian",
                    "recovered",
                    record.candidate_generation_id.as_deref(),
                    now_ms_i64(),
                    Some(record.mode.as_str()),
                )?;
                update_guardian_state(
                    paths,
                    transaction_id,
                    "recovered",
                    Some("known-good service snapshot restored"),
                )?;
                Ok(json!({"ok": true, "guardian": "recovered", "transaction_id": transaction_id}))
            }
        }
    }

    fn guardian(
        paths: &ServicePaths,
        transaction_id: &str,
        parent_pid: u32,
    ) -> Result<Value, String> {
        if !guardian_fd_is_open(GUARDIAN_PARENT_FD) || !guardian_fd_is_open(GUARDIAN_LOCK_FD) {
            return Err(
                "guardian requires inherited parent-signal and mutation-lock descriptors"
                    .to_owned(),
            );
        }
        set_guardian_fd_cloexec(GUARDIAN_PARENT_FD)?;
        set_guardian_fd_cloexec(GUARDIAN_LOCK_FD)?;
        let record = read_guardian_record(paths, transaction_id)?;
        if record.transaction_id != transaction_id || record.parent_pid != parent_pid {
            return Err("guardian transaction identity does not match invocation".to_owned());
        }
        if record.state == "aborted" {
            return Ok(
                json!({"ok": true, "guardian": "aborted", "transaction_id": transaction_id}),
            );
        }
        if record.state != "armed" {
            return Err(format!(
                "guardian expected armed transaction, found {}",
                record.state
            ));
        }
        if !transition_guardian_state(
            paths,
            transaction_id,
            &["armed"],
            "watching",
            Some("guardian handshake complete; waiting for parent settlement"),
        )? {
            let current = read_guardian_record(paths, transaction_id)?;
            if current.state == "aborted" {
                return Ok(json!({
                    "ok": true,
                    "guardian": "aborted",
                    "transaction_id": transaction_id
                }));
            }
            return Err(format!(
                "guardian could not transition transaction from armed to watching; state is {}",
                current.state
            ));
        }
        let parent_closed =
            wait_for_guardian_parent_signal(GUARDIAN_PARENT_FD, GUARDIAN_MAX_LIFETIME)?;
        let result = if parent_closed {
            guardian_after_parent_exit(paths, transaction_id)
        } else {
            let _ = update_guardian_state(
                paths,
                transaction_id,
                "expired_parent_alive",
                Some(
                    "guardian lifetime expired while parent signal remained open; no recovery attempted",
                ),
            );
            let _ = StateStore::open_in_dir(&paths.config_dir, "state").and_then(|store| {
                store.record_service_event(
                    "guardian",
                    "expired_parent_alive",
                    record.candidate_generation_id.as_deref(),
                    now_ms_i64(),
                    Some(record.mode.as_str()),
                )
            });
            Ok(json!({
                "ok": true,
                "guardian": "expired_parent_alive",
                "transaction_id": transaction_id
            }))
        };
        if let Err(error) = &result {
            let _ = update_guardian_state(paths, transaction_id, "recovery_failed", Some(error));
            let _ = StateStore::open_in_dir(&paths.config_dir, "state").and_then(|store| {
                store.record_service_event(
                    "guardian",
                    "recovery_failed",
                    record.candidate_generation_id.as_deref(),
                    now_ms_i64(),
                    Some(error),
                )
            });
        }
        let _ = remove_regular_file(&guardian_binary_path(paths, transaction_id)?);
        result
    }

    fn install(
        paths: &ServicePaths,
        adopt_node: bool,
        mutation_lock: &ServiceMutationLock,
    ) -> Result<Value, String> {
        install_with_noop_checks(
            paths,
            adopt_node,
            mutation_lock,
            || is_loaded(SERVICE_LABEL),
            || health_once(DEFAULT_PORT),
        )
    }

    fn install_with_noop_checks<Loaded, Healthy>(
        paths: &ServicePaths,
        adopt_node: bool,
        mutation_lock: &ServiceMutationLock,
        loaded: Loaded,
        healthy: Healthy,
    ) -> Result<Value, String>
    where
        Loaded: FnOnce() -> bool,
        Healthy: FnOnce() -> bool,
    {
        let existing_bytes = read_optional_bounded(&paths.plist, 256 * 1024)?;
        let existing = describe_service(existing_bytes.as_deref(), paths)?;
        match existing.kind {
            ServiceKind::Missing | ServiceKind::Rust => {}
            ServiceKind::Node if adopt_node => {}
            ServiceKind::Node => {
                return Err(
                    "existing Node herdr-mcp service detected; rerun with service install --adopt-node"
                        .to_owned(),
                );
            }
            ServiceKind::Other => {
                return Err("existing service plist is not owned by herdr-mcp Rust/Node".to_owned());
            }
        }

        let watchdog_bytes = read_optional_bounded(&paths.watchdog_plist, 256 * 1024)?;
        if watchdog_bytes.is_some() && !watchdog_is_legacy_owned(watchdog_bytes.as_deref())? {
            return Err("existing watchdog plist is not the legacy herdr-mcp watchdog".to_owned());
        }
        if watchdog_bytes.is_some() && existing.kind != ServiceKind::Node {
            return Err(
                "legacy watchdog is still installed; only explicit Node adoption may retire it"
                    .to_owned(),
            );
        }

        if let Some(mut result) = same_active_install_noop_with(paths, &existing, loaded, healthy)?
        {
            let user_cli = user_cli::ensure_link(&paths.home, &paths.current_binary)?;
            if let Some(object) = result.as_object_mut() {
                object.insert(
                    "user_cli".to_owned(),
                    json!(user_cli.path.to_string_lossy()),
                );
                object.insert(
                    "user_cli_target".to_owned(),
                    json!(user_cli.target.to_string_lossy()),
                );
                object.insert("user_cli_changed".to_owned(), json!(user_cli.changed));
            }
            return Ok(result);
        }

        let generation = prepare_generation(paths)?;
        let now = now_ms_i64();
        let mut store = StateStore::open_in_dir(&paths.config_dir, "state")?;
        store.stage_runtime_generation(
            &generation.generation_id,
            &generation.binary.to_string_lossy(),
            &generation.sha256,
            if existing.kind == ServiceKind::Node {
                "node-adoption"
            } else {
                "service-install"
            },
            now,
        )?;

        let env = service_environment(paths, &existing.env, &generation)?;
        let new_plist = encode_service_plist(paths, &env)?;
        let rollback = RollbackState {
            server_plist: existing_bytes.clone(),
            server_was_loaded: is_loaded(SERVICE_LABEL),
            watchdog_plist: watchdog_bytes.clone(),
            watchdog_was_loaded: is_loaded(WATCHDOG_LABEL),
            previous_current: current_target(paths)?,
        };

        let source_kind = match existing.kind {
            ServiceKind::Node => Some("node"),
            ServiceKind::Rust => Some("rust"),
            ServiceKind::Missing => None,
            ServiceKind::Other => unreachable!("unowned services were rejected above"),
        };
        let mut backups = Vec::new();
        let server_backup = match (source_kind, existing_bytes.as_deref()) {
            (Some(kind), Some(bytes)) => {
                let path = backup_bytes(paths, &format!("{kind}-server"), bytes)?;
                backups.push(path.clone());
                Some(path)
            }
            _ => None,
        };
        let watchdog_backup = match watchdog_bytes.as_deref() {
            Some(bytes) => {
                let path = backup_bytes(paths, "node-watchdog", bytes)?;
                backups.push(path.clone());
                Some(path)
            }
            None => None,
        };
        let rollback_id = source_kind.map(|kind| {
            format!(
                "rb-{}-{kind}-{}",
                now,
                generation.sha256.get(..8).unwrap_or("generation")
            )
        });
        if let (Some(rollback_id), Some(source_kind)) = (rollback_id.as_deref(), source_kind) {
            store.prepare_service_rollback(&ServiceRollbackRecord {
                rollback_id: rollback_id.to_owned(),
                source_kind: source_kind.to_owned(),
                activated_generation_id: generation.generation_id.clone(),
                server_plist_backup: server_backup.clone(),
                watchdog_plist_backup: watchdog_backup.clone(),
                previous_current_target: rollback
                    .previous_current
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                server_was_loaded: rollback.server_was_loaded,
                watchdog_was_loaded: rollback.watchdog_was_loaded,
                created_at: now,
                state: "prepared".to_owned(),
            })?;
        }

        let transaction_id = new_guardian_transaction_id()?;
        let mut guardian = match arm_guardian(
            paths,
            mutation_lock,
            GuardianRecord {
                transaction_id: transaction_id.clone(),
                mode: GuardianMode::Install,
                state: "armed".to_owned(),
                parent_pid: std::process::id(),
                created_at: now_ms_i64(),
                rollback_id: rollback_id.clone(),
                candidate_generation_id: Some(generation.generation_id.clone()),
                server_plist_backup: server_backup.clone(),
                watchdog_plist_backup: watchdog_backup.clone(),
                previous_current_target: rollback
                    .previous_current
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                server_was_loaded: rollback.server_was_loaded,
                watchdog_was_loaded: rollback.watchdog_was_loaded,
                detail: None,
            },
        ) {
            Ok(guardian) => guardian,
            Err(error) => {
                if let Some(rollback_id) = rollback_id.as_deref() {
                    let _ =
                        store.mark_prepared_service_rollback(rollback_id, "aborted", now_ms_i64());
                }
                return Err(format!(
                    "service guardian could not arm before install mutation: {error}"
                ));
            }
        };

        let mut server_bootout_pending = false;
        let mut user_cli_link = None;
        let activation = (|| -> Result<(), String> {
            if rollback.watchdog_was_loaded {
                bootout(WATCHDOG_LABEL)?;
            }
            if rollback.watchdog_plist.is_some() {
                remove_regular_file(&paths.watchdog_plist)?;
            }
            if rollback.server_was_loaded {
                server_bootout_pending = request_bootout(SERVICE_LABEL)?;
                if server_bootout_pending {
                    wait_launchd_absent(SERVICE_LABEL, LAUNCHD_BOOTOUT_BUDGET)?;
                    server_bootout_pending = false;
                }
            }
            atomic_write(&paths.plist, &new_plist, 0o600)?;
            switch_current(paths, &generation)?;
            // Point PATH entry at runtime/current before bootstrap so a failed
            // link rolls back with the rest of activation. The link always
            // resolves through `current`, so generation rollback stays valid.
            user_cli_link = Some(user_cli::ensure_link(&paths.home, &paths.current_binary)?);
            bootstrap_with_retry(&paths.plist, SERVICE_LABEL)?;
            wait_for_health(DEFAULT_PORT)?;
            store.activate_runtime_generation_with_rollback(
                &generation.generation_id,
                rollback_id.as_deref(),
                now_ms_i64(),
            )?;
            Ok(())
        })();

        if let Err(error) = activation {
            let rollback_error = rollback_install(paths, &rollback, server_bootout_pending).err();
            if let Some(rollback_id) = rollback_id.as_deref() {
                let _ = store.mark_prepared_service_rollback(
                    rollback_id,
                    if rollback_error.is_some() {
                        "rollback_failed"
                    } else {
                        "auto_rolled_back"
                    },
                    now_ms_i64(),
                );
            }
            let _ = store.record_service_event(
                "install",
                "rolled_back",
                Some(&generation.generation_id),
                now_ms_i64(),
                Some(&error),
            );
            if rollback_error.is_none() {
                let _ = guardian.finish(
                    "parent_recovered",
                    "service manager synchronous rollback restored the pre-install service",
                );
            }
            return Err(match rollback_error {
                Some(rollback_error) => format!(
                    "Rust service activation failed: {error}; rollback also failed: {rollback_error}; one-shot guardian remains armed for known-good recovery"
                ),
                None => format!("Rust service activation failed and was rolled back: {error}"),
            });
        }

        let evidence_recorded = store
            .record_service_event(
                "install",
                "ok",
                Some(&generation.generation_id),
                now_ms_i64(),
                Some(if existing.kind == ServiceKind::Node {
                    "adopted-node"
                } else {
                    "rust-service"
                }),
            )
            .is_ok();

        let guardian_settled = guardian
            .finish(
                "committed",
                "service generation activation and health gate committed",
            )
            .is_ok();

        let user_cli = user_cli_link.ok_or_else(|| {
            "user CLI link missing after successful service activation".to_owned()
        })?;

        Ok(json!({
            "ok": true,
            "implementation": "rust",
            "label": SERVICE_LABEL,
            "generation": generation.generation_id,
            "sha256": generation.sha256,
            "runtime_binary": generation.binary,
            "current_binary": paths.current_binary,
            "user_cli": user_cli.path,
            "user_cli_target": user_cli.target,
            "user_cli_changed": user_cli.changed,
            "plist": paths.plist,
            "extension_socket": paths.extension_socket,
            "adopted_node": existing.kind == ServiceKind::Node,
            "retired_legacy_watchdog": rollback.watchdog_plist.is_some(),
            "backups": backups,
            "rollback_id": rollback_id,
            "rollback_ready": rollback_id.is_some(),
            "guardian_transaction": transaction_id,
            "guardian_settled": guardian_settled,
            "evidence_recorded": evidence_recorded,
        }))
    }

    fn same_active_install_noop_with<Loaded, Healthy>(
        paths: &ServicePaths,
        existing: &ServiceDescriptor,
        loaded: Loaded,
        healthy: Healthy,
    ) -> Result<Option<Value>, String>
    where
        Loaded: FnOnce() -> bool,
        Healthy: FnOnce() -> bool,
    {
        if existing.kind != ServiceKind::Rust {
            return Ok(None);
        }

        let sha256 = file_sha256(&paths.source_binary)?;
        let generation_id = format!("rust-{}", &sha256[..16]);

        // Resolve the active generation; propagate errors rather than swallowing
        // them so a malformed or non-symlink runtime/current fails closed.
        let Some(target) = current_target(paths)? else {
            return Ok(None);
        };
        if !is_owned_generation_target(&target) {
            return Err(format!(
                "runtime/current points outside managed generations: {}",
                target.display()
            ));
        }
        let Some(current_generation) = generation_id_from_target(&target) else {
            return Err("runtime/current target is not a managed generation".to_owned());
        };
        if current_generation != generation_id {
            return Ok(None);
        }

        // Verify the generation directory itself is a real directory, not a
        // symlink; a symlinked generation dir is an unsafe path and must fail
        // closed. A missing generation dir falls through to repair like a
        // missing binary.
        let generation_dir = paths.generations_dir.join(&generation_id);
        match fs::symlink_metadata(&generation_dir) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "installed generation directory must not be a symlink: {}",
                    generation_dir.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "installed generation directory is not a directory: {}",
                    generation_dir.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => {
                return Err(format!(
                    "cannot inspect installed generation directory {}: {}",
                    generation_dir.display(),
                    error
                ));
            }
        }

        // Verify the installed generation binary is a regular non-symlink file
        // whose full SHA equals the source SHA. Missing or hash-mismatched
        // binaries fall through to repair; symlink/unsafe paths fail closed.
        let installed_binary = generation_dir.join("herdr-mcp");
        let installed_sha = match fs::symlink_metadata(&installed_binary) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "installed generation binary must not be a symlink: {}",
                    installed_binary.display()
                ));
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(format!(
                    "installed generation binary is not a regular file: {}",
                    installed_binary.display()
                ));
            }
            Ok(_) => file_sha256(&installed_binary)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => {
                return Err(format!(
                    "cannot inspect installed generation binary {}: {}",
                    installed_binary.display(),
                    error
                ));
            }
        };
        if installed_sha != sha256 {
            return Ok(None);
        }

        // The service plist descriptor must name the active generation.
        if existing
            .env
            .get("HERDR_MCP_RUNTIME_GENERATION")
            .map(String::as_str)
            != Some(generation_id.as_str())
        {
            return Ok(None);
        }

        if !loaded() || !healthy() {
            return Ok(None);
        }

        Ok(Some(json!({
            "ok": true,
            "implementation": "rust",
            "label": SERVICE_LABEL,
            "already_active": true,
            "changed": false,
            "generation": generation_id,
            "sha256": sha256,
            "runtime_binary": paths.generations_dir.join(&generation_id).join("herdr-mcp"),
            "current_binary": paths.current_binary,
            "plist": paths.plist,
            "extension_socket": paths.extension_socket,
            "adopted_node": false,
            "retired_legacy_watchdog": false,
            "backups": [],
            "rollback_id": Value::Null,
            "rollback_ready": false,
            "guardian_transaction": Value::Null,
            "guardian_settled": Value::Null,
            "evidence_recorded": false,
        })))
    }

    pub(super) fn doctor_status() -> Result<Value, String> {
        let paths = ServicePaths::discover()?;
        status(&paths)
    }

    fn status(paths: &ServicePaths) -> Result<Value, String> {
        status_with(
            paths,
            || is_loaded(SERVICE_LABEL),
            || is_loaded(WATCHDOG_LABEL),
            || is_loaded(HEALTH_WATCHDOG_LABEL),
            |kind, loaded| {
                if kind == ServiceKind::Rust && loaded {
                    health_once(DEFAULT_PORT)
                } else {
                    false
                }
            },
        )
    }

    fn status_with<ServiceLoaded, LegacyLoaded, HealthLoaded, Healthy>(
        paths: &ServicePaths,
        service_loaded: ServiceLoaded,
        legacy_watchdog_loaded: LegacyLoaded,
        health_watchdog_loaded: HealthLoaded,
        healthy: Healthy,
    ) -> Result<Value, String>
    where
        ServiceLoaded: FnOnce() -> bool,
        LegacyLoaded: FnOnce() -> bool,
        HealthLoaded: FnOnce() -> bool,
        Healthy: FnOnce(ServiceKind, bool) -> bool,
    {
        let bytes = read_optional_bounded(&paths.plist, 256 * 1024)?;
        let descriptor = describe_service(bytes.as_deref(), paths)?;
        let loaded = service_loaded();
        let health = healthy(descriptor.kind, loaded);
        let generation = descriptor.env.get("HERDR_MCP_RUNTIME_GENERATION").cloned();
        let implementation = match descriptor.kind {
            ServiceKind::Missing => "missing",
            ServiceKind::Rust => "rust",
            ServiceKind::Node => "node",
            ServiceKind::Other => "other",
        };
        Ok(json!({
            "ok": descriptor.kind == ServiceKind::Rust && loaded && health,
            "label": SERVICE_LABEL,
            "implementation": implementation,
            "loaded": loaded,
            "healthy": health,
            "generation": generation,
            "current_target": current_target(paths)?.map(|path| path.to_string_lossy().into_owned()),
            "plist": paths.plist,
            "extension_socket": paths.extension_socket,
            "legacy_watchdog_present": paths.watchdog_plist.exists(),
            "legacy_watchdog_loaded": legacy_watchdog_loaded(),
            "health_watchdog_label": HEALTH_WATCHDOG_LABEL,
            "health_watchdog_present": paths.health_watchdog_plist.exists(),
            "health_watchdog_loaded": health_watchdog_loaded(),
        }))
    }

    fn start(paths: &ServicePaths) -> Result<Value, String> {
        let descriptor = require_rust_service(paths)?;
        if is_loaded(SERVICE_LABEL) {
            kickstart()?;
        } else {
            bootstrap_with_retry(&paths.plist, SERVICE_LABEL)?;
        }
        wait_for_health(DEFAULT_PORT)?;
        let evidence_recorded = record_action(paths, "start", "ok", &descriptor, None);
        Ok(json!({
            "ok": true,
            "action": "start",
            "label": SERVICE_LABEL,
            "evidence_recorded": evidence_recorded,
        }))
    }

    fn stop(paths: &ServicePaths) -> Result<Value, String> {
        let descriptor = require_rust_service(paths)?;
        if is_loaded(SERVICE_LABEL) {
            bootout(SERVICE_LABEL)?;
        }
        let evidence_recorded = record_action(paths, "stop", "ok", &descriptor, None);
        Ok(json!({
            "ok": true,
            "action": "stop",
            "label": SERVICE_LABEL,
            "evidence_recorded": evidence_recorded,
        }))
    }

    fn restart(paths: &ServicePaths) -> Result<Value, String> {
        let descriptor = require_rust_service(paths)?;
        if is_loaded(SERVICE_LABEL) {
            kickstart()?;
        } else {
            bootstrap_with_retry(&paths.plist, SERVICE_LABEL)?;
        }
        wait_for_health(DEFAULT_PORT)?;
        let evidence_recorded = record_action(paths, "restart", "ok", &descriptor, None);
        Ok(json!({
            "ok": true,
            "action": "restart",
            "label": SERVICE_LABEL,
            "evidence_recorded": evidence_recorded,
        }))
    }

    fn rollback(
        paths: &ServicePaths,
        mutation_lock: &ServiceMutationLock,
    ) -> Result<Value, String> {
        let current_bytes = read_optional_bounded(&paths.plist, 256 * 1024)?
            .ok_or_else(|| "Rust service plist is missing".to_owned())?;
        let current = require_rust_service(paths)?;
        let current_generation = current
            .env
            .get("HERDR_MCP_RUNTIME_GENERATION")
            .cloned()
            .ok_or_else(|| "Rust service generation identity is missing".to_owned())?;
        let current_target = current_target(paths)?
            .ok_or_else(|| "Rust service runtime/current pointer is missing".to_owned())?;
        if !is_owned_generation_target(&current_target) {
            return Err("Rust service runtime/current pointer is not managed".to_owned());
        }
        let current_was_loaded = is_loaded(SERVICE_LABEL);

        let mut store = StateStore::open_in_dir(&paths.config_dir, "state")?;
        let rollback = store
            .latest_ready_service_rollback()?
            .ok_or_else(|| "no ready service rollback is available".to_owned())?;

        let reject_ready = |reason: String| -> Result<Value, String> { Err(reason) };

        if rollback.activated_generation_id != current_generation {
            return reject_ready(format!(
                "ready rollback {} targets generation {}, but current service is {}",
                rollback.rollback_id, rollback.activated_generation_id, current_generation
            ));
        }
        let active_generation = store.active_runtime_generation()?;
        if active_generation
            .as_ref()
            .map(|generation| generation.generation_id.as_str())
            != Some(current_generation.as_str())
        {
            return reject_ready(format!(
                "runtime generation ledger is not aligned with current service generation {current_generation}"
            ));
        }

        let Some(server_backup) = rollback.server_plist_backup.as_deref() else {
            return reject_ready(format!(
                "rollback {} has no server plist backup",
                rollback.rollback_id
            ));
        };
        let source_bytes = match read_owned_backup(paths, server_backup) {
            Ok(bytes) => bytes,
            Err(error) => return reject_ready(error),
        };
        let source = match describe_service(Some(&source_bytes), paths) {
            Ok(source) => source,
            Err(error) => return reject_ready(error),
        };
        let source_kind = service_kind_name(&source.kind);
        if source_kind != rollback.source_kind {
            return reject_ready(format!(
                "rollback source mismatch: ledger={}, backup={source_kind}",
                rollback.source_kind
            ));
        }
        if !matches!(source.kind, ServiceKind::Node | ServiceKind::Rust) {
            return reject_ready(
                "rollback server backup is not an owned Node/Rust service".to_owned(),
            );
        }

        let watchdog_bytes = match rollback.watchdog_plist_backup.as_deref() {
            Some(path) => match read_owned_backup(paths, path) {
                Ok(bytes) => {
                    if !watchdog_is_legacy_owned(Some(&bytes))? {
                        return reject_ready(
                            "rollback watchdog backup is not legacy herdr-mcp watchdog".to_owned(),
                        );
                    }
                    Some(bytes)
                }
                Err(error) => return reject_ready(error),
            },
            None => None,
        };
        if rollback.watchdog_was_loaded && watchdog_bytes.is_none() {
            return reject_ready(
                "rollback requires a loaded watchdog but has no watchdog backup".to_owned(),
            );
        }

        let previous_target = rollback
            .previous_current_target
            .as_deref()
            .map(PathBuf::from);
        if previous_target
            .as_deref()
            .is_some_and(|target| !is_owned_generation_target(target))
        {
            return reject_ready(
                "rollback previous runtime/current target is not managed".to_owned(),
            );
        }
        let previous_generation_id = if source.kind == ServiceKind::Rust {
            previous_target
                .as_deref()
                .and_then(generation_id_from_target)
                .map(str::to_owned)
                .ok_or_else(|| {
                    "Rust rollback source has no managed previous generation target".to_owned()
                })?
                .into()
        } else {
            None
        };

        let current_backup = match backup_bytes(paths, "guardian-current-server", &current_bytes) {
            Ok(path) => path,
            Err(error) => return reject_ready(error),
        };
        let transaction_id = new_guardian_transaction_id()?;
        let mut guardian = match arm_guardian(
            paths,
            mutation_lock,
            GuardianRecord {
                transaction_id: transaction_id.clone(),
                mode: GuardianMode::Rollback,
                state: "armed".to_owned(),
                parent_pid: std::process::id(),
                created_at: now_ms_i64(),
                rollback_id: Some(rollback.rollback_id.clone()),
                candidate_generation_id: Some(current_generation.clone()),
                server_plist_backup: Some(current_backup),
                watchdog_plist_backup: None,
                previous_current_target: Some(current_target.to_string_lossy().into_owned()),
                server_was_loaded: current_was_loaded,
                watchdog_was_loaded: false,
                detail: None,
            },
        ) {
            Ok(guardian) => guardian,
            Err(error) => {
                return Err(format!(
                    "service guardian could not arm before rollback mutation: {error}"
                ));
            }
        };

        let rollback_snapshot = rollback.clone();
        let rollback = match store.claim_service_rollback(&rollback.rollback_id) {
            Ok(claimed) => claimed,
            Err(error) => {
                let settlement = guardian.finish(
                    "parent_recovered",
                    "rollback claim was not acquired; current service remained unchanged",
                );
                return Err(match settlement {
                    Ok(()) => format!(
                        "service rollback could not be claimed after guardian handshake: {error}"
                    ),
                    Err(settlement_error) => format!(
                        "service rollback could not be claimed after guardian handshake: {error}; guardian settlement also failed: {settlement_error}"
                    ),
                });
            }
        };
        let mut expected_claimed = rollback_snapshot;
        expected_claimed.state = "consuming".to_owned();
        if rollback != expected_claimed {
            let release =
                store.finish_service_rollback(&rollback.rollback_id, "ready", now_ms_i64());
            let settlement = guardian.finish(
                "parent_recovered",
                "rollback ledger changed after preflight; current service remained unchanged",
            );
            return Err(match (release, settlement) {
                (Ok(()), Ok(())) => {
                    "service rollback ledger changed between preflight and claim".to_owned()
                }
                (Err(release_error), Ok(())) => format!(
                    "service rollback ledger changed between preflight and claim; rollback claim could not be released: {release_error}"
                ),
                (Ok(()), Err(settlement_error)) => format!(
                    "service rollback ledger changed between preflight and claim; guardian settlement also failed: {settlement_error}"
                ),
                (Err(release_error), Err(settlement_error)) => format!(
                    "service rollback ledger changed between preflight and claim; rollback claim could not be released: {release_error}; guardian settlement also failed: {settlement_error}"
                ),
            });
        }

        let mut current_bootout_pending = false;
        let apply = (|| -> Result<(), String> {
            if current_was_loaded {
                current_bootout_pending = request_bootout(SERVICE_LABEL)?;
                if current_bootout_pending {
                    wait_launchd_absent(SERVICE_LABEL, LAUNCHD_BOOTOUT_BUDGET)?;
                    current_bootout_pending = false;
                }
            }
            atomic_write(&paths.plist, &source_bytes, 0o600)?;
            restore_current(paths, previous_target.as_deref())?;
            if let Some(bytes) = watchdog_bytes.as_deref() {
                atomic_write(&paths.watchdog_plist, bytes, 0o600)?;
            }
            if rollback.server_was_loaded {
                bootstrap_with_retry(&paths.plist, SERVICE_LABEL)?;
                wait_for_service_health(&source, DEFAULT_PORT)?;
            }
            if rollback.watchdog_was_loaded {
                bootstrap_with_retry(&paths.watchdog_plist, WATCHDOG_LABEL)?;
            }
            store.complete_service_rollback(
                &rollback.rollback_id,
                &rollback.activated_generation_id,
                previous_generation_id.as_deref(),
                now_ms_i64(),
            )?;
            Ok(())
        })();

        if let Err(error) = apply {
            let restore_error = restore_current_rust_after_failed_rollback(
                paths,
                &current_bytes,
                &current_target,
                current_was_loaded,
                current_bootout_pending,
            )
            .err();
            let state = if restore_error.is_some() {
                "rollback_failed"
            } else {
                "ready"
            };
            let _ = store.finish_service_rollback(&rollback.rollback_id, state, now_ms_i64());
            if restore_error.is_none() {
                let _ = guardian.finish(
                    "parent_recovered",
                    "service manager restored the current Rust service after rollback failure",
                );
            }
            return Err(match restore_error {
                Some(restore_error) => format!(
                    "service rollback failed: {error}; restoring current Rust service also failed: {restore_error}; one-shot guardian remains armed for known-good recovery"
                ),
                None => format!(
                    "service rollback failed but current Rust service was restored: {error}"
                ),
            });
        }

        let evidence_recorded = store
            .record_service_event(
                "rollback",
                "ok",
                Some(&rollback.activated_generation_id),
                now_ms_i64(),
                Some(&format!("restored-{}", rollback.source_kind)),
            )
            .is_ok();
        let guardian_settled = guardian
            .finish(
                "committed",
                "service rollback committed and previous implementation passed its health gate",
            )
            .is_ok();
        Ok(json!({
            "ok": true,
            "action": "rollback",
            "rollback_id": rollback.rollback_id,
            "from_generation": rollback.activated_generation_id,
            "restored_implementation": rollback.source_kind,
            "restored_loaded": rollback.server_was_loaded,
            "restored_watchdog": rollback.watchdog_was_loaded,
            "guardian_transaction": transaction_id,
            "guardian_settled": guardian_settled,
            "evidence_recorded": evidence_recorded,
        }))
    }

    fn uninstall(paths: &ServicePaths) -> Result<Value, String> {
        let descriptor = require_rust_service(paths)?;
        if is_loaded(SERVICE_LABEL) {
            bootout(SERVICE_LABEL)?;
        }
        remove_regular_file(&paths.plist)?;
        let user_cli_removed = user_cli::remove_link_if_owned(&paths.home, &paths.current_binary)?;
        remove_current_if_owned(paths)?;
        let evidence_recorded = record_action(paths, "uninstall", "ok", &descriptor, None);
        Ok(json!({
            "ok": true,
            "action": "uninstall",
            "label": SERVICE_LABEL,
            "generations_preserved": true,
            "user_cli_removed": user_cli_removed,
            "evidence_recorded": evidence_recorded,
        }))
    }

    fn require_rust_service(paths: &ServicePaths) -> Result<ServiceDescriptor, String> {
        let bytes = read_optional_bounded(&paths.plist, 256 * 1024)?;
        let descriptor = describe_service(bytes.as_deref(), paths)?;
        if descriptor.kind != ServiceKind::Rust {
            return Err("service is not installed as an owned Rust service".to_owned());
        }
        if paths.watchdog_plist.exists() || is_loaded(WATCHDOG_LABEL) {
            return Err(
                "legacy watchdog is present; refusing dual supervisor ownership".to_owned(),
            );
        }
        Ok(descriptor)
    }

    fn record_action(
        paths: &ServicePaths,
        action: &str,
        outcome: &str,
        descriptor: &ServiceDescriptor,
        detail: Option<&str>,
    ) -> bool {
        StateStore::open_in_dir(&paths.config_dir, "state")
            .and_then(|store| {
                store.record_service_event(
                    action,
                    outcome,
                    descriptor
                        .env
                        .get("HERDR_MCP_RUNTIME_GENERATION")
                        .map(String::as_str),
                    now_ms_i64(),
                    detail,
                )
            })
            .is_ok()
    }

    fn service_environment(
        paths: &ServicePaths,
        inherited: &BTreeMap<String, String>,
        generation: &PreparedGeneration,
    ) -> Result<BTreeMap<String, String>, String> {
        let mut out = BTreeMap::new();
        let preserve = [
            "HERDR_MCP_BASE_URL",
            "HERDR_MCP_CONTRACT_PROFILE",
            "HERDR_SKILL_NETWORK",
            "HERDR_SOCKET_PATH",
            "PATH",
        ];
        for key in preserve {
            if let Some(value) = inherited.get(key).filter(|value| !value.is_empty()) {
                out.insert(key.to_owned(), value.clone());
            }
        }
        let token = inherited
            .get("HERDR_MCP_TOKEN")
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or(secure_token_hex()?);
        out.insert("HERDR_MCP_TOKEN".to_owned(), token);
        out.insert("HERDR_MCP_HOST".to_owned(), "127.0.0.1".to_owned());
        out.insert("HERDR_MCP_PORT".to_owned(), DEFAULT_PORT.to_string());
        out.insert(
            "HERDR_MCP_CONTRACT_PROFILE".to_owned(),
            out.get("HERDR_MCP_CONTRACT_PROFILE")
                .cloned()
                .unwrap_or_else(|| "epoch2".to_owned()),
        );
        out.insert(
            "HERDR_SKILL_NETWORK".to_owned(),
            out.get("HERDR_SKILL_NETWORK")
                .cloned()
                .unwrap_or_else(|| "1".to_owned()),
        );
        out.insert(
            "HERDR_SOCKET_PATH".to_owned(),
            out.get("HERDR_SOCKET_PATH")
                .cloned()
                .unwrap_or_else(|| paths.herdr_socket.to_string_lossy().into_owned()),
        );
        out.insert(
            "PATH".to_owned(),
            out.get("PATH").cloned().unwrap_or_else(|| {
                format!(
                    "{}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
                    paths.home.display()
                )
            }),
        );
        out.insert("HOME".to_owned(), paths.home.to_string_lossy().into_owned());
        out.insert(
            "HERDR_MCP_STATE_DIR".to_owned(),
            paths.config_dir.to_string_lossy().into_owned(),
        );
        out.insert(
            "HERDR_EXTENSION_IPC_SOCKET".to_owned(),
            paths.extension_socket.to_string_lossy().into_owned(),
        );
        out.insert(
            "HERDR_MCP_RUNTIME_GENERATION".to_owned(),
            generation.generation_id.clone(),
        );
        out.insert("HERDR_MCP_SERVICE_IMPL".to_owned(), SERVICE_IMPL.to_owned());
        Ok(out)
    }

    fn encode_service_plist(
        paths: &ServicePaths,
        env: &BTreeMap<String, String>,
    ) -> Result<Vec<u8>, String> {
        let mut root = Dictionary::new();
        root.insert(
            "Label".to_owned(),
            PlistValue::String(SERVICE_LABEL.to_owned()),
        );
        root.insert(
            "ProgramArguments".to_owned(),
            PlistValue::Array(vec![
                PlistValue::String(paths.current_binary.to_string_lossy().into_owned()),
                PlistValue::String("candidate".to_owned()),
                PlistValue::String("--port".to_owned()),
                PlistValue::String(DEFAULT_PORT.to_string()),
            ]),
        );
        root.insert(
            "WorkingDirectory".to_owned(),
            PlistValue::String(paths.config_dir.to_string_lossy().into_owned()),
        );
        let mut env_dict = Dictionary::new();
        for (key, value) in env {
            env_dict.insert(key.clone(), PlistValue::String(value.clone()));
        }
        root.insert(
            "EnvironmentVariables".to_owned(),
            PlistValue::Dictionary(env_dict),
        );
        root.insert("RunAtLoad".to_owned(), PlistValue::Boolean(true));
        root.insert("KeepAlive".to_owned(), PlistValue::Boolean(true));
        root.insert(
            "ProcessType".to_owned(),
            PlistValue::String("Interactive".to_owned()),
        );
        root.insert(
            "StandardOutPath".to_owned(),
            PlistValue::String(paths.log_path.to_string_lossy().into_owned()),
        );
        root.insert(
            "StandardErrorPath".to_owned(),
            PlistValue::String(paths.log_path.to_string_lossy().into_owned()),
        );
        let mut bytes = Vec::new();
        PlistValue::Dictionary(root)
            .to_writer_xml(&mut bytes)
            .map_err(|error| format!("cannot encode launchd plist: {error}"))?;
        Ok(bytes)
    }

    fn describe_service(
        bytes: Option<&[u8]>,
        paths: &ServicePaths,
    ) -> Result<ServiceDescriptor, String> {
        let Some(bytes) = bytes else {
            return Ok(ServiceDescriptor {
                kind: ServiceKind::Missing,
                env: BTreeMap::new(),
            });
        };
        let value = PlistValue::from_reader(Cursor::new(bytes))
            .map_err(|error| format!("cannot parse service plist: {error}"))?;
        let dict = value
            .as_dictionary()
            .ok_or_else(|| "service plist root is not a dictionary".to_owned())?;
        let label = dict.get("Label").and_then(PlistValue::as_string);
        let program_arguments = dict
            .get("ProgramArguments")
            .and_then(PlistValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(PlistValue::as_string)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let env = dict
            .get("EnvironmentVariables")
            .and_then(PlistValue::as_dictionary)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|(key, value)| {
                        value
                            .as_string()
                            .map(|value| (key.clone(), value.to_owned()))
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let rust_owned = label == Some(SERVICE_LABEL)
            && program_arguments.first().map(String::as_str)
                == Some(paths.current_binary.to_string_lossy().as_ref())
            && program_arguments.get(1).map(String::as_str) == Some("candidate")
            && env.get("HERDR_MCP_SERVICE_IMPL").map(String::as_str) == Some(SERVICE_IMPL);
        let node_owned = label == Some(SERVICE_LABEL)
            && program_arguments
                .first()
                .and_then(|value| Path::new(value).file_name())
                .is_some_and(|value| value == OsStr::new("node"))
            && program_arguments
                .get(1)
                .is_some_and(|value| value.ends_with("/dist/server.js"))
            && env
                .get("HERDR_MCP_TOKEN")
                .is_some_and(|value| !value.is_empty());
        Ok(ServiceDescriptor {
            kind: if rust_owned {
                ServiceKind::Rust
            } else if node_owned {
                ServiceKind::Node
            } else {
                ServiceKind::Other
            },
            env,
        })
    }

    fn watchdog_is_legacy_owned(bytes: Option<&[u8]>) -> Result<bool, String> {
        let Some(bytes) = bytes else {
            return Ok(false);
        };
        let value = PlistValue::from_reader(Cursor::new(bytes))
            .map_err(|error| format!("cannot parse watchdog plist: {error}"))?;
        let Some(dict) = value.as_dictionary() else {
            return Ok(false);
        };
        if dict.get("Label").and_then(PlistValue::as_string) != Some(WATCHDOG_LABEL) {
            return Ok(false);
        }
        let args = dict
            .get("ProgramArguments")
            .and_then(PlistValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(PlistValue::as_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        // Basename must be exactly `watchdog.sh`. A trailing `ends_with("watchdog.sh")`
        // would also match the Rust-era `health-watchdog.sh` sidecar and falsely
        // treat it as the legacy Node supervisor reserved for adoption/rollback.
        let runs_legacy_script = args.iter().any(|value| {
            Path::new(value)
                .file_name()
                .is_some_and(|name| name == OsStr::new("watchdog.sh"))
        });
        Ok(runs_legacy_script && args.contains(&"once"))
    }

    fn prepare_generation(paths: &ServicePaths) -> Result<PreparedGeneration, String> {
        ensure_secure_dir(&paths.config_dir)?;
        ensure_secure_dir(&paths.runtime_root)?;
        ensure_secure_dir(&paths.generations_dir)?;
        let sha256 = file_sha256(&paths.source_binary)?;
        let generation_id = format!("rust-{}", &sha256[..16]);
        let generation_dir = paths.generations_dir.join(&generation_id);
        ensure_secure_dir(&generation_dir)?;
        let binary = generation_dir.join("herdr-mcp");
        atomic_copy_executable(&paths.source_binary, &binary)?;
        let copied = file_sha256(&binary)?;
        if copied != sha256 {
            return Err("runtime generation checksum mismatch after copy".to_owned());
        }
        Ok(PreparedGeneration {
            generation_id,
            sha256,
            binary,
        })
    }

    fn switch_current(paths: &ServicePaths, generation: &PreparedGeneration) -> Result<(), String> {
        let target = PathBuf::from("generations").join(&generation.generation_id);
        if let Some(existing) = current_target(paths)?
            && !is_owned_generation_target(&existing)
        {
            return Err(format!(
                "runtime/current points outside managed generations: {}",
                existing.display()
            ));
        }
        let temp =
            paths
                .runtime_root
                .join(format!(".current-{}-{}", std::process::id(), now_ms_i64()));
        if temp.exists() {
            return Err(format!(
                "temporary generation link already exists: {}",
                temp.display()
            ));
        }
        symlink(&target, &temp)
            .map_err(|error| format!("cannot create generation symlink: {error}"))?;
        fs::rename(&temp, &paths.current_link)
            .map_err(|error| format!("cannot activate generation symlink: {error}"))?;
        Ok(())
    }

    fn current_target(paths: &ServicePaths) -> Result<Option<PathBuf>, String> {
        match fs::symlink_metadata(&paths.current_link) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = fs::read_link(&paths.current_link)
                    .map_err(|error| format!("cannot read runtime/current symlink: {error}"))?;
                Ok(Some(target))
            }
            Ok(_) => Err("runtime/current exists but is not a symlink".to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("cannot inspect runtime/current: {error}")),
        }
    }

    fn is_owned_generation_target(target: &Path) -> bool {
        let mut components = target.components();
        matches!(components.next(), Some(Component::Normal(value)) if value == OsStr::new("generations"))
            && matches!(components.next(), Some(Component::Normal(value)) if value.to_string_lossy().starts_with("rust-"))
            && components.next().is_none()
    }

    fn generation_id_from_target(target: &Path) -> Option<&str> {
        let mut components = target.components();
        if !matches!(components.next(), Some(Component::Normal(value)) if value == OsStr::new("generations"))
        {
            return None;
        }
        let id = match components.next() {
            Some(Component::Normal(value)) => value.to_str()?,
            _ => return None,
        };
        (id.starts_with("rust-") && components.next().is_none()).then_some(id)
    }

    fn restore_current(paths: &ServicePaths, previous: Option<&Path>) -> Result<(), String> {
        match previous {
            Some(previous) => {
                if !is_owned_generation_target(previous) {
                    return Err("refusing to restore unmanaged runtime/current target".to_owned());
                }
                let temp = paths.runtime_root.join(format!(
                    ".rollback-current-{}-{}",
                    std::process::id(),
                    now_ms_i64()
                ));
                symlink(previous, &temp).map_err(|error| {
                    format!("cannot create rollback generation symlink: {error}")
                })?;
                fs::rename(&temp, &paths.current_link)
                    .map_err(|error| format!("cannot restore runtime/current: {error}"))
            }
            None => {
                if paths.current_link.exists() || fs::symlink_metadata(&paths.current_link).is_ok()
                {
                    fs::remove_file(&paths.current_link)
                        .map_err(|error| format!("cannot remove runtime/current: {error}"))?;
                }
                Ok(())
            }
        }
    }

    fn remove_current_if_owned(paths: &ServicePaths) -> Result<(), String> {
        let Some(target) = current_target(paths)? else {
            return Ok(());
        };
        if !is_owned_generation_target(&target) {
            return Err("refusing to remove unmanaged runtime/current target".to_owned());
        }
        fs::remove_file(&paths.current_link)
            .map_err(|error| format!("cannot remove runtime/current: {error}"))
    }

    fn rollback_install(
        paths: &ServicePaths,
        rollback: &RollbackState,
        server_bootout_pending: bool,
    ) -> Result<(), String> {
        settle_service_for_restore(SERVICE_LABEL, server_bootout_pending)?;
        match rollback.server_plist.as_deref() {
            Some(bytes) => atomic_write(&paths.plist, bytes, 0o600)?,
            None => remove_regular_file(&paths.plist)?,
        }
        restore_current(paths, rollback.previous_current.as_deref())?;
        if let Some(bytes) = rollback.watchdog_plist.as_deref() {
            atomic_write(&paths.watchdog_plist, bytes, 0o600)?;
        }
        if rollback.server_was_loaded && rollback.server_plist.is_some() {
            bootstrap_with_retry(&paths.plist, SERVICE_LABEL)?;
        }
        if rollback.watchdog_was_loaded && rollback.watchdog_plist.is_some() {
            bootstrap_with_retry(&paths.watchdog_plist, WATCHDOG_LABEL)?;
        }
        Ok(())
    }

    fn backup_bytes(paths: &ServicePaths, kind: &str, bytes: &[u8]) -> Result<String, String> {
        ensure_secure_dir(&paths.backups_dir)?;
        let path = paths
            .backups_dir
            .join(format!("{SERVICE_LABEL}.{kind}.{}.plist", now_ms_i64()));
        atomic_write(&path, bytes, 0o600)?;
        Ok(path.to_string_lossy().into_owned())
    }

    fn read_owned_backup(paths: &ServicePaths, value: &str) -> Result<Vec<u8>, String> {
        let path = PathBuf::from(value);
        if path.parent() != Some(paths.backups_dir.as_path()) {
            return Err(format!(
                "rollback backup is outside managed backup directory: {}",
                path.display()
            ));
        }
        read_optional_bounded(&path, 256 * 1024)?
            .ok_or_else(|| format!("rollback backup is missing: {}", path.display()))
    }

    fn restore_current_rust_after_failed_rollback(
        paths: &ServicePaths,
        current_plist: &[u8],
        current_target: &Path,
        current_was_loaded: bool,
        current_bootout_pending: bool,
    ) -> Result<(), String> {
        if is_loaded(WATCHDOG_LABEL) {
            bootout(WATCHDOG_LABEL)?;
        }
        remove_regular_file(&paths.watchdog_plist)?;
        settle_service_for_restore(SERVICE_LABEL, current_bootout_pending)?;
        atomic_write(&paths.plist, current_plist, 0o600)?;
        restore_current(paths, Some(current_target))?;
        if current_was_loaded {
            bootstrap_with_retry(&paths.plist, SERVICE_LABEL)?;
            wait_for_health(DEFAULT_PORT)?;
        }
        Ok(())
    }

    fn read_optional_bounded(path: &Path, max: usize) -> Result<Option<Vec<u8>>, String> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!("{} must be a regular file", path.display()));
        }
        if metadata.len() > max as u64 {
            return Err(format!("{} exceeds {} bytes", path.display(), max));
        }
        fs::read(path)
            .map(Some)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))
    }

    fn remove_regular_file(path: &Path) -> Result<(), String> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                fs::remove_file(path)
                    .map_err(|error| format!("cannot remove {}: {error}", path.display()))
            }
            Ok(_) => Err(format!(
                "refusing to remove non-regular path {}",
                path.display()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
        }
    }

    fn ensure_secure_dir(path: &Path) -> Result<(), String> {
        if let Ok(metadata) = fs::symlink_metadata(path)
            && metadata.file_type().is_symlink()
        {
            return Err(format!("{} must not be a symlink", path.display()));
        }
        fs::create_dir_all(path)
            .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure {}: {error}", path.display()))
    }

    fn atomic_copy_executable(source: &Path, target: &Path) -> Result<(), String> {
        let parent = target
            .parent()
            .ok_or_else(|| "runtime binary has no parent directory".to_owned())?;
        ensure_secure_dir(parent)?;
        if let Ok(metadata) = fs::symlink_metadata(target)
            && metadata.file_type().is_symlink()
        {
            return Err(format!(
                "runtime binary {} must not be a symlink",
                target.display()
            ));
        }
        let bytes = fs::read(source)
            .map_err(|error| format!("cannot read source runtime {}: {error}", source.display()))?;
        atomic_write(target, &bytes, 0o700)
    }

    fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        if let Ok(metadata) = fs::symlink_metadata(path)
            && metadata.file_type().is_symlink()
        {
            return Err(format!("{} must not be a symlink", path.display()));
        }
        let temp = parent.join(format!(
            ".{}.tmp-{}-{}",
            path.file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("herdr-mcp"),
            std::process::id(),
            now_ms_i64()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(mode);
        let mut file = options
            .open(&temp)
            .map_err(|error| format!("cannot create {}: {error}", temp.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("cannot write {}: {error}", temp.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync {}: {error}", temp.display()))?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))
            .map_err(|error| format!("cannot chmod {}: {error}", temp.display()))?;
        fs::rename(&temp, path)
            .map_err(|error| format!("cannot replace {}: {error}", path.display()))?;
        Ok(())
    }

    fn file_sha256(path: &Path) -> Result<String, String> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!("{} must be a regular file", path.display()));
        }
        let mut file =
            File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("cannot hash {}: {error}", path.display()))?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        Ok(format!("{:x}", hash.finalize()))
    }

    fn secure_token_hex() -> Result<String, String> {
        let mut bytes = [0_u8; 32];
        File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut bytes))
            .map_err(|error| format!("cannot read secure random token: {error}"))?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    fn now_ms_i64() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(i64::MAX)
    }

    fn domain() -> String {
        format!("gui/{}", unsafe { libc::getuid() })
    }

    fn target(label: &str) -> String {
        format!("{}/{}", domain(), label)
    }

    fn is_loaded(label: &str) -> bool {
        Command::new("/bin/launchctl")
            .args(["print", &target(label)])
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn wait_launchd_absent(label: &str, budget: Duration) -> Result<(), String> {
        let deadline = Instant::now() + budget;
        loop {
            if !is_loaded(label) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "launchd service {label} remained loaded for {}ms after bootout",
                    budget.as_millis()
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn request_bootout(label: &str) -> Result<bool, String> {
        if !is_loaded(label) {
            return Ok(false);
        }
        run_launchctl([OsStr::new("bootout"), OsStr::new(&target(label))])?;
        Ok(true)
    }

    fn settle_service_for_restore(label: &str, bootout_pending: bool) -> Result<(), String> {
        settle_service_for_restore_with(
            bootout_pending,
            || is_loaded(label),
            || {
                if request_bootout(label)? {
                    wait_launchd_absent(label, LAUNCHD_RECOVERY_BUDGET)?;
                }
                Ok(())
            },
            || wait_launchd_absent(label, LAUNCHD_RECOVERY_BUDGET),
        )
    }

    fn settle_service_for_restore_with<Loaded, Stop, AwaitPending>(
        bootout_pending: bool,
        mut is_loaded: Loaded,
        mut stop: Stop,
        mut await_pending: AwaitPending,
    ) -> Result<(), String>
    where
        Loaded: FnMut() -> bool,
        Stop: FnMut() -> Result<(), String>,
        AwaitPending: FnMut() -> Result<(), String>,
    {
        if bootout_pending {
            return await_pending();
        }
        if is_loaded() {
            stop()?;
        }
        Ok(())
    }

    fn bootstrap_with_retry(plist: &Path, label: &str) -> Result<(), String> {
        bootstrap_retry_with(
            &BOOTSTRAP_RETRY_DELAYS,
            || wait_launchd_absent(label, LAUNCHD_ABSENT_BUDGET),
            || {
                run_launchctl([
                    OsStr::new("bootstrap"),
                    OsStr::new(&domain()),
                    plist.as_os_str(),
                ])
                .map(|_| ())
            },
            thread::sleep,
        )
        .map(|_| ())
    }

    fn bootstrap_retry_with<Absent, Bootstrap, Sleep>(
        delays: &[Duration],
        mut wait_absent: Absent,
        mut bootstrap: Bootstrap,
        mut sleep: Sleep,
    ) -> Result<usize, String>
    where
        Absent: FnMut() -> Result<(), String>,
        Bootstrap: FnMut() -> Result<(), String>,
        Sleep: FnMut(Duration),
    {
        let mut last_error = "launchctl bootstrap did not run".to_owned();
        for attempt in 0..=delays.len() {
            match wait_absent().and_then(|_| bootstrap()) {
                Ok(()) => return Ok(attempt + 1),
                Err(error) => last_error = error,
            }
            if let Some(delay) = delays.get(attempt).copied() {
                sleep(delay);
            }
        }
        Err(format!(
            "launchctl bootstrap failed after {} attempts: {last_error}",
            delays.len() + 1
        ))
    }

    fn bootout(label: &str) -> Result<(), String> {
        if request_bootout(label)? {
            wait_launchd_absent(label, LAUNCHD_BOOTOUT_BUDGET)?;
        }
        Ok(())
    }

    fn kickstart() -> Result<(), String> {
        run_launchctl([
            OsStr::new("kickstart"),
            OsStr::new("-k"),
            OsStr::new(&target(SERVICE_LABEL)),
        ])
        .map(|_| ())
    }

    fn run_launchctl<I, S>(args: I) -> Result<Output, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new("/bin/launchctl")
            .args(args)
            .output()
            .map_err(|error| format!("cannot execute launchctl: {error}"))?;
        if output.status.success() {
            return Ok(output);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim().chars().take(400).collect::<String>();
        Err(format!("launchctl failed: {detail}"))
    }

    fn health_once(port: u16) -> bool {
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(800))
            .build()
        {
            Ok(client) => client,
            Err(_) => return false,
        };
        client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .ok()
            .filter(|response| response.status().is_success())
            .and_then(|response| response.text().ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .is_some_and(|value| value.get("ok").and_then(Value::as_bool) == Some(true))
    }

    fn mcp_discover_once(descriptor: &ServiceDescriptor, port: u16) -> bool {
        let Some(token) = descriptor
            .env
            .get("HERDR_MCP_TOKEN")
            .filter(|value| !value.is_empty())
        else {
            return false;
        };
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(900))
            .build()
        {
            Ok(client) => client,
            Err(_) => return false,
        };
        client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(
                r#"{"jsonrpc":"2.0","id":"service-health","method":"server/discover","params":{}}"#,
            )
            .send()
            .ok()
            .filter(|response| response.status().is_success())
            .and_then(|response| response.text().ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .is_some_and(|value| value.get("result").is_some())
    }

    fn wait_for_service_health(descriptor: &ServiceDescriptor, port: u16) -> Result<(), String> {
        let deadline = Instant::now() + HEALTH_BUDGET;
        while Instant::now() < deadline {
            let healthy = match descriptor.kind {
                ServiceKind::Rust => health_once(port),
                ServiceKind::Node => mcp_discover_once(descriptor, port),
                _ => false,
            };
            if healthy {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(150));
        }
        Err(format!(
            "restored {} service did not become healthy on 127.0.0.1:{port} within {}s",
            service_kind_name(&descriptor.kind),
            HEALTH_BUDGET.as_secs()
        ))
    }

    fn wait_for_health(port: u16) -> Result<(), String> {
        let deadline = Instant::now() + HEALTH_BUDGET;
        while Instant::now() < deadline {
            if health_once(port) {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(150));
        }
        Err(format!(
            "Rust service did not become healthy on 127.0.0.1:{port} within {}s",
            HEALTH_BUDGET.as_secs()
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);

        #[test]
        fn service_status_is_the_only_command_safe_inside_managed_exec() {
            assert!(!service_command_requires_independent_process(
                &ServiceCommand::Status
            ));
            assert!(service_command_requires_independent_process(
                &ServiceCommand::Install { adopt_node: false }
            ));
            assert!(service_command_requires_independent_process(
                &ServiceCommand::Install { adopt_node: true }
            ));
            assert!(service_command_requires_independent_process(
                &ServiceCommand::Start
            ));
            assert!(service_command_requires_independent_process(
                &ServiceCommand::Stop
            ));
            assert!(service_command_requires_independent_process(
                &ServiceCommand::Restart
            ));
            assert!(service_command_requires_independent_process(
                &ServiceCommand::Rollback
            ));
            assert!(service_command_requires_independent_process(
                &ServiceCommand::Uninstall
            ));
            assert!(service_command_requires_independent_process(
                &ServiceCommand::Guardian {
                    transaction_id: "gtx-1234-guardian".to_owned(),
                    parent_pid: 1234,
                }
            ));
            assert!(!service_command_requires_mutation_lock(
                &ServiceCommand::Status
            ));
            assert!(!service_command_requires_mutation_lock(
                &ServiceCommand::Guardian {
                    transaction_id: "gtx-1234-guardian".to_owned(),
                    parent_pid: 1234,
                }
            ));
            assert!(service_command_requires_mutation_lock(
                &ServiceCommand::Install { adopt_node: false }
            ));
            assert!(service_command_requires_mutation_lock(
                &ServiceCommand::Install { adopt_node: true }
            ));
            assert!(service_command_requires_mutation_lock(
                &ServiceCommand::Start
            ));
            assert!(service_command_requires_mutation_lock(
                &ServiceCommand::Stop
            ));
            assert!(service_command_requires_mutation_lock(
                &ServiceCommand::Restart
            ));
            assert!(service_command_requires_mutation_lock(
                &ServiceCommand::Rollback
            ));
            assert!(service_command_requires_mutation_lock(
                &ServiceCommand::Uninstall
            ));
        }

        fn root(label: &str) -> PathBuf {
            env::temp_dir().join(format!(
                "herdr-mcp-service-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
        }

        fn fixture() -> (PathBuf, ServicePaths) {
            let root = root("fixture");
            let home = root.join("home");
            let config = home.join(".config/herdr-mcp");
            fs::create_dir_all(&config).unwrap();
            let source = root.join("herdr-mcp-source");
            fs::write(&source, b"rust-binary-fixture").unwrap();
            fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
            let paths = ServicePaths::for_values(
                home.clone(),
                config,
                source,
                home.join(".config/herdr/herdr.sock"),
            );
            (root, paths)
        }

        fn node_plist(paths: &ServicePaths) -> Vec<u8> {
            let mut root = Dictionary::new();
            root.insert(
                "Label".to_owned(),
                PlistValue::String(SERVICE_LABEL.to_owned()),
            );
            root.insert(
                "ProgramArguments".to_owned(),
                PlistValue::Array(vec![
                    PlistValue::String("/usr/local/bin/node".to_owned()),
                    PlistValue::String("/tmp/herdr/dist/server.js".to_owned()),
                ]),
            );
            let mut env = Dictionary::new();
            env.insert(
                "HERDR_MCP_TOKEN".to_owned(),
                PlistValue::String("old-token".to_owned()),
            );
            env.insert(
                "HERDR_MCP_BASE_URL".to_owned(),
                PlistValue::String("https://edge.example".to_owned()),
            );
            env.insert(
                "HERDR_SOCKET_PATH".to_owned(),
                PlistValue::String(paths.herdr_socket.to_string_lossy().into_owned()),
            );
            root.insert(
                "EnvironmentVariables".to_owned(),
                PlistValue::Dictionary(env),
            );
            let mut bytes = Vec::new();
            PlistValue::Dictionary(root)
                .to_writer_xml(&mut bytes)
                .unwrap();
            bytes
        }

        #[test]
        fn node_adoption_preserves_token_and_edge_but_rust_plist_owns_runtime_paths() {
            let (root, paths) = fixture();
            let descriptor = describe_service(Some(&node_plist(&paths)), &paths).unwrap();
            assert_eq!(descriptor.kind, ServiceKind::Node);
            let generation = prepare_generation(&paths).unwrap();
            let env = service_environment(&paths, &descriptor.env, &generation).unwrap();
            assert_eq!(
                env.get("HERDR_MCP_TOKEN").map(String::as_str),
                Some("old-token")
            );
            assert_eq!(
                env.get("HERDR_MCP_BASE_URL").map(String::as_str),
                Some("https://edge.example")
            );
            assert_eq!(
                env.get("HERDR_EXTENSION_IPC_SOCKET").map(String::as_str),
                Some(paths.extension_socket.to_string_lossy().as_ref())
            );
            let plist = encode_service_plist(&paths, &env).unwrap();
            let rust = describe_service(Some(&plist), &paths).unwrap();
            assert_eq!(rust.kind, ServiceKind::Rust);
            let value = PlistValue::from_reader(Cursor::new(&plist)).unwrap();
            let args = value
                .as_dictionary()
                .unwrap()
                .get("ProgramArguments")
                .unwrap()
                .as_array()
                .unwrap();
            assert_eq!(
                args[0].as_string(),
                Some(paths.current_binary.to_string_lossy().as_ref())
            );
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn generation_copy_is_content_addressed_and_current_pointer_is_managed() {
            let (root, paths) = fixture();
            let generation = prepare_generation(&paths).unwrap();
            assert!(generation.generation_id.starts_with("rust-"));
            assert_eq!(file_sha256(&generation.binary).unwrap(), generation.sha256);
            switch_current(&paths, &generation).unwrap();
            let target = current_target(&paths).unwrap().unwrap();
            assert!(is_owned_generation_target(&target));
            assert_eq!(
                target,
                PathBuf::from("generations").join(&generation.generation_id)
            );
            remove_current_if_owned(&paths).unwrap();
            assert!(current_target(&paths).unwrap().is_none());
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn same_active_install_preflight_is_a_true_noop() {
            let (root, paths) = fixture();
            let generation = prepare_generation(&paths).unwrap();
            switch_current(&paths, &generation).unwrap();
            let env = service_environment(&paths, &BTreeMap::new(), &generation).unwrap();
            let plist = encode_service_plist(&paths, &env).unwrap();
            fs::create_dir_all(paths.plist.parent().unwrap()).unwrap();
            fs::write(&paths.plist, &plist).unwrap();

            let lock = ServiceMutationLock::acquire(&paths).unwrap();

            let binary_before = fs::read(&generation.binary).unwrap();
            let plist_before = fs::read(&paths.plist).unwrap();
            let current_before = fs::read_link(&paths.current_link).unwrap();
            let backups_before = paths.backups_dir.exists();
            let guardians_before = paths.guardians_dir.exists();
            let mut config_entries_before = fs::read_dir(&paths.config_dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            config_entries_before.sort();

            let result = install_with_noop_checks(&paths, false, &lock, || true, || true).unwrap();
            assert_eq!(result["ok"], true);
            assert_eq!(result["already_active"], true);
            assert_eq!(result["changed"], false);
            assert_eq!(result["generation"], generation.generation_id);
            assert_eq!(result["sha256"], generation.sha256);
            assert_eq!(fs::read(&generation.binary).unwrap(), binary_before);
            assert_eq!(fs::read(&paths.plist).unwrap(), plist_before);
            assert_eq!(fs::read_link(&paths.current_link).unwrap(), current_before);
            assert_eq!(paths.backups_dir.exists(), backups_before);
            assert_eq!(paths.guardians_dir.exists(), guardians_before);
            let mut config_entries_after = fs::read_dir(&paths.config_dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            config_entries_after.sort();
            assert_eq!(config_entries_after, config_entries_before);
            fs::remove_dir_all(root).unwrap();
        }

        fn rust_descriptor(generation_id: &str) -> ServiceDescriptor {
            ServiceDescriptor {
                kind: ServiceKind::Rust,
                env: BTreeMap::from([(
                    "HERDR_MCP_RUNTIME_GENERATION".to_owned(),
                    generation_id.to_owned(),
                )]),
            }
        }

        #[test]
        fn same_active_install_preflight_falls_through_when_any_gate_fails() {
            let (root, paths) = fixture();
            let generation = prepare_generation(&paths).unwrap();
            switch_current(&paths, &generation).unwrap();
            let matching = rust_descriptor(&generation.generation_id);

            // Non-Rust service kind is never a no-op.
            assert!(
                same_active_install_noop_with(
                    &paths,
                    &ServiceDescriptor {
                        kind: ServiceKind::Node,
                        env: BTreeMap::new(),
                    },
                    || true,
                    || true,
                )
                .unwrap()
                .is_none()
            );

            // Missing runtime/current falls through.
            fs::remove_file(&paths.current_link).unwrap();
            assert!(
                same_active_install_noop_with(&paths, &matching, || true, || true,)
                    .unwrap()
                    .is_none()
            );
            switch_current(&paths, &generation).unwrap();

            // Non-symlink runtime/current fails closed.
            fs::remove_file(&paths.current_link).unwrap();
            fs::write(&paths.current_link, b"not-a-symlink").unwrap();
            assert!(same_active_install_noop_with(&paths, &matching, || true, || true,).is_err());
            fs::remove_file(&paths.current_link).unwrap();
            switch_current(&paths, &generation).unwrap();

            // Unowned runtime/current target fails closed.
            fs::remove_file(&paths.current_link).unwrap();
            symlink("generations/other", &paths.current_link).unwrap();
            assert!(same_active_install_noop_with(&paths, &matching, || true, || true,).is_err());
            fs::remove_file(&paths.current_link).unwrap();
            switch_current(&paths, &generation).unwrap();

            // Mismatched generation id falls through.
            fs::remove_file(&paths.current_link).unwrap();
            symlink("generations/rust-other", &paths.current_link).unwrap();
            let mut loaded_called = false;
            assert!(
                same_active_install_noop_with(
                    &paths,
                    &matching,
                    || {
                        loaded_called = true;
                        true
                    },
                    || true,
                )
                .unwrap()
                .is_none()
            );
            assert!(
                !loaded_called,
                "launchd probe must not run for a mismatched generation"
            );
            fs::remove_file(&paths.current_link).unwrap();
            switch_current(&paths, &generation).unwrap();

            // Missing installed generation binary falls through.
            fs::remove_file(&generation.binary).unwrap();
            assert!(
                same_active_install_noop_with(&paths, &matching, || true, || true,)
                    .unwrap()
                    .is_none()
            );
            fs::write(&generation.binary, b"rust-binary-fixture").unwrap();
            fs::set_permissions(&generation.binary, fs::Permissions::from_mode(0o700)).unwrap();

            // Installed generation binary hash mismatch falls through.
            fs::write(&generation.binary, b"tampered-binary").unwrap();
            fs::set_permissions(&generation.binary, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(
                same_active_install_noop_with(&paths, &matching, || true, || true,)
                    .unwrap()
                    .is_none()
            );
            fs::write(&generation.binary, b"rust-binary-fixture").unwrap();
            fs::set_permissions(&generation.binary, fs::Permissions::from_mode(0o700)).unwrap();

            // Installed generation binary symlink fails closed.
            fs::remove_file(&generation.binary).unwrap();
            symlink("../../herdr-mcp-source", &generation.binary).unwrap();
            assert!(same_active_install_noop_with(&paths, &matching, || true, || true,).is_err());
            fs::remove_file(&generation.binary).unwrap();
            fs::write(&generation.binary, b"rust-binary-fixture").unwrap();
            fs::set_permissions(&generation.binary, fs::Permissions::from_mode(0o700)).unwrap();

            // Installed generation directory symlink fails closed.
            let generation_dir = paths.generations_dir.join(&generation.generation_id);
            fs::remove_dir_all(&generation_dir).unwrap();
            symlink("outside", &generation_dir).unwrap();
            assert!(same_active_install_noop_with(&paths, &matching, || true, || true,).is_err());
            fs::remove_file(&generation_dir).unwrap();
            fs::create_dir_all(&generation_dir).unwrap();
            fs::write(&generation.binary, b"rust-binary-fixture").unwrap();
            fs::set_permissions(&generation.binary, fs::Permissions::from_mode(0o700)).unwrap();

            // Plist generation env mismatch falls through.
            let mismatched = rust_descriptor("rust-other");
            assert!(
                same_active_install_noop_with(&paths, &mismatched, || true, || true,)
                    .unwrap()
                    .is_none()
            );

            // Unloaded service falls through; health must not run.
            let mut health_called = false;
            assert!(
                same_active_install_noop_with(
                    &paths,
                    &matching,
                    || false,
                    || {
                        health_called = true;
                        true
                    },
                )
                .unwrap()
                .is_none()
            );
            assert!(
                !health_called,
                "health must not run for an unloaded service"
            );

            // Loaded but unhealthy service falls through.
            assert!(
                same_active_install_noop_with(&paths, &matching, || true, || false,)
                    .unwrap()
                    .is_none()
            );

            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn same_active_install_noops_even_with_adopt_node_when_rust_owned() {
            let (root, paths) = fixture();
            let generation = prepare_generation(&paths).unwrap();
            switch_current(&paths, &generation).unwrap();
            let env = service_environment(&paths, &BTreeMap::new(), &generation).unwrap();
            let plist = encode_service_plist(&paths, &env).unwrap();
            fs::create_dir_all(paths.plist.parent().unwrap()).unwrap();
            fs::write(&paths.plist, &plist).unwrap();

            let lock = ServiceMutationLock::acquire(&paths).unwrap();
            let result = install_with_noop_checks(&paths, true, &lock, || true, || true).unwrap();
            assert_eq!(result["ok"], true);
            assert_eq!(result["already_active"], true);
            assert_eq!(result["changed"], false);
            assert_eq!(result["generation"], generation.generation_id);
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn same_active_install_full_path_fails_closed_on_non_symlink_current_without_side_effects()
        {
            let (root, paths) = fixture();
            let generation = prepare_generation(&paths).unwrap();
            switch_current(&paths, &generation).unwrap();
            let env = service_environment(&paths, &BTreeMap::new(), &generation).unwrap();
            let plist = encode_service_plist(&paths, &env).unwrap();
            fs::create_dir_all(paths.plist.parent().unwrap()).unwrap();
            fs::write(&paths.plist, &plist).unwrap();

            let lock = ServiceMutationLock::acquire(&paths).unwrap();

            // Replace runtime/current with a regular file so the full preflight
            // fails closed before any mutation. Take the baseline only after this
            // intentional fixture mutation, so the regular file itself is pinned.
            fs::remove_file(&paths.current_link).unwrap();
            fs::write(&paths.current_link, b"not-a-symlink").unwrap();

            let binary_before = fs::read(&generation.binary).unwrap();
            let plist_before = fs::read(&paths.plist).unwrap();
            let current_before = fs::read(&paths.current_link).unwrap();
            let current_kind_before = fs::symlink_metadata(&paths.current_link)
                .unwrap()
                .file_type()
                .is_symlink();
            let backups_before = paths.backups_dir.exists();
            let guardians_before = paths.guardians_dir.exists();
            let mut config_entries_before = fs::read_dir(&paths.config_dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            config_entries_before.sort();

            assert!(install_with_noop_checks(&paths, false, &lock, || true, || true).is_err());

            assert_eq!(fs::read(&generation.binary).unwrap(), binary_before);
            assert_eq!(fs::read(&paths.plist).unwrap(), plist_before);
            assert_eq!(fs::read(&paths.current_link).unwrap(), current_before);
            assert_eq!(
                fs::symlink_metadata(&paths.current_link)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                current_kind_before
            );
            assert_eq!(paths.backups_dir.exists(), backups_before);
            assert_eq!(paths.guardians_dir.exists(), guardians_before);
            let mut config_entries_after = fs::read_dir(&paths.config_dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            config_entries_after.sort();
            assert_eq!(config_entries_after, config_entries_before);
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn same_active_install_full_path_fails_closed_on_symlinked_generation_dir_without_side_effects()
         {
            let (root, paths) = fixture();
            let generation = prepare_generation(&paths).unwrap();
            switch_current(&paths, &generation).unwrap();
            let env = service_environment(&paths, &BTreeMap::new(), &generation).unwrap();
            let plist = encode_service_plist(&paths, &env).unwrap();
            fs::create_dir_all(paths.plist.parent().unwrap()).unwrap();
            fs::write(&paths.plist, &plist).unwrap();

            let lock = ServiceMutationLock::acquire(&paths).unwrap();

            // Replace the generation directory with a symlink so the full
            // preflight fails closed before any mutation. Take the baseline only
            // after this intentional fixture mutation.
            let generation_dir = paths.generations_dir.join(&generation.generation_id);
            fs::remove_dir_all(&generation_dir).unwrap();
            symlink("outside", &generation_dir).unwrap();

            let plist_before = fs::read(&paths.plist).unwrap();
            let current_before = fs::read_link(&paths.current_link).unwrap();
            let generation_dir_kind_before = fs::symlink_metadata(&generation_dir)
                .unwrap()
                .file_type()
                .is_symlink();
            let backups_before = paths.backups_dir.exists();
            let guardians_before = paths.guardians_dir.exists();
            let mut config_entries_before = fs::read_dir(&paths.config_dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            config_entries_before.sort();

            assert!(install_with_noop_checks(&paths, false, &lock, || true, || true).is_err());

            assert_eq!(fs::read(&paths.plist).unwrap(), plist_before);
            assert_eq!(fs::read_link(&paths.current_link).unwrap(), current_before);
            assert_eq!(
                fs::symlink_metadata(&generation_dir)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                generation_dir_kind_before
            );
            assert_eq!(paths.backups_dir.exists(), backups_before);
            assert_eq!(paths.guardians_dir.exists(), guardians_before);
            let mut config_entries_after = fs::read_dir(&paths.config_dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            config_entries_after.sort();
            assert_eq!(config_entries_after, config_entries_before);
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn unrelated_service_and_watchdog_shapes_are_rejected() {
            let (root, paths) = fixture();
            let mut other = Dictionary::new();
            other.insert(
                "Label".to_owned(),
                PlistValue::String(SERVICE_LABEL.to_owned()),
            );
            other.insert(
                "ProgramArguments".to_owned(),
                PlistValue::Array(vec![PlistValue::String("/bin/echo".to_owned())]),
            );
            let mut bytes = Vec::new();
            PlistValue::Dictionary(other)
                .to_writer_xml(&mut bytes)
                .unwrap();
            assert_eq!(
                describe_service(Some(&bytes), &paths).unwrap().kind,
                ServiceKind::Other
            );
            assert!(!watchdog_is_legacy_owned(Some(&bytes)).unwrap());
            fs::remove_dir_all(root).unwrap();
        }

        fn watchdog_plist_bytes(label: &str, script: &str) -> Vec<u8> {
            let mut root = Dictionary::new();
            root.insert("Label".to_owned(), PlistValue::String(label.to_owned()));
            root.insert(
                "ProgramArguments".to_owned(),
                PlistValue::Array(vec![
                    PlistValue::String("/bin/bash".to_owned()),
                    PlistValue::String(script.to_owned()),
                    PlistValue::String("once".to_owned()),
                ]),
            );
            let mut bytes = Vec::new();
            PlistValue::Dictionary(root)
                .to_writer_xml(&mut bytes)
                .unwrap();
            bytes
        }

        #[test]
        fn legacy_watchdog_identity_rejects_health_watchdog_script_basename() {
            let legacy = watchdog_plist_bytes(
                WATCHDOG_LABEL,
                "/Users/example/.config/herdr-mcp/watchdog.sh",
            );
            assert!(watchdog_is_legacy_owned(Some(&legacy)).unwrap());

            let health_under_legacy_label = watchdog_plist_bytes(
                WATCHDOG_LABEL,
                "/Users/example/.config/herdr-mcp/health-watchdog.sh",
            );
            assert!(
                !watchdog_is_legacy_owned(Some(&health_under_legacy_label)).unwrap(),
                "health-watchdog.sh must not satisfy the legacy watchdog.sh basename gate"
            );

            let health_label = watchdog_plist_bytes(
                HEALTH_WATCHDOG_LABEL,
                "/Users/example/.config/herdr-mcp/health-watchdog.sh",
            );
            assert!(!watchdog_is_legacy_owned(Some(&health_label)).unwrap());
        }

        #[test]
        fn service_status_reports_health_watchdog_separately_from_legacy() {
            let (root, paths) = fixture();
            assert_eq!(
                paths
                    .health_watchdog_plist
                    .file_name()
                    .and_then(|value| value.to_str()),
                Some("dev.herdr-mcp.health-watchdog.plist")
            );
            assert_eq!(
                paths
                    .watchdog_plist
                    .file_name()
                    .and_then(|value| value.to_str()),
                Some("dev.herdr-mcp.watchdog.plist")
            );
            assert_ne!(paths.health_watchdog_plist, paths.watchdog_plist);

            fs::create_dir_all(paths.health_watchdog_plist.parent().unwrap()).unwrap();
            fs::write(
                &paths.health_watchdog_plist,
                watchdog_plist_bytes(
                    HEALTH_WATCHDOG_LABEL,
                    paths
                        .config_dir
                        .join("health-watchdog.sh")
                        .to_string_lossy()
                        .as_ref(),
                ),
            )
            .unwrap();

            let status =
                status_with(&paths, || false, || false, || true, |_kind, _loaded| false).unwrap();
            assert_eq!(status["legacy_watchdog_present"], false);
            assert_eq!(status["legacy_watchdog_loaded"], false);
            assert_eq!(status["health_watchdog_label"], HEALTH_WATCHDOG_LABEL);
            assert_eq!(status["health_watchdog_present"], true);
            assert_eq!(status["health_watchdog_loaded"], true);
            assert_eq!(status["implementation"], "missing");
            assert_eq!(status["ok"], false);

            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn rollback_backup_reader_is_confined_and_rejects_symlinks() {
            let (root, paths) = fixture();
            let backup = backup_bytes(&paths, "node-server", b"owned-backup").unwrap();
            assert_eq!(read_owned_backup(&paths, &backup).unwrap(), b"owned-backup");

            let outside = root.join("outside.plist");
            fs::write(&outside, b"outside").unwrap();
            assert!(
                read_owned_backup(&paths, outside.to_string_lossy().as_ref()).is_err(),
                "rollback must not read arbitrary paths"
            );

            ensure_secure_dir(&paths.backups_dir).unwrap();
            let link = paths.backups_dir.join("linked.plist");
            symlink(&outside, &link).unwrap();
            assert!(
                read_owned_backup(&paths, link.to_string_lossy().as_ref()).is_err(),
                "rollback must not follow backup symlinks"
            );
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn bootstrap_retry_absorbs_transient_launchd_io_errors() {
            let delays = [
                Duration::from_millis(1),
                Duration::from_millis(2),
                Duration::from_millis(3),
            ];
            let mut attempts = 0_usize;
            let mut absent_checks = 0_usize;
            let mut sleeps = Vec::new();
            let result = bootstrap_retry_with(
                &delays,
                || {
                    absent_checks += 1;
                    Ok(())
                },
                || {
                    attempts += 1;
                    if attempts < 3 {
                        Err("launchctl failed: Bootstrap failed: 5: Input/output error".to_owned())
                    } else {
                        Ok(())
                    }
                },
                |delay| sleeps.push(delay),
            )
            .unwrap();
            assert_eq!(result, 3);
            assert_eq!(attempts, 3);
            assert_eq!(absent_checks, 3);
            assert_eq!(sleeps, delays[..2]);
        }

        #[test]
        fn bootstrap_retry_fails_closed_after_bounded_attempts() {
            let delays = [Duration::from_millis(1), Duration::from_millis(2)];
            let mut attempts = 0_usize;
            let mut sleeps = Vec::new();
            let error = bootstrap_retry_with(
                &delays,
                || Ok(()),
                || {
                    attempts += 1;
                    Err(format!("bootstrap-{attempts}"))
                },
                |delay| sleeps.push(delay),
            )
            .unwrap_err();
            assert_eq!(attempts, 3);
            assert_eq!(sleeps, delays);
            assert!(error.contains("failed after 3 attempts"));
            assert!(error.contains("bootstrap-3"));
        }

        #[test]
        fn rollback_waits_for_an_inflight_bootout_without_sending_a_second_stop() {
            let mut loaded_checks = 0_usize;
            let mut stops = 0_usize;
            let mut waits = 0_usize;
            settle_service_for_restore_with(
                true,
                || {
                    loaded_checks += 1;
                    true
                },
                || {
                    stops += 1;
                    Ok(())
                },
                || {
                    waits += 1;
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(loaded_checks, 0);
            assert_eq!(stops, 0, "rollback must not issue a second bootout");
            assert_eq!(waits, 1);
        }

        #[test]
        fn rollback_stops_a_loaded_service_when_no_bootout_is_inflight() {
            let mut loaded_checks = 0_usize;
            let mut stops = 0_usize;
            let mut waits = 0_usize;
            settle_service_for_restore_with(
                false,
                || {
                    loaded_checks += 1;
                    true
                },
                || {
                    stops += 1;
                    Ok(())
                },
                || {
                    waits += 1;
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(loaded_checks, 1);
            assert_eq!(stops, 1);
            assert_eq!(waits, 0);
        }

        #[test]
        fn rollback_leaves_an_already_absent_service_alone() {
            let mut stops = 0_usize;
            let mut waits = 0_usize;
            settle_service_for_restore_with(
                false,
                || false,
                || {
                    stops += 1;
                    Ok(())
                },
                || {
                    waits += 1;
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(stops, 0);
            assert_eq!(waits, 0);
        }

        fn guardian_record(mode: GuardianMode, rollback_id: Option<&str>) -> GuardianRecord {
            GuardianRecord {
                transaction_id: "gtx-12345678-guardian".to_owned(),
                mode,
                state: "watching".to_owned(),
                parent_pid: 1234,
                created_at: 1,
                rollback_id: rollback_id.map(str::to_owned),
                candidate_generation_id: Some("rust-candidate".to_owned()),
                server_plist_backup: Some("/backups/server.plist".to_owned()),
                watchdog_plist_backup: None,
                previous_current_target: Some("generations/rust-known-good".to_owned()),
                server_was_loaded: true,
                watchdog_was_loaded: false,
                detail: None,
            }
        }

        fn guardian_rollback(rollback_id: &str, state: &str) -> ServiceRollbackRecord {
            ServiceRollbackRecord {
                rollback_id: rollback_id.to_owned(),
                source_kind: "rust".to_owned(),
                activated_generation_id: "rust-candidate".to_owned(),
                server_plist_backup: Some("/backups/server.plist".to_owned()),
                watchdog_plist_backup: None,
                previous_current_target: Some("generations/rust-known-good".to_owned()),
                server_was_loaded: true,
                watchdog_was_loaded: false,
                created_at: 1,
                state: state.to_owned(),
            }
        }

        fn active_generation(id: &str) -> RuntimeGenerationRecord {
            RuntimeGenerationRecord {
                generation_id: id.to_owned(),
                runtime_path: format!("/runtime/{id}"),
                sha256: "sha".to_owned(),
                source: "service-install".to_owned(),
                state: "active".to_owned(),
                installed_at: 1,
                activated_at: Some(2),
                deactivated_at: None,
            }
        }

        #[test]
        fn guardian_transaction_ids_and_records_are_strict_and_round_trip() {
            assert!(valid_guardian_transaction_id("gtx-12345678-guardian"));
            assert!(!valid_guardian_transaction_id("../guardian"));
            assert!(!valid_guardian_transaction_id("gtx-bad/slash"));

            let record = guardian_record(GuardianMode::Install, Some("rb-1"));
            let decoded = decode_guardian_record(&guardian_record_value(&record)).unwrap();
            assert_eq!(decoded, record);
            let mut invalid = guardian_record_value(&record);
            invalid
                .as_object_mut()
                .unwrap()
                .insert("unexpected".to_owned(), json!(true));
            assert!(decode_guardian_record(&invalid).is_err());
        }

        #[test]
        fn guardian_decision_never_rolls_back_a_durably_committed_transaction() {
            let install = guardian_record(GuardianMode::Install, Some("rb-1"));
            let ready = guardian_rollback("rb-1", "ready");
            let candidate = active_generation("rust-candidate");
            assert_eq!(
                guardian_decision(&install, Some(&ready), Some(&candidate)),
                GuardianDecision::Exit("committed")
            );
            let prepared = guardian_rollback("rb-1", "prepared");
            let old = active_generation("rust-old");
            assert_eq!(
                guardian_decision(&install, Some(&prepared), Some(&old)),
                GuardianDecision::Recover
            );
            let auto = guardian_rollback("rb-1", "auto_rolled_back");
            assert_eq!(
                guardian_decision(&install, Some(&auto), Some(&old)),
                GuardianDecision::Exit("parent_recovered")
            );

            let fresh_install = guardian_record(GuardianMode::Install, None);
            assert_eq!(
                guardian_decision(&fresh_install, None, Some(&candidate)),
                GuardianDecision::Exit("committed")
            );
            assert_eq!(
                guardian_decision(&fresh_install, None, Some(&old)),
                GuardianDecision::Recover
            );

            let rollback = guardian_record(GuardianMode::Rollback, Some("rb-2"));
            assert_eq!(
                guardian_decision(
                    &rollback,
                    Some(&guardian_rollback("rb-2", "consumed")),
                    Some(&active_generation("rust-known-good"))
                ),
                GuardianDecision::Exit("committed")
            );
            assert_eq!(
                guardian_decision(
                    &rollback,
                    Some(&guardian_rollback("rb-2", "ready")),
                    Some(&candidate)
                ),
                GuardianDecision::Exit("parent_recovered")
            );
            assert_eq!(
                guardian_decision(
                    &rollback,
                    Some(&guardian_rollback("rb-2", "consuming")),
                    Some(&candidate)
                ),
                GuardianDecision::Recover
            );
            assert!(matches!(
                guardian_decision(&install, Some(&ready), Some(&old)),
                GuardianDecision::Refuse(_)
            ));
        }

        #[test]
        fn guardian_quiesce_absorbs_inflight_bootout_before_sending_one_stop() {
            let mut waits = 0_usize;
            let mut stops = 0_usize;
            guardian_quiesce_with(
                || true,
                || {
                    waits += 1;
                    if waits == 1 {
                        Err("still loaded".to_owned())
                    } else {
                        Ok(())
                    }
                },
                || {
                    stops += 1;
                    Ok(true)
                },
            )
            .unwrap();
            assert_eq!(waits, 2);
            assert_eq!(stops, 1);

            let mut immediate_waits = 0_usize;
            let mut immediate_stops = 0_usize;
            guardian_quiesce_with(
                || true,
                || {
                    immediate_waits += 1;
                    Ok(())
                },
                || {
                    immediate_stops += 1;
                    Ok(true)
                },
            )
            .unwrap();
            assert_eq!(immediate_waits, 1);
            assert_eq!(immediate_stops, 0);
        }

        fn append_guardian_test_marker(path: &Path, marker: &str) {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)
                .unwrap();
            writeln!(file, "{marker}").unwrap();
            file.sync_all().unwrap();
        }

        fn wait_for_guardian_test_marker(path: &Path, marker: &str, budget: Duration) {
            let complete = format!("{marker}\n");
            let deadline = Instant::now() + budget;
            loop {
                let text = fs::read_to_string(path).unwrap_or_default();
                // Only a newline-terminated complete record counts. str::lines()
                // would treat a trailing partial "READY" (newline not yet
                // written by the watcher) as a full marker, letting the parent
                // start its mutation while the watcher is still finishing the
                // READY write; the interleaved appends then merge into a line
                // like "READYMUTATION". Inspect only segments that end in the
                // newline so the observed record is complete.
                if text
                    .split_inclusive('\n')
                    .any(|segment| segment == complete)
                {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "guardian subprocess did not emit {marker}; observed: {text:?}"
                );
                thread::sleep(Duration::from_millis(20));
            }
        }

        fn guardian_test_lock(path: &Path) -> File {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)
                .unwrap();
            assert_eq!(
                unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
                0
            );
            file
        }

        fn spawn_guardian_test_watcher(
            result_path: &Path,
            read_signal: &File,
            write_signal: &File,
            lock: &File,
            mode: &str,
        ) -> Child {
            let read_fd = read_signal.as_raw_fd();
            let write_fd = write_signal.as_raw_fd();
            let lock_fd = lock.as_raw_fd();
            assert!(
                [read_fd, write_fd, lock_fd]
                    .iter()
                    .all(|fd| !matches!(*fd, GUARDIAN_PARENT_FD | GUARDIAN_LOCK_FD))
            );
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "service_manager::macos::tests::guardian_subprocess_helper",
                    "--nocapture",
                ])
                .env("HERDR_MCP_GUARDIAN_TEST_HELPER", mode)
                .env("HERDR_MCP_GUARDIAN_TEST_RESULT", result_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            unsafe {
                command.pre_exec(move || {
                    if libc::dup2(read_fd, GUARDIAN_PARENT_FD) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::dup2(lock_fd, GUARDIAN_LOCK_FD) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    libc::close(write_fd);
                    libc::close(read_fd);
                    libc::close(lock_fd);
                    Ok(())
                });
            }
            command.spawn().unwrap()
        }

        fn assert_guardian_test_child_exits(child: &mut Child, budget: Duration) {
            let deadline = Instant::now() + budget;
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "guardian subprocess failed with {status}");
                    return;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("guardian subprocess did not exit within {budget:?}");
                }
                thread::sleep(Duration::from_millis(20));
            }
        }

        #[test]
        fn guardian_subprocess_helper() {
            let Ok(mode) = env::var("HERDR_MCP_GUARDIAN_TEST_HELPER") else {
                return;
            };
            let result_path = PathBuf::from(
                env::var_os("HERDR_MCP_GUARDIAN_TEST_RESULT").expect("guardian helper result path"),
            );
            match mode.as_str() {
                "watcher" | "silent-watcher" => {
                    assert!(guardian_fd_is_open(GUARDIAN_PARENT_FD));
                    assert!(guardian_fd_is_open(GUARDIAN_LOCK_FD));
                    set_guardian_fd_cloexec(GUARDIAN_PARENT_FD).unwrap();
                    set_guardian_fd_cloexec(GUARDIAN_LOCK_FD).unwrap();
                    if mode == "watcher" {
                        append_guardian_test_marker(&result_path, "READY");
                    }
                    let closed =
                        wait_for_guardian_parent_signal(GUARDIAN_PARENT_FD, Duration::from_secs(5))
                            .unwrap();
                    assert!(closed, "guardian helper never observed parent POLLHUP");
                    append_guardian_test_marker(&result_path, "HUP");
                }
                "parent-exit" => {
                    let lock = guardian_test_lock(&result_path.with_extension("lock"));
                    let (read_signal, write_signal) = guardian_pipe().unwrap();
                    let _watcher = spawn_guardian_test_watcher(
                        &result_path,
                        &read_signal,
                        &write_signal,
                        &lock,
                        "watcher",
                    );
                    drop(read_signal);
                    wait_for_guardian_test_marker(&result_path, "READY", Duration::from_secs(3));
                    // Keep the writer open until the OS tears down this process.
                    // The orphaned watcher must observe POLLHUP from process exit,
                    // not from an explicit close in the helper.
                    std::mem::forget(write_signal);
                    std::mem::forget(lock);
                    std::process::exit(0);
                }
                other => panic!("unknown guardian subprocess helper mode {other}"),
            }
        }

        #[test]
        fn guardian_subprocess_handshake_precedes_mutation_and_finish_delivers_hup() {
            let root = root("guardian-subprocess-finish");
            fs::create_dir_all(&root).unwrap();
            let result_path = root.join("events.txt");
            let lock = guardian_test_lock(&root.join("mutation.lock"));
            let (read_signal, write_signal) = guardian_pipe().unwrap();
            let mut child = spawn_guardian_test_watcher(
                &result_path,
                &read_signal,
                &write_signal,
                &lock,
                "watcher",
            );
            drop(read_signal);

            wait_for_guardian_test_marker(&result_path, "READY", Duration::from_secs(3));
            append_guardian_test_marker(&result_path, "MUTATION");
            drop(write_signal);
            assert_guardian_test_child_exits(&mut child, Duration::from_secs(3));

            let events = fs::read_to_string(&result_path).unwrap();
            assert_eq!(
                events.lines().collect::<Vec<_>>(),
                vec!["READY", "MUTATION", "HUP"],
                "mutation must start only after handshake and parent close must not leave a writer inherited in the guardian"
            );
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn guardian_subprocess_parent_exit_delivers_hup_without_pid_liveness_polling() {
            let root = root("guardian-subprocess-parent-exit");
            fs::create_dir_all(&root).unwrap();
            let result_path = root.join("events.txt");
            let mut parent = Command::new(std::env::current_exe().unwrap());
            parent
                .args([
                    "--exact",
                    "service_manager::macos::tests::guardian_subprocess_helper",
                    "--nocapture",
                ])
                .env("HERDR_MCP_GUARDIAN_TEST_HELPER", "parent-exit")
                .env("HERDR_MCP_GUARDIAN_TEST_RESULT", &result_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut parent = parent.spawn().unwrap();
            assert_guardian_test_child_exits(&mut parent, Duration::from_secs(4));
            wait_for_guardian_test_marker(&result_path, "HUP", Duration::from_secs(4));
            let events = fs::read_to_string(&result_path).unwrap();
            assert_eq!(events.lines().collect::<Vec<_>>(), vec!["READY", "HUP"]);
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn guardian_startup_timeout_aborts_and_reaps_before_parent_signal_closes() {
            let (root, paths) = fixture();
            ensure_secure_dir(&paths.guardians_dir).unwrap();
            let transaction_id = "gtx-12345678-timeout";
            let directory = guardian_dir(&paths, transaction_id).unwrap();
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            let mut record = guardian_record(GuardianMode::Rollback, Some("rb-timeout"));
            record.transaction_id = transaction_id.to_owned();
            record.state = "armed".to_owned();
            write_guardian_record(&paths, &record).unwrap();

            let result_path = root.join("timeout-events.txt");
            let lock = guardian_test_lock(&root.join("timeout-mutation.lock"));
            let (read_signal, write_signal) = guardian_pipe().unwrap();
            let mut child = spawn_guardian_test_watcher(
                &result_path,
                &read_signal,
                &write_signal,
                &lock,
                "silent-watcher",
            );
            drop(read_signal);
            let mut write_signal = Some(write_signal);

            abort_guardian_startup(
                &paths,
                transaction_id,
                &mut child,
                &mut write_signal,
                "fault-injected handshake timeout",
            )
            .unwrap();
            assert!(write_signal.is_none());
            assert!(
                child.try_wait().unwrap().is_some(),
                "guardian child must be reaped"
            );
            let aborted = read_guardian_record(&paths, transaction_id).unwrap();
            assert_eq!(aborted.state, "aborted");
            assert_eq!(
                guardian_decision(&aborted, None, None),
                GuardianDecision::Exit("aborted"),
                "startup timeout must never enter recovery"
            );
            assert!(
                !fs::read_to_string(&result_path)
                    .unwrap_or_default()
                    .lines()
                    .any(|line| line == "HUP"),
                "the child must be killed before the parent-signal writer is closed"
            );
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn guardian_startup_state_fence_failure_still_reaps_before_parent_signal_closes() {
            let (root, paths) = fixture();
            ensure_secure_dir(&paths.guardians_dir).unwrap();
            let transaction_id = "gtx-12345678-fence-failure";
            let directory = guardian_dir(&paths, transaction_id).unwrap();
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            let mut record = guardian_record(GuardianMode::Rollback, Some("rb-fence"));
            record.transaction_id = transaction_id.to_owned();
            record.state = "armed".to_owned();
            write_guardian_record(&paths, &record).unwrap();

            let outside_lock = root.join("outside-transaction.lock");
            fs::write(&outside_lock, b"outside").unwrap();
            symlink(
                &outside_lock,
                guardian_state_lock_path(&paths, transaction_id).unwrap(),
            )
            .unwrap();

            let result_path = root.join("fence-events.txt");
            let lock = guardian_test_lock(&root.join("fence-mutation.lock"));
            let (read_signal, write_signal) = guardian_pipe().unwrap();
            let mut child = spawn_guardian_test_watcher(
                &result_path,
                &read_signal,
                &write_signal,
                &lock,
                "silent-watcher",
            );
            drop(read_signal);
            let mut write_signal = Some(write_signal);

            let error = abort_guardian_startup(
                &paths,
                transaction_id,
                &mut child,
                &mut write_signal,
                "fault-injected transaction-lock failure",
            )
            .unwrap_err();
            assert!(error.contains("must not be a symlink"));
            assert!(write_signal.is_none());
            assert!(
                child.try_wait().unwrap().is_some(),
                "guardian child must be reaped"
            );
            assert_eq!(
                read_guardian_record(&paths, transaction_id).unwrap().state,
                "armed"
            );
            assert!(
                !fs::read_to_string(&result_path)
                    .unwrap_or_default()
                    .lines()
                    .any(|line| line == "HUP"),
                "state-fence failure must still kill the child before the signal writer closes"
            );
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn service_mutation_lock_is_single_writer_and_released_on_close() {
            let (root, paths) = fixture();
            let first = ServiceMutationLock::acquire(&paths).unwrap();
            assert!(ServiceMutationLock::acquire(&paths).is_err());
            drop(first);
            // On macOS flock state follows the open file description across fork(2).
            // Parallel tests can therefore inherit this unique fixture lock for the
            // short fork-to-exec window even after the parent File has been dropped.
            // Wait only for that bounded CLOEXEC handoff; a persistent lock leak must
            // still fail this test.
            let deadline = Instant::now() + Duration::from_secs(1);
            let second = loop {
                match ServiceMutationLock::acquire(&paths) {
                    Ok(lock) => break lock,
                    Err(error)
                        if error.starts_with("another service mutation is in progress:")
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("service mutation lock was not released: {error}"),
                }
            };
            drop(second);
            fs::remove_dir_all(root).unwrap();
        }
    }
}
