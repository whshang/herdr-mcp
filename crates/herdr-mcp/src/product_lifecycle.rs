//! Product-level lifecycle commands for herdr-mcp itself.
//!
//! `service ...` remains the granular service-management surface. The top-level
//! `reinstall` and `uninstall` commands deliberately do not install, start,
//! stop, clean, remove, or otherwise lifecycle-mutate the independent `herdr`
//! runtime. The herdr-mcp service may still connect to Herdr through its normal
//! runtime transport while being health-validated.

#[cfg(target_os = "macos")]
use crate::paths::RuntimePaths;
use std::process::ExitCode;

pub(crate) fn reinstall() -> Result<ExitCode, String> {
    refuse_managed_exec_mutation()?;
    #[cfg(not(target_os = "macos"))]
    {
        Err("product reinstall currently requires macOS".to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        macos::reinstall()
    }
}

pub(crate) fn uninstall() -> Result<ExitCode, String> {
    refuse_managed_exec_mutation()?;
    #[cfg(not(target_os = "macos"))]
    {
        Err("product uninstall currently requires macOS".to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        macos::uninstall()
    }
}

/// Validate the durable config-root ownership marker before a service install.
#[cfg(target_os = "macos")]
pub(crate) fn preflight_installation_identity() -> Result<(), String> {
    macos::preflight_installation_identity()
}

/// Record the config root as intentionally managed by herdr-mcp. The narrower
/// `service uninstall` preserves this marker for later product cleanup.
#[cfg(target_os = "macos")]
pub(crate) fn record_installation_identity() -> Result<(), String> {
    macos::record_installation_identity()
}

/// Capture the exact prior installation-identity marker (or its absence) so a
/// failed post-commit step can restore it. Fails closed on a non-owned marker.
#[cfg(target_os = "macos")]
pub(crate) fn capture_installation_identity() -> Result<Option<Vec<u8>>, String> {
    macos::capture_installation_identity()
}

/// Restore the installation-identity marker to its exact prior bytes, or remove
/// a newly-created owned marker when the prior state was absent.
#[cfg(target_os = "macos")]
pub(crate) fn restore_installation_identity(prior: Option<&[u8]>) -> Result<(), String> {
    macos::restore_installation_identity(prior)
}

fn refuse_managed_exec_mutation() -> Result<(), String> {
    if std::env::var_os("HERDR_MCP_EXEC_ID").is_some() {
        return Err(
            "product lifecycle mutations cannot run inside a managed herdr_exec session; run the command from an independent terminal"
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use crate::cli::ServiceCommand;
    use crate::instance::InstanceId;
    use crate::link::ownership::{LINK_LABEL, LINK_PROD_LABEL};
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};
    use std::ffi::OsStr;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::os::unix::io::AsRawFd;
    use std::path::{Component, Path, PathBuf};
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const JOURNAL_SCHEMA: u8 = 2;
    const JOURNAL_NAME: &str = "product-uninstall.json";
    const LAUNCHD_BOOTOUT_POLL_INTERVAL: Duration = Duration::from_millis(50);
    const LAUNCHD_BOOTOUT_MAX_POLLS: usize = 200;
    const INSTALL_IDENTITY_SCHEMA: u8 = 1;
    const INSTALL_IDENTITY_NAME: &str = "product-install.json";
    const PHASE_AUTO_UPDATE: &str = "auto_update_scheduler";
    const PHASE_LAUNCH_AGENTS: &str = "launch_agents";
    const PHASE_NATIVE_HOST: &str = "native_host";
    const PHASE_SERVICE: &str = "service";
    const PHASE_USER_CLI: &str = "user_cli";
    const PHASE_EXTERNAL_COMPLETE: &str = "external_complete";
    const KNOWN_PHASES: &[&str] = &[
        PHASE_AUTO_UPDATE,
        PHASE_LAUNCH_AGENTS,
        PHASE_NATIVE_HOST,
        PHASE_SERVICE,
        PHASE_USER_CLI,
        PHASE_EXTERNAL_COMPLETE,
    ];

    #[derive(Debug, Clone)]
    struct LaunchAgentRemoval {
        label: String,
        path: PathBuf,
        present: bool,
        reason: String,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum UserCliDisposition {
        Absent,
        Owned,
        Preserve,
    }

    #[derive(Debug, Clone)]
    struct UserCliPlan {
        path: PathBuf,
        disposition: UserCliDisposition,
    }

    struct UninstallResultReport {
        config_removed: bool,
        service_removed: bool,
        native_host_removed: bool,
        user_cli_removed: bool,
        scheduler_removed: bool,
        launch_agents_removed: Vec<String>,
        journal_resumed: bool,
    }

    struct ProductMutationLock {
        file: fs::File,
        path: PathBuf,
        dir: PathBuf,
    }

    #[derive(Debug, Clone)]
    struct ReinstallFileSnapshot {
        path: PathBuf,
        bytes: Option<Vec<u8>>,
    }

    #[derive(Debug, Clone, Default)]
    struct ReinstallIntegrationSnapshot {
        files: Vec<ReinstallFileSnapshot>,
    }

    impl ProductMutationLock {
        fn acquire(home: &Path, instance: &InstanceId) -> Result<Self, String> {
            let dir = home.join("Library/Caches/herdr-mcp");
            match fs::symlink_metadata(&dir) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(format!(
                        "product lifecycle lock directory is not an owned real directory: {}",
                        dir.display()
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir_all(&dir).map_err(|error| {
                        format!("cannot create product lifecycle lock directory: {error}")
                    })?;
                }
                Err(error) => {
                    return Err(format!(
                        "cannot inspect product lifecycle lock directory {}: {error}",
                        dir.display()
                    ));
                }
            }
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|error| {
                format!("cannot secure product lifecycle lock directory: {error}")
            })?;
            let suffix = instance.name().unwrap_or("default");
            let path = dir.join(format!("product-lifecycle-{suffix}.lock"));
            reject_symlink_if_present(&path, "product lifecycle lock")?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&path)
                .map_err(|error| format!("cannot open product lifecycle lock: {error}"))?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("cannot secure product lifecycle lock: {error}"))?;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(format!(
                    "another product lifecycle mutation is in progress for this instance: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self { file, path, dir })
        }
    }

    impl Drop for ProductMutationLock {
        fn drop(&mut self) {
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
            let _ = fs::remove_file(&self.path);
            let _ = fs::remove_dir(&self.dir);
        }
    }

    impl ReinstallIntegrationSnapshot {
        fn capture(home: &Path, config_dir: &Path, instance: &InstanceId) -> Result<Self, String> {
            if instance.is_named() {
                return Ok(Self::default());
            }
            let paths = [
                launch_agent_path(home, LINK_PROD_LABEL),
                config_dir.join("runtime-control-prod.json"),
                config_dir.join("runtime-control.json"),
            ];
            let mut files = Vec::with_capacity(paths.len());
            for path in paths {
                files.push(ReinstallFileSnapshot {
                    bytes: read_optional_reinstall_file(&path)?,
                    path,
                });
            }
            Ok(Self { files })
        }

        fn restore(&self) -> Result<(), String> {
            for snapshot in &self.files {
                match &snapshot.bytes {
                    Some(bytes) => write_reinstall_file(&snapshot.path, bytes)?,
                    None => match fs::symlink_metadata(&snapshot.path) {
                        Ok(metadata) if metadata.file_type().is_symlink() => {
                            return Err(format!(
                                "reinstall compensation refuses symlink {}",
                                snapshot.path.display()
                            ));
                        }
                        Ok(metadata) if metadata.is_file() => {
                            fs::remove_file(&snapshot.path).map_err(|error| {
                                format!(
                                    "cannot restore absent reinstall snapshot {}: {error}",
                                    snapshot.path.display()
                                )
                            })?;
                        }
                        Ok(_) => {
                            return Err(format!(
                                "reinstall compensation found non-file {}",
                                snapshot.path.display()
                            ));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(format!(
                                "cannot inspect reinstall compensation path {}: {error}",
                                snapshot.path.display()
                            ));
                        }
                    },
                }
            }
            Ok(())
        }
    }

    fn read_optional_reinstall_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 1024 * 1024
        {
            return Err(format!(
                "reinstall snapshot path must be a regular file <=1MiB: {}",
                path.display()
            ));
        }
        fs::read(path)
            .map(Some)
            .map_err(|error| format!("cannot snapshot {}: {error}", path.display()))
    }

    fn write_reinstall_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
        reject_symlink_if_present(path, "reinstall compensation target")?;
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent", path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = parent.join(format!(
            ".{}.reinstall-{}-{stamp}",
            path.file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("herdr-mcp"),
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| format!("cannot create reinstall temp file: {error}"))?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot persist reinstall compensation file: {error}"))?;
        fs::rename(&temp, path).map_err(|error| {
            let _ = fs::remove_file(&temp);
            format!("cannot restore {}: {error}", path.display())
        })
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct InstallationIdentity {
        schema_version: u8,
        instance: Option<String>,
        config_root: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct UninstallJournal {
        schema_version: u8,
        action: String,
        instance: Option<String>,
        config_root: String,
        ownership_proof: Vec<String>,
        native_host_snapshot: Option<Value>,
        completed: Vec<String>,
    }

    pub(super) fn preflight_installation_identity() -> Result<(), String> {
        let paths = RuntimePaths::discover()?;
        let home = home_dir()?;
        let safe_config = validate_config_root(&home, &paths.config_dir, &paths.instance)?;
        // A valid existing marker is the strongest ownership evidence.
        if read_installation_identity(&safe_config, &paths.instance)?.is_some() {
            return Ok(());
        }
        // No marker: the root must be new, safely empty, or already carry
        // verifiable herdr-mcp product evidence. A pre-existing non-empty
        // foreign root is refused BEFORE service install writes runtime/state/
        // logs into it, so a later product uninstall can never `remove_dir_all`
        // unrelated content merely because a missing marker could be created.
        match fs::symlink_metadata(&safe_config) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
                "refusing install into symlink config root {}",
                safe_config.display()
            )),
            Ok(metadata) if metadata.is_dir() => {
                if config_root_is_empty(&safe_config)?
                    || config_root_has_product_evidence(&safe_config)
                {
                    Ok(())
                } else {
                    Err(format!(
                        "refusing to install into pre-existing non-empty config root {}: no verifiable herdr-mcp product evidence; use a fresh/empty config root or the default ~/.config/herdr-mcp",
                        safe_config.display()
                    ))
                }
            }
            Ok(_) => Err(format!(
                "config root is not a directory: {}",
                safe_config.display()
            )),
            Err(error) => Err(format!(
                "cannot inspect config root {}: {error}",
                safe_config.display()
            )),
        }
    }

    fn config_root_is_empty(config_dir: &Path) -> Result<bool, String> {
        let mut entries = fs::read_dir(config_dir).map_err(|error| {
            format!("cannot list config root {}: {error}", config_dir.display())
        })?;
        Ok(entries.next().is_none())
    }

    pub(super) fn record_installation_identity() -> Result<(), String> {
        let paths = RuntimePaths::discover()?;
        let home = home_dir()?;
        let safe_config = validate_config_root(&home, &paths.config_dir, &paths.instance)?;
        if read_installation_identity(&safe_config, &paths.instance)?.is_some() {
            return Ok(());
        }
        // Ownership must be established from verifiable product evidence, not
        // merely by writing a marker into an arbitrary pre-existing directory.
        // A config root that already exists and is non-empty is only claimable
        // when it already carries herdr-mcp-owned artifacts (the service install
        // just wrote them). A brand-new or empty root is safe to claim. Any
        // other pre-existing non-empty directory is refused so a later product
        // uninstall can never `remove_dir_all` unrelated content merely because
        // a missing marker could be created there.
        if path_present(&safe_config) && !config_root_has_product_evidence(&safe_config) {
            return Err(format!(
                "refusing to record installation identity in pre-existing non-empty config root {}: no verifiable herdr-mcp product evidence; use a fresh/empty config root or the default ~/.config/herdr-mcp",
                safe_config.display()
            ));
        }
        fs::create_dir_all(&safe_config).map_err(|error| {
            format!(
                "cannot create managed config root {}: {error}",
                safe_config.display()
            )
        })?;
        fs::set_permissions(&safe_config, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "cannot secure managed config root {}: {error}",
                safe_config.display()
            )
        })?;
        let identity = InstallationIdentity {
            schema_version: INSTALL_IDENTITY_SCHEMA,
            instance: paths.instance.name().map(str::to_owned),
            config_root: safe_config.to_string_lossy().into_owned(),
        };
        let bytes = serde_json::to_vec_pretty(&identity)
            .map_err(|error| format!("cannot encode installation identity: {error}"))?;
        write_private_atomic(&safe_config.join(INSTALL_IDENTITY_NAME), &bytes)
    }

    pub(super) fn capture_installation_identity() -> Result<Option<Vec<u8>>, String> {
        let paths = RuntimePaths::discover()?;
        let home = home_dir()?;
        let safe_config = validate_config_root(&home, &paths.config_dir, &paths.instance)?;
        let path = safe_config.join(INSTALL_IDENTITY_NAME);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "cannot inspect installation identity {}: {error}",
                    path.display()
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 8192 {
            return Err(format!(
                "installation identity is not an owned regular file: {}",
                path.display()
            ));
        }
        fs::read(&path)
            .map(Some)
            .map_err(|error| format!("cannot read installation identity: {error}"))
    }

    pub(super) fn restore_installation_identity(prior: Option<&[u8]>) -> Result<(), String> {
        let paths = RuntimePaths::discover()?;
        let home = home_dir()?;
        let safe_config = validate_config_root(&home, &paths.config_dir, &paths.instance)?;
        let path = safe_config.join(INSTALL_IDENTITY_NAME);
        match prior {
            Some(bytes) => write_private_atomic(&path, bytes),
            None => match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
                    "installation identity restore refuses symlink {}",
                    path.display()
                )),
                Ok(metadata) if metadata.is_file() => fs::remove_file(&path).map_err(|error| {
                    format!("cannot remove newly-created installation identity: {error}")
                }),
                Ok(_) => Err(format!(
                    "installation identity restore found non-file {}",
                    path.display()
                )),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(format!(
                    "cannot inspect installation identity restore path: {error}"
                )),
            },
        }
    }

    /// Verifiable herdr-mcp product evidence inside a config root. A marker file
    /// alone is never enough; the root must already carry artifacts the service
    /// install writes (runtime generations/current, state db, server log, or the
    /// service mutation lock).
    fn config_root_has_product_evidence(config_dir: &Path) -> bool {
        let runtime_current = config_dir.join("runtime/current");
        if fs::symlink_metadata(&runtime_current).is_ok() {
            return true;
        }
        for owned in ["state.db", "server.log", "service-mutation.lock", "runtime"] {
            if path_present(&config_dir.join(owned)) {
                return true;
            }
        }
        false
    }

    fn read_installation_identity(
        config_dir: &Path,
        instance: &InstanceId,
    ) -> Result<Option<InstallationIdentity>, String> {
        let path = config_dir.join(INSTALL_IDENTITY_NAME);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "cannot inspect installation identity {}: {error}",
                    path.display()
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 8192 {
            return Err(format!(
                "installation identity is not an owned regular file: {}",
                path.display()
            ));
        }
        let bytes = fs::read(&path).map_err(|error| {
            format!(
                "cannot read installation identity {}: {error}",
                path.display()
            )
        })?;
        let identity: InstallationIdentity = serde_json::from_slice(&bytes)
            .map_err(|error| format!("cannot parse installation identity: {error}"))?;
        if identity.schema_version != INSTALL_IDENTITY_SCHEMA
            || identity.instance.as_deref() != instance.name()
            || Path::new(&identity.config_root) != config_dir
        {
            return Err(
                "installation identity does not match this instance/config root".to_owned(),
            );
        }
        Ok(Some(identity))
    }

    fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
        reject_symlink_if_present(path, "managed product identity")?;
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent", path.display()))?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = parent.join(format!(
            ".{}.tmp-{}-{stamp}",
            path.file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("product-install.json"),
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| format!("cannot create product identity temp file: {error}"))?;
        file.write_all(bytes)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot persist product identity: {error}"))?;
        fs::rename(&temp, path).map_err(|error| {
            let _ = fs::remove_file(&temp);
            format!("cannot commit product identity: {error}")
        })
    }

    pub(super) fn reinstall() -> Result<ExitCode, String> {
        let paths = RuntimePaths::discover()?;
        let home = home_dir()?;
        let _safe_config = validate_config_root(&home, &paths.config_dir, &paths.instance)?;
        let _product_lock = ProductMutationLock::acquire(&home, &paths.instance)?;
        let service_mutation_lock = crate::service_manager::acquire_mutation_lock()?;
        let status = crate::service_manager::doctor_status()?;
        let service_installed = preflight_service(&paths, &status, "reinstall")?;
        let before_generation = status
            .get("generation")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let integration_snapshot =
            ReinstallIntegrationSnapshot::capture(&home, &_safe_config, &paths.instance)?;

        // Product reinstall intentionally bypasses `service_lifecycle`: that
        // wrapper reconciles the independent Herdr supervisor. The service
        // manager is the transactional herdr-mcp-only install primitive.
        let code = crate::service_manager::run_with_mutation_lock(
            ServiceCommand::Install { adopt_node: false },
            &service_mutation_lock,
        )?;
        if code != ExitCode::SUCCESS {
            return Ok(code);
        }

        // Reconcile only herdr-mcp integrations that already exist. If a
        // post-commit integration step fails after the service generation
        // changed, compensate through the service-manager rollback primitive
        // (never the Herdr-supervisor wrapper) and reconcile integrations back
        // to the restored generation before returning the original failure.
        let integration = (|| -> Result<Option<Value>, String> {
            crate::link::reconcile_after_service_generation_change(&paths)?;
            if paths.instance.is_default() {
                Ok(Some(
                    crate::native_host_install::sync_owned_runtime_from_active()?,
                ))
            } else {
                Ok(None)
            }
        })();
        let native_host = match integration {
            Ok(value) => value,
            Err(error) => {
                compensate_failed_reinstall(
                    &paths,
                    &service_mutation_lock,
                    service_installed,
                    before_generation.as_deref(),
                    &integration_snapshot,
                    &error,
                )?;
                return Err(error);
            }
        };

        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "action": "product-reinstall",
                "instance": paths.instance.name(),
                "config_preserved": true,
                "credentials_preserved": true,
                "generation_retention": "normal service GC policy (active and rollback-safe retained set)",
                "native_host_sync": native_host,
                "herdr_preserved": true,
            }))
            .map_err(|error| format!("cannot encode reinstall result: {error}"))?
        );
        Ok(ExitCode::SUCCESS)
    }

    fn compensate_failed_reinstall(
        paths: &RuntimePaths,
        service_mutation_lock: &crate::service_manager::ServiceMutationLease,
        service_was_installed: bool,
        before_generation: Option<&str>,
        integration_snapshot: &ReinstallIntegrationSnapshot,
        original_error: &str,
    ) -> Result<(), String> {
        let after = crate::service_manager::doctor_status()?;
        let after_generation = after.get("generation").and_then(Value::as_str);
        if before_generation != after_generation {
            let command = if service_was_installed {
                ServiceCommand::Rollback
            } else {
                ServiceCommand::Uninstall
            };
            let compensation = crate::service_manager::run_with_mutation_lock(
                command,
                service_mutation_lock,
            )
            .map_err(|error| {
                format!(
                    "reinstall integration failed ({original_error}); service compensation also failed: {error}"
                )
            })?;
            if compensation != ExitCode::SUCCESS {
                return Err(format!(
                    "reinstall integration failed ({original_error}); service compensation returned failure"
                ));
            }
        }

        integration_snapshot.restore().map_err(|error| {
            format!(
                "reinstall integration failed ({original_error}); service compensation completed but integration snapshot restore failed: {error}"
            )
        })?;

        if !service_was_installed {
            return Ok(());
        }

        crate::link::reconcile_after_service_generation_change(paths).map_err(|error| {
            format!(
                "reinstall integration failed ({original_error}); rollback succeeded but Link compensation failed: {error}"
            )
        })?;
        if paths.instance.is_default() {
            crate::native_host_install::sync_owned_runtime_from_active().map_err(|error| {
                format!(
                    "reinstall integration failed ({original_error}); rollback succeeded but Native Host compensation failed: {error}"
                )
            })?;
        }
        Ok(())
    }

    pub(super) fn uninstall() -> Result<ExitCode, String> {
        let paths = RuntimePaths::discover()?;
        let home = home_dir()?;
        let safe_config = validate_config_root(&home, &paths.config_dir, &paths.instance)?;
        let _product_lock = ProductMutationLock::acquire(&home, &paths.instance)?;
        let existing_journal = read_existing_journal(&safe_config, &paths.instance)?;

        // All destructive ownership decisions happen before the first mutation.
        // A resumed transaction may rely only on ownership evidence persisted
        // before its first mutation; it must not rediscover an orphan as owned.
        let service_status = crate::service_manager::doctor_status()?;
        let service_installed = preflight_service(&paths, &service_status, "uninstall")?;
        let launch_agents = preflight_launch_agents(&home, &safe_config, &paths.instance)?;
        let scheduler_recorded = existing_journal.as_ref().is_some_and(|journal| {
            journal
                .ownership_proof
                .iter()
                .any(|proof| proof == "owned-auto-update-scheduler")
        });
        let scheduler_present = if paths.instance.is_default() {
            if let Some(journal) = existing_journal.as_ref() {
                let current = crate::update_scheduler::product_uninstall_preflight()?;
                if phase_done(journal, PHASE_AUTO_UPDATE) {
                    if current {
                        return Err(
                            "the auto-update scheduler reappeared after its product-uninstall phase completed; refusing to remove a new cohort"
                                .to_owned(),
                        );
                    }
                    false
                } else if scheduler_recorded {
                    true
                } else if current {
                    return Err(
                        "an auto-update scheduler appeared after product uninstall started; refusing to widen the recorded ownership cohort"
                            .to_owned(),
                    );
                } else {
                    false
                }
            } else {
                crate::update_scheduler::product_uninstall_preflight()?
            }
        } else {
            false
        };
        let native_host_snapshot = if paths.instance.is_default() {
            if let Some(journal) = existing_journal.as_ref() {
                if phase_done(journal, PHASE_NATIVE_HOST) {
                    if crate::native_host_install::product_uninstall_preflight()? {
                        return Err(
                            "a Native Host reappeared after its product-uninstall phase completed; refusing to remove a new cohort"
                                .to_owned(),
                        );
                    }
                    None
                } else if let Some(snapshot) = journal.native_host_snapshot.clone() {
                    Some(snapshot)
                } else if crate::native_host_install::product_uninstall_preflight()? {
                    return Err(
                        "a Native Host appeared after product uninstall started; refusing to widen the recorded ownership cohort"
                            .to_owned(),
                    );
                } else {
                    None
                }
            } else {
                crate::native_host_install::product_uninstall_snapshot()?
            }
        } else {
            None
        };
        let user_cli = if paths.instance.is_default() {
            preflight_user_cli(&home, &safe_config)?
        } else {
            UserCliPlan {
                path: crate::user_cli::user_cli_path(&home),
                disposition: UserCliDisposition::Absent,
            }
        };
        let config_present = path_present(&safe_config);
        let installation_identity_present = if existing_journal.is_none() {
            read_installation_identity(&safe_config, &paths.instance)?.is_some()
        } else {
            false
        };

        if let Some(journal) = existing_journal.as_ref() {
            if phase_done(journal, PHASE_SERVICE) && service_installed {
                return Err(
                    "the service reappeared after its product-uninstall phase completed; refusing to remove a new service cohort"
                        .to_owned(),
                );
            }
            if phase_done(journal, PHASE_LAUNCH_AGENTS)
                && launch_agents.iter().any(|plan| plan.present)
            {
                return Err(
                    "a Link/watchdog LaunchAgent reappeared after its product-uninstall phase completed; refusing to remove a new cohort"
                        .to_owned(),
                );
            }
            if phase_done(journal, PHASE_USER_CLI)
                && user_cli.disposition == UserCliDisposition::Owned
            {
                return Err(
                    "the managed user CLI reappeared after its product-uninstall phase completed; refusing to remove a new entrypoint"
                        .to_owned(),
                );
            }
        }

        let mut ownership_proof = existing_journal
            .as_ref()
            .map(|journal| journal.ownership_proof.clone())
            .unwrap_or_default();
        if existing_journal.is_none() {
            if installation_identity_present {
                ownership_proof.push("owned-installation-marker".to_owned());
            }
            if service_installed {
                ownership_proof.push("owned-service".to_owned());
            }
            if launch_agents.iter().any(|plan| plan.present) {
                ownership_proof.push("owned-launch-agent".to_owned());
            }
            if scheduler_present {
                ownership_proof.push("owned-auto-update-scheduler".to_owned());
            }
            if native_host_snapshot.is_some() {
                ownership_proof.push("owned-native-host".to_owned());
            }
            if user_cli.disposition == UserCliDisposition::Owned {
                ownership_proof.push("owned-user-cli".to_owned());
            }
        }
        if config_present && existing_journal.is_none() && ownership_proof.is_empty() {
            return Err(format!(
                "refusing to recursively remove {}: path shape alone is not herdr-mcp ownership evidence; no installation marker, owned service/LaunchAgent/Native Host/user CLI, or resumable product journal was found",
                safe_config.display()
            ));
        }

        let work_needed = config_present
            || service_installed
            || scheduler_present
            || launch_agents.iter().any(|plan| plan.present)
            || native_host_snapshot.is_some()
            || user_cli.disposition == UserCliDisposition::Owned;
        if !work_needed {
            print_uninstall_result(
                &paths,
                &safe_config,
                UninstallResultReport {
                    config_removed: false,
                    service_removed: false,
                    native_host_removed: false,
                    user_cli_removed: false,
                    scheduler_removed: false,
                    launch_agents_removed: Vec::new(),
                    journal_resumed: false,
                },
            )?;
            return Ok(ExitCode::SUCCESS);
        }

        let (mut journal, resumed) = load_or_create_journal(
            &safe_config,
            &paths.instance,
            ownership_proof,
            native_host_snapshot,
        )?;

        // Fence updates before removing any external component. This blocks a
        // scheduled or already-detached updater from racing product teardown
        // and resurrecting the service after it has been removed.
        if paths.instance.is_default() {
            crate::update_scheduler::arm_service_uninstall_fence()?;
        }

        let scheduler_removed = if !phase_done(&journal, PHASE_AUTO_UPDATE) {
            let removed = if scheduler_present {
                crate::update_scheduler::remove_before_service_uninstall_checked()?
                    .get("removed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            } else {
                false
            };
            mark_phase(&safe_config, &mut journal, PHASE_AUTO_UPDATE)?;
            removed
        } else {
            false
        };

        let mut removed_launch_agents = Vec::new();
        if !phase_done(&journal, PHASE_LAUNCH_AGENTS) {
            for plan in &launch_agents {
                if plan.present {
                    remove_launch_agent(plan)?;
                    removed_launch_agents.push(plan.label.clone());
                }
            }
            mark_phase(&safe_config, &mut journal, PHASE_LAUNCH_AGENTS)?;
        }

        let native_host_removed = if !phase_done(&journal, PHASE_NATIVE_HOST) {
            let removed = if let Some(snapshot) = journal.native_host_snapshot.as_ref() {
                crate::native_host_install::product_uninstall_owned_from_snapshot(snapshot)?;
                true
            } else {
                false
            };
            mark_phase(&safe_config, &mut journal, PHASE_NATIVE_HOST)?;
            removed
        } else {
            false
        };

        // Stop the service through the product's fail-closed launchctl path
        // before invoking the narrower service cleanup primitive. This prevents
        // service_manager's diagnostic `is_loaded` fallback from being the
        // authority for a destructive product uninstall.
        let service_removed = if !phase_done(&journal, PHASE_SERVICE) {
            let removed = if service_installed {
                ensure_service_stopped_fail_closed(&paths.instance.service_label())?;
                let code = crate::service_manager::run(ServiceCommand::Uninstall)?;
                if code != ExitCode::SUCCESS {
                    return Err("service uninstall did not complete successfully".to_owned());
                }
                true
            } else {
                false
            };
            mark_phase(&safe_config, &mut journal, PHASE_SERVICE)?;
            removed
        } else {
            false
        };

        let user_cli_removed = if !phase_done(&journal, PHASE_USER_CLI) {
            let removed = remove_user_cli_if_owned(&user_cli)?;
            mark_phase(&safe_config, &mut journal, PHASE_USER_CLI)?;
            removed
        } else {
            false
        };
        if !phase_done(&journal, PHASE_EXTERNAL_COMPLETE) {
            mark_phase(&safe_config, &mut journal, PHASE_EXTERNAL_COMPLETE)?;
        }

        // `safe_config` is the canonical path validated once before mutation.
        // Never re-resolve it against a possibly symlinked lexical HOME after
        // external artifacts have already been removed.
        remove_config_root(&safe_config)?;

        print_uninstall_result(
            &paths,
            &safe_config,
            UninstallResultReport {
                config_removed: true,
                service_removed,
                native_host_removed,
                user_cli_removed,
                scheduler_removed,
                launch_agents_removed: removed_launch_agents,
                journal_resumed: resumed,
            },
        )?;
        Ok(ExitCode::SUCCESS)
    }

    fn print_uninstall_result(
        paths: &RuntimePaths,
        config_dir: &Path,
        report: UninstallResultReport,
    ) -> Result<(), String> {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "action": "product-uninstall",
                "instance": paths.instance.name(),
                "scope": if paths.instance.is_default() { "default-product" } else { "named-instance-only" },
                "config_removed": report.config_removed.then(|| config_dir.to_path_buf()),
                "service_removed": report.service_removed,
                "native_host_removed": report.native_host_removed,
                "user_cli_removed": report.user_cli_removed,
                "auto_update_scheduler_removed": report.scheduler_removed,
                "launch_agents_removed": report.launch_agents_removed,
                "journal_resumed": report.journal_resumed,
                "herdr_preserved": true,
                "herdr_config_preserved": home_dir()?.join(".config/herdr"),
                "credentials_preserved": [
                    "macOS Keychain authorization",
                    "macOS TCC authorization",
                    "Cloudflare/browser account state"
                ],
            }))
            .map_err(|error| format!("cannot encode uninstall result: {error}"))?
        );
        Ok(())
    }

    fn preflight_service(
        paths: &RuntimePaths,
        status: &Value,
        action: &str,
    ) -> Result<bool, String> {
        let label = paths.instance.service_label();
        let actual_loaded = launchd_loaded(&label)?;
        validate_service_shape(status, actual_loaded, action)?;

        let implementation = status
            .get("implementation")
            .and_then(Value::as_str)
            .ok_or_else(|| "service status is missing implementation".to_owned())?;
        if implementation == "missing" {
            if let Some(path) = status.get("plist").and_then(Value::as_str) {
                let path = Path::new(path);
                if path_present(path) {
                    return Err(format!(
                        "{action} found a service plist but could not prove Rust ownership: {}",
                        path.display()
                    ));
                }
            }
            return Ok(false);
        }

        let plist = status
            .get("plist")
            .and_then(Value::as_str)
            .ok_or_else(|| "Rust service status is missing plist path".to_owned())?;
        reject_symlink(Path::new(plist), "service plist")?;
        Ok(true)
    }

    fn validate_service_shape(
        status: &Value,
        actual_loaded: bool,
        action: &str,
    ) -> Result<(), String> {
        let implementation = status
            .get("implementation")
            .and_then(Value::as_str)
            .ok_or_else(|| "service status is missing implementation".to_owned())?;
        match implementation {
            "rust" => Ok(()),
            "missing" => {
                let reported_loaded = status
                    .get("loaded")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if reported_loaded || actual_loaded {
                    Err(format!(
                        "{action} found a loaded service without an inspectable owned plist; refusing blind mutation"
                    ))
                } else {
                    Ok(())
                }
            }
            other => Err(format!(
                "{action} refuses non-Rust service ownership ({other}); use the explicit migration/adoption workflow"
            )),
        }
    }

    fn preflight_launch_agents(
        home: &Path,
        config_dir: &Path,
        instance: &InstanceId,
    ) -> Result<Vec<LaunchAgentRemoval>, String> {
        let mut plans = Vec::new();
        if instance.is_default() {
            for label in [
                LINK_LABEL,
                LINK_PROD_LABEL,
                crate::link::LINK_RUST_CANDIDATE_LABEL,
            ] {
                plans.push(preflight_link_agent(home, config_dir, label)?);
            }
        }
        plans.push(preflight_watchdog(
            home,
            config_dir,
            &instance.watchdog_label(),
            "watchdog.sh",
        )?);
        plans.push(preflight_watchdog(
            home,
            config_dir,
            &instance.health_watchdog_label(),
            "health-watchdog.sh",
        )?);
        Ok(plans)
    }

    fn preflight_link_agent(
        home: &Path,
        config_dir: &Path,
        label: &str,
    ) -> Result<LaunchAgentRemoval, String> {
        let path = launch_agent_path(home, label);
        let loaded = launchd_loaded(label)?;
        if !path_present(&path) {
            if loaded {
                return Err(format!(
                    "uninstall found loaded Link {label} without an inspectable plist; refusing blind removal"
                ));
            }
            return Ok(absent_plan(label, path));
        }
        reject_symlink(&path, "Link LaunchAgent plist")?;
        let implementation = verify_owned_link_plist(&path, label, config_dir)?;
        Ok(LaunchAgentRemoval {
            label: label.to_owned(),
            path,
            present: true,
            reason: format!("owned herdr-mcp Link ({implementation})"),
        })
    }

    fn verify_owned_link_plist(
        path: &Path,
        expected_label: &str,
        config_dir: &Path,
    ) -> Result<&'static str, String> {
        let value = plist::Value::from_file(path)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        let dict = value
            .as_dictionary()
            .ok_or_else(|| format!("{} plist root must be a dictionary", path.display()))?;
        let label = dict
            .get("Label")
            .and_then(plist::Value::as_string)
            .unwrap_or("");
        if label != expected_label {
            return Err(format!(
                "Link plist Label={label} does not match expected {expected_label}; refusing removal"
            ));
        }
        let args = plist_program_arguments_from_dict(dict)?;
        let expected_binary = config_dir.join("runtime/current/herdr-mcp");
        let implementation = if args.len() == 3
            && Path::new(&args[0]) == expected_binary
            && args[1] == "link"
            && args[2] == "run"
        {
            let working = dict
                .get("WorkingDirectory")
                .and_then(plist::Value::as_string)
                .ok_or_else(|| format!("{expected_label} is missing WorkingDirectory"))?;
            if lexical_normalize(Path::new(working))? != lexical_normalize(config_dir)? {
                return Err(format!(
                    "{expected_label} Rust WorkingDirectory is outside this herdr-mcp config root"
                ));
            }
            "rust"
        } else if args.len() == 2 && node_program_is_owned(&args)? {
            // Historical Node Link plists sometimes pointed directly at a
            // repository checkout and omitted WorkingDirectory. If one is
            // present, require it to contain the exact daemon script (or be
            // the managed config root) before accepting the legacy cohort.
            if let Some(working) = dict
                .get("WorkingDirectory")
                .and_then(plist::Value::as_string)
            {
                let working = lexical_normalize(Path::new(working))?;
                let script = lexical_normalize(Path::new(&args[1]))?;
                let config = lexical_normalize(config_dir)?;
                if !script.starts_with(&working) && working != config {
                    return Err(format!(
                        "{expected_label} legacy Node WorkingDirectory does not own its daemon script"
                    ));
                }
            }
            "node-legacy"
        } else {
            return Err(format!(
                "{expected_label} ProgramArguments do not match an owned managed-runtime or recognized local legacy Link"
            ));
        };

        if let Some(env) = dict
            .get("EnvironmentVariables")
            .and_then(plist::Value::as_dictionary)
        {
            for key in ["HERDR_RUNTIME_CONTROL_PATH", "HERDR_RUNTIME_STATUS_PATH"] {
                if let Some(raw) = env.get(key).and_then(plist::Value::as_string) {
                    let resolved = lexical_normalize(Path::new(raw))?;
                    if !resolved.starts_with(lexical_normalize(config_dir)?) {
                        return Err(format!(
                            "{expected_label} {key} is outside this herdr-mcp config root"
                        ));
                    }
                }
            }
        }
        Ok(implementation)
    }

    fn node_program_is_owned(args: &[String]) -> Result<bool, String> {
        let first = Path::new(&args[0]);
        let first_name = first.file_name().and_then(OsStr::to_str).unwrap_or("");
        if first_name != "node" && first_name != "nodejs" {
            return Ok(false);
        }
        let script = lexical_normalize(Path::new(&args[1]))?;
        Ok(script.ends_with(Path::new("dist/link/macos-daemon.js")))
    }

    fn preflight_watchdog(
        home: &Path,
        config_dir: &Path,
        label: &str,
        script_basename: &str,
    ) -> Result<LaunchAgentRemoval, String> {
        let path = launch_agent_path(home, label);
        let loaded = launchd_loaded(label)?;
        if !path_present(&path) {
            if loaded {
                return Err(format!(
                    "uninstall found loaded watchdog {label} without an inspectable plist"
                ));
            }
            return Ok(absent_plan(label, path));
        }
        reject_symlink(&path, "watchdog plist")?;
        let value = plist::Value::from_file(&path)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        let dict = value
            .as_dictionary()
            .ok_or_else(|| "watchdog plist root must be a dictionary".to_owned())?;
        let plist_label = dict
            .get("Label")
            .and_then(plist::Value::as_string)
            .unwrap_or("");
        let args = plist_program_arguments_from_dict(dict)?;
        let expected_script = config_dir.join(script_basename);
        let expected_args = vec![
            "/bin/bash".to_owned(),
            expected_script.to_string_lossy().into_owned(),
            "once".to_owned(),
        ];
        let owned = plist_label == label && args == expected_args;
        if !owned {
            return Err(format!(
                "watchdog {label} does not match the exact owned argv contract (/bin/bash {} once)",
                expected_script.display()
            ));
        }
        Ok(LaunchAgentRemoval {
            label: label.to_owned(),
            path,
            present: true,
            reason: format!("owned herdr-mcp {script_basename}"),
        })
    }

    fn remove_launch_agent(plan: &LaunchAgentRemoval) -> Result<(), String> {
        ensure_launchd_job_stopped_fail_closed(&plan.label)?;
        match fs::remove_file(&plan.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "cannot remove owned LaunchAgent {} ({}): {error}",
                plan.path.display(),
                plan.reason
            )),
        }
    }

    fn launchd_loaded(label: &str) -> Result<bool, String> {
        let target = format!("gui/{}/{}", unsafe { libc::geteuid() }, label);
        let output = Command::new("/bin/launchctl")
            .args(["print", &target])
            .output()
            .map_err(|error| format!("cannot query launchd for {label}: {error}"))?;
        interpret_launchctl_print(
            output.status.success(),
            &String::from_utf8_lossy(&output.stderr),
        )
        .map_err(|detail| format!("cannot establish launchd ownership state for {label}: {detail}"))
    }

    fn ensure_service_stopped_fail_closed(label: &str) -> Result<(), String> {
        ensure_launchd_job_stopped_fail_closed(label)
    }

    fn ensure_launchd_job_stopped_fail_closed(label: &str) -> Result<(), String> {
        ensure_launchd_job_stopped_with(
            label,
            LAUNCHD_BOOTOUT_MAX_POLLS,
            || launchd_loaded(label),
            || {
                let target = format!("gui/{}/{}", unsafe { libc::geteuid() }, label);
                let output = Command::new("/bin/launchctl")
                    .args(["bootout", &target])
                    .output()
                    .map_err(|error| {
                        format!("cannot bootout owned LaunchAgent {label}: {error}")
                    })?;
                if output.status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "cannot bootout owned LaunchAgent {label}: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ))
                }
            },
            thread::sleep,
        )
    }

    fn ensure_launchd_job_stopped_with<Loaded, Bootout, Sleep>(
        label: &str,
        max_polls: usize,
        mut loaded: Loaded,
        mut bootout: Bootout,
        mut sleep: Sleep,
    ) -> Result<(), String>
    where
        Loaded: FnMut() -> Result<bool, String>,
        Bootout: FnMut() -> Result<(), String>,
        Sleep: FnMut(Duration),
    {
        if !loaded()? {
            return Ok(());
        }
        bootout()?;
        for poll in 0..=max_polls {
            if !loaded()? {
                return Ok(());
            }
            if poll == max_polls {
                return Err(format!(
                    "LaunchAgent {label} remained loaded for {}ms after bootout; refusing product cleanup",
                    LAUNCHD_BOOTOUT_POLL_INTERVAL.as_millis() * max_polls as u128
                ));
            }
            sleep(LAUNCHD_BOOTOUT_POLL_INTERVAL);
        }
        unreachable!("bounded launchd bootout loop must return")
    }

    fn interpret_launchctl_print(success: bool, stderr: &str) -> Result<bool, String> {
        if success {
            return Ok(true);
        }
        let lower = stderr.to_ascii_lowercase();
        if lower.contains("could not find service")
            || lower.contains("could not find specified service")
            || lower.contains("service not found")
            || lower.contains("no such process")
        {
            return Ok(false);
        }
        Err(if stderr.trim().is_empty() {
            "launchctl print failed without a not-found diagnostic".to_owned()
        } else {
            stderr.trim().to_owned()
        })
    }

    fn preflight_user_cli(home: &Path, config_dir: &Path) -> Result<UserCliPlan, String> {
        let path = crate::user_cli::user_cli_path(home);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(UserCliPlan {
                    path,
                    disposition: UserCliDisposition::Absent,
                });
            }
            Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
        };
        if !metadata.file_type().is_symlink() {
            return Ok(UserCliPlan {
                path,
                disposition: UserCliDisposition::Preserve,
            });
        }
        let target = fs::read_link(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let disposition = classify_user_cli_target(&path, &target, config_dir)?;
        Ok(UserCliPlan { path, disposition })
    }

    fn classify_user_cli_target(
        cli_path: &Path,
        raw_target: &Path,
        config_dir: &Path,
    ) -> Result<UserCliDisposition, String> {
        let resolved = if raw_target.is_absolute() {
            lexical_normalize(raw_target)?
        } else {
            lexical_normalize(
                &cli_path
                    .parent()
                    .ok_or_else(|| "user CLI path has no parent".to_owned())?
                    .join(raw_target),
            )?
        };
        let config = lexical_normalize(config_dir)?;
        let current = config.join("runtime/current/herdr-mcp");
        let generations = config.join("runtime/generations");
        if resolved == current || resolved.starts_with(&generations) {
            return Ok(UserCliDisposition::Owned);
        }
        if resolved.starts_with(&config) {
            return Err(format!(
                "user CLI {} resolves inside the config root to an unrecognized target {}; refusing to leave a dangling entrypoint",
                cli_path.display(),
                resolved.display()
            ));
        }
        Ok(UserCliDisposition::Preserve)
    }

    fn remove_user_cli_if_owned(plan: &UserCliPlan) -> Result<bool, String> {
        if plan.disposition != UserCliDisposition::Owned {
            return Ok(false);
        }
        match fs::remove_file(&plan.path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!(
                "cannot remove owned user CLI {}: {error}",
                plan.path.display()
            )),
        }
    }

    fn validate_config_root(
        home: &Path,
        config_dir: &Path,
        instance: &InstanceId,
    ) -> Result<PathBuf, String> {
        if !config_dir.is_absolute() {
            return Err(format!(
                "refusing non-absolute herdr-mcp config root {}",
                config_dir.display()
            ));
        }
        reject_dot_components(config_dir)?;
        let expected_leaf = instance.config_leaf();
        let actual_leaf = config_dir
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| format!("config root has no valid leaf: {}", config_dir.display()))?;
        let explicit_override = std::env::var_os("HERDR_MCP_CONFIG_DIR").is_some();
        if actual_leaf.starts_with("herdr-mcp") && actual_leaf != expected_leaf {
            return Err(format!(
                "refusing config root {} because its herdr-mcp leaf belongs to a different instance; expected {expected_leaf}",
                config_dir.display()
            ));
        }
        if !explicit_override && actual_leaf != expected_leaf {
            return Err(format!(
                "refusing config root {} for this instance; expected leaf {expected_leaf}",
                config_dir.display()
            ));
        }
        if !config_dir.starts_with(home) {
            return Err(format!(
                "product uninstall only removes config roots below HOME; refusing {}",
                config_dir.display()
            ));
        }

        reject_symlink_ancestors_below_home(home, config_dir)?;
        let resolved_home = fs::canonicalize(home)
            .map_err(|error| format!("cannot canonicalize HOME {}: {error}", home.display()))?;
        let resolved = canonicalize_with_missing_tail(config_dir)?;
        if !resolved.starts_with(&resolved_home) {
            return Err(format!(
                "resolved config root escaped the expected HOME/instance boundary: {}",
                resolved.display()
            ));
        }
        if !explicit_override && resolved.file_name() != Some(OsStr::new(&expected_leaf)) {
            return Err(format!(
                "resolved config root changed instance leaf unexpectedly: {}",
                resolved.display()
            ));
        }
        let independent_herdr = canonicalize_with_missing_tail(&home.join(".config/herdr"))?;
        if resolved == independent_herdr || resolved.starts_with(&independent_herdr) {
            return Err(format!(
                "refusing independent Herdr config path {}",
                resolved.display()
            ));
        }
        if resolved == resolved_home
            || resolved == canonicalize_with_missing_tail(&home.join(".config"))?
        {
            return Err(format!(
                "refusing unsafe config root {}",
                resolved.display()
            ));
        }
        Ok(resolved)
    }

    fn reject_dot_components(path: &Path) -> Result<(), String> {
        if path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return Err(format!(
                "refusing config path containing '.' or '..': {}",
                path.display()
            ));
        }
        Ok(())
    }

    fn reject_symlink_ancestors_below_home(home: &Path, path: &Path) -> Result<(), String> {
        let relative = path
            .strip_prefix(home)
            .map_err(|_| format!("{} is not below HOME {}", path.display(), home.display()))?;
        let mut current = home.to_path_buf();
        for component in relative.components() {
            current.push(component.as_os_str());
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(format!(
                        "refusing config path with symlink component {}",
                        current.display()
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => {
                    return Err(format!("cannot inspect {}: {error}", current.display()));
                }
            }
        }
        Ok(())
    }

    fn canonicalize_with_missing_tail(path: &Path) -> Result<PathBuf, String> {
        let mut cursor = path.to_path_buf();
        let mut tail = Vec::new();
        loop {
            match fs::canonicalize(&cursor) {
                Ok(mut resolved) => {
                    for part in tail.iter().rev() {
                        resolved.push(part);
                    }
                    return Ok(resolved);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let name = cursor.file_name().ok_or_else(|| {
                        format!(
                            "cannot resolve missing path boundary for {}",
                            path.display()
                        )
                    })?;
                    tail.push(name.to_os_string());
                    cursor = cursor
                        .parent()
                        .ok_or_else(|| format!("cannot resolve parent for {}", path.display()))?
                        .to_path_buf();
                }
                Err(error) => {
                    return Err(format!("cannot canonicalize {}: {error}", cursor.display()));
                }
            }
        }
    }

    fn lexical_normalize(path: &Path) -> Result<PathBuf, String> {
        if !path.is_absolute() {
            return Err(format!("expected absolute path, got {}", path.display()));
        }
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                    out.push(component.as_os_str())
                }
                Component::CurDir => {}
                Component::ParentDir => {
                    if !out.pop() {
                        return Err(format!("path escapes filesystem root: {}", path.display()));
                    }
                }
            }
        }
        Ok(out)
    }

    fn remove_config_root(safe_config: &Path) -> Result<(), String> {
        let metadata = match fs::symlink_metadata(safe_config) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "cannot inspect config root {}: {error}",
                    safe_config.display()
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "refusing recursive removal of non-directory/symlink config root {}",
                safe_config.display()
            ));
        }
        fs::remove_dir_all(safe_config).map_err(|error| {
            format!(
                "cannot remove config root {}: {error}",
                safe_config.display()
            )
        })
    }

    fn read_existing_journal(
        config_dir: &Path,
        instance: &InstanceId,
    ) -> Result<Option<UninstallJournal>, String> {
        let path = config_dir.join(JOURNAL_NAME);
        if !path_present(&path) {
            return Ok(None);
        }
        reject_symlink(&path, "product uninstall journal")?;
        let bytes = fs::read(&path).map_err(|error| {
            format!("cannot read uninstall journal {}: {error}", path.display())
        })?;
        if bytes.len() > 64 * 1024 {
            return Err("product uninstall journal exceeds 64 KiB safety bound".to_owned());
        }
        let journal: UninstallJournal = serde_json::from_slice(&bytes)
            .map_err(|error| format!("cannot parse uninstall journal: {error}"))?;
        validate_journal(&journal, config_dir, instance)?;
        Ok(Some(journal))
    }

    fn load_or_create_journal(
        config_dir: &Path,
        instance: &InstanceId,
        ownership_proof: Vec<String>,
        native_host_snapshot: Option<Value>,
    ) -> Result<(UninstallJournal, bool), String> {
        if let Some(journal) = read_existing_journal(config_dir, instance)? {
            return Ok((journal, true));
        }
        if ownership_proof.is_empty() {
            return Err(
                "refusing to create a destructive product journal without external ownership proof"
                    .to_owned(),
            );
        }
        fs::create_dir_all(config_dir).map_err(|error| {
            format!(
                "cannot create uninstall journal root {}: {error}",
                config_dir.display()
            )
        })?;
        let journal = UninstallJournal {
            schema_version: JOURNAL_SCHEMA,
            action: "product-uninstall".to_owned(),
            instance: instance.name().map(str::to_owned),
            config_root: config_dir.to_string_lossy().into_owned(),
            ownership_proof,
            native_host_snapshot,
            completed: Vec::new(),
        };
        write_journal(config_dir, &journal)?;
        Ok((journal, false))
    }

    fn validate_journal(
        journal: &UninstallJournal,
        config_dir: &Path,
        instance: &InstanceId,
    ) -> Result<(), String> {
        if journal.schema_version != JOURNAL_SCHEMA
            || journal.action != "product-uninstall"
            || journal.instance.as_deref() != instance.name()
            || Path::new(&journal.config_root) != config_dir
            || journal.ownership_proof.is_empty()
            || journal.ownership_proof.iter().any(|proof| {
                !matches!(
                    proof.as_str(),
                    "owned-installation-marker"
                        | "owned-service"
                        | "owned-launch-agent"
                        | "owned-native-host"
                        | "owned-user-cli"
                        | "owned-auto-update-scheduler"
                )
            })
            || journal
                .completed
                .iter()
                .any(|phase| !KNOWN_PHASES.contains(&phase.as_str()))
        {
            return Err(
                "product uninstall journal does not match this instance/config root".to_owned(),
            );
        }
        Ok(())
    }

    fn phase_done(journal: &UninstallJournal, phase: &str) -> bool {
        journal.completed.iter().any(|item| item == phase)
    }

    fn mark_phase(
        config_dir: &Path,
        journal: &mut UninstallJournal,
        phase: &str,
    ) -> Result<(), String> {
        if !journal.completed.iter().any(|item| item == phase) {
            journal.completed.push(phase.to_owned());
            write_journal(config_dir, journal)?;
        }
        Ok(())
    }

    fn write_journal(config_dir: &Path, journal: &UninstallJournal) -> Result<(), String> {
        let target = config_dir.join(JOURNAL_NAME);
        reject_symlink_if_present(&target, "product uninstall journal")?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = config_dir.join(format!(
            ".{JOURNAL_NAME}.tmp-{}-{stamp}",
            std::process::id()
        ));
        let bytes = serde_json::to_vec_pretty(journal)
            .map_err(|error| format!("cannot encode uninstall journal: {error}"))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| format!("cannot create uninstall journal temp file: {error}"))?;
        file.write_all(&bytes)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot persist uninstall journal: {error}"))?;
        fs::rename(&temp, &target).map_err(|error| {
            let _ = fs::remove_file(&temp);
            format!("cannot commit uninstall journal: {error}")
        })?;
        Ok(())
    }

    fn plist_program_arguments_from_dict(dict: &plist::Dictionary) -> Result<Vec<String>, String> {
        let args = dict
            .get("ProgramArguments")
            .and_then(plist::Value::as_array)
            .ok_or_else(|| "ProgramArguments missing".to_owned())?;
        let mut out = Vec::with_capacity(args.len());
        for value in args {
            let value = value
                .as_string()
                .ok_or_else(|| "ProgramArguments must contain only strings".to_owned())?;
            out.push(value.to_owned());
        }
        Ok(out)
    }

    fn launch_agent_path(home: &Path, label: &str) -> PathBuf {
        home.join("Library/LaunchAgents")
            .join(format!("{label}.plist"))
    }

    fn absent_plan(label: &str, path: PathBuf) -> LaunchAgentRemoval {
        LaunchAgentRemoval {
            label: label.to_owned(),
            path,
            present: false,
            reason: "absent".to_owned(),
        }
    }

    fn reject_symlink(path: &Path, label: &str) -> Result<(), String> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("cannot inspect {label} {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("{label} must not be a symlink: {}", path.display()));
        }
        Ok(())
    }

    fn reject_symlink_if_present(path: &Path, label: &str) -> Result<(), String> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err(format!("{label} must not be a symlink: {}", path.display()))
            }
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "cannot inspect {label} {}: {error}",
                path.display()
            )),
        }
    }

    fn path_present(path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }

    fn home_dir() -> Result<PathBuf, String> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is required for product lifecycle".to_owned())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn temp_home(name: &str) -> PathBuf {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let home = std::env::temp_dir().join(format!(
                "herdr-product-{name}-{}-{stamp}",
                std::process::id()
            ));
            fs::create_dir_all(home.join(".config")).unwrap();
            home
        }

        #[test]
        fn config_root_guard_binds_instance_and_preserves_independent_herdr() {
            let home = temp_home("paths");
            let default = InstanceId::default_instance();
            let named = InstanceId::parse("uat").unwrap();
            let default_root = home.join(".config/herdr-mcp");
            let named_root = home.join(".config/herdr-mcp-uat");
            assert!(validate_config_root(&home, &default_root, &default).is_ok());
            assert!(validate_config_root(&home, &named_root, &named).is_ok());
            assert!(validate_config_root(&home, &home.join(".config/herdr"), &default).is_err());
            assert!(validate_config_root(&home, &named_root, &default).is_err());
            assert!(validate_config_root(&home, &default_root, &named).is_err());
            assert!(
                validate_config_root(&home, &home.join(".config/herdr-mcp/../herdr"), &default)
                    .is_err()
            );
            let _ = fs::remove_dir_all(home);
        }

        #[test]
        fn config_root_guard_rejects_symlink_ancestor_escape() {
            use std::os::unix::fs::symlink;
            let home = temp_home("symlink");
            let outside = temp_home("outside");
            symlink(&outside, home.join(".config/redirect")).unwrap();
            let path = home.join(".config/redirect/herdr-mcp");
            assert!(validate_config_root(&home, &path, &InstanceId::default_instance()).is_err());
            let _ = fs::remove_dir_all(home);
            let _ = fs::remove_dir_all(outside);
        }

        #[test]
        fn explicit_custom_config_root_is_allowed_but_cross_instance_leaf_is_not() {
            let _guard = crate::test_env::lock();
            let home = temp_home("custom-config");
            let previous = std::env::var_os("HERDR_MCP_CONFIG_DIR");
            let custom = home.join("state/custom-runtime");
            unsafe { std::env::set_var("HERDR_MCP_CONFIG_DIR", &custom) };
            assert!(validate_config_root(&home, &custom, &InstanceId::default_instance()).is_ok());
            assert!(
                validate_config_root(
                    &home,
                    &home.join(".config/herdr-mcp-uat"),
                    &InstanceId::default_instance(),
                )
                .is_err()
            );
            unsafe {
                match previous {
                    Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                    None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
                }
            }
            let _ = fs::remove_dir_all(home);
        }

        #[test]
        fn missing_service_must_not_be_loaded() {
            let missing = json!({"implementation":"missing","loaded":false});
            assert!(validate_service_shape(&missing, false, "uninstall").is_ok());
            assert!(validate_service_shape(&missing, true, "uninstall").is_err());
            let reported_loaded = json!({"implementation":"missing","loaded":true});
            assert!(validate_service_shape(&reported_loaded, false, "reinstall").is_err());
            let foreign = json!({"implementation":"node","loaded":true});
            assert!(validate_service_shape(&foreign, true, "uninstall").is_err());
        }

        #[test]
        fn launchctl_query_only_treats_explicit_not_found_as_absent() {
            assert!(interpret_launchctl_print(true, "").unwrap());
            assert!(
                !interpret_launchctl_print(
                    false,
                    "Could not find service foo in domain for user gui: 501"
                )
                .unwrap()
            );
            assert!(interpret_launchctl_print(false, "Operation not permitted").is_err());
            assert!(interpret_launchctl_print(false, "").is_err());
        }

        #[test]
        fn product_uninstall_waits_for_bootout_to_converge_before_cleanup() {
            let mut states = std::collections::VecDeque::from([true, true, true, false]);
            let mut bootouts = 0usize;
            let mut sleeps = 0usize;
            ensure_launchd_job_stopped_with(
                "dev.herdr-mcp.uat.server",
                4,
                || Ok(states.pop_front().unwrap_or(false)),
                || {
                    bootouts += 1;
                    Ok(())
                },
                |_| sleeps += 1,
            )
            .unwrap();
            assert_eq!(bootouts, 1);
            assert_eq!(sleeps, 2);
        }

        #[test]
        fn product_uninstall_bootout_wait_is_bounded_and_fail_closed() {
            let mut bootouts = 0usize;
            let mut sleeps = 0usize;
            let error = ensure_launchd_job_stopped_with(
                "dev.herdr-mcp.uat.server",
                2,
                || Ok(true),
                || {
                    bootouts += 1;
                    Ok(())
                },
                |_| sleeps += 1,
            )
            .unwrap_err();
            assert!(error.contains("remained loaded"));
            assert!(error.contains("100ms"));
            assert_eq!(bootouts, 1);
            assert_eq!(sleeps, 2);
        }

        #[test]
        fn product_uninstall_does_not_bootout_an_absent_job() {
            let mut bootouts = 0usize;
            let mut sleeps = 0usize;
            ensure_launchd_job_stopped_with(
                "dev.herdr-mcp.uat.server",
                2,
                || Ok(false),
                || {
                    bootouts += 1;
                    Ok(())
                },
                |_| sleeps += 1,
            )
            .unwrap();
            assert_eq!(bootouts, 0);
            assert_eq!(sleeps, 0);
        }

        #[test]
        fn link_ownership_requires_exact_label_and_known_rust_or_legacy_program() {
            let home = temp_home("link");
            let config = home.join(".config/herdr-mcp");
            fs::create_dir_all(&config).unwrap();
            let plist_path = home.join("link.plist");
            let mut dict = plist::Dictionary::new();
            dict.insert(
                "Label".to_owned(),
                plist::Value::String(LINK_PROD_LABEL.to_owned()),
            );
            dict.insert(
                "WorkingDirectory".to_owned(),
                plist::Value::String(config.to_string_lossy().into_owned()),
            );
            dict.insert(
                "ProgramArguments".to_owned(),
                plist::Value::Array(vec![
                    plist::Value::String(
                        config
                            .join("runtime/current/herdr-mcp")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    plist::Value::String("link".to_owned()),
                    plist::Value::String("run".to_owned()),
                ]),
            );
            plist::Value::Dictionary(dict.clone())
                .to_file_xml(&plist_path)
                .unwrap();
            assert_eq!(
                verify_owned_link_plist(&plist_path, LINK_PROD_LABEL, &config).unwrap(),
                "rust"
            );
            dict.insert(
                "Label".to_owned(),
                plist::Value::String("foreign.label".to_owned()),
            );
            plist::Value::Dictionary(dict)
                .to_file_xml(&plist_path)
                .unwrap();
            assert!(verify_owned_link_plist(&plist_path, LINK_PROD_LABEL, &config).is_err());

            let mut legacy = plist::Dictionary::new();
            legacy.insert(
                "Label".to_owned(),
                plist::Value::String(LINK_PROD_LABEL.to_owned()),
            );
            legacy.insert(
                "ProgramArguments".to_owned(),
                plist::Value::Array(vec![
                    plist::Value::String("/usr/local/bin/node".to_owned()),
                    plist::Value::String(
                        home.join("checkout/dist/link/macos-daemon.js")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                ]),
            );
            plist::Value::Dictionary(legacy.clone())
                .to_file_xml(&plist_path)
                .unwrap();
            assert_eq!(
                verify_owned_link_plist(&plist_path, LINK_PROD_LABEL, &config).unwrap(),
                "node-legacy"
            );
            legacy.insert(
                "ProgramArguments".to_owned(),
                plist::Value::Array(vec![
                    plist::Value::String("/usr/local/bin/node".to_owned()),
                    plist::Value::String(
                        home.join("checkout/other.js")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                ]),
            );
            plist::Value::Dictionary(legacy)
                .to_file_xml(&plist_path)
                .unwrap();
            assert!(verify_owned_link_plist(&plist_path, LINK_PROD_LABEL, &config).is_err());
            let _ = fs::remove_dir_all(home);
        }

        #[test]
        fn watchdog_requires_exact_owned_script_path() {
            let home = temp_home("watchdog");
            let config = home.join(".config/herdr-mcp");
            fs::create_dir_all(&config).unwrap();
            let label = InstanceId::default_instance().watchdog_label();
            let path = launch_agent_path(&home, &label);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut dict = plist::Dictionary::new();
            dict.insert("Label".to_owned(), plist::Value::String(label.clone()));
            dict.insert(
                "ProgramArguments".to_owned(),
                plist::Value::Array(vec![
                    plist::Value::String("/bin/bash".to_owned()),
                    plist::Value::String(config.join("watchdog.sh").to_string_lossy().into_owned()),
                    plist::Value::String("once".to_owned()),
                ]),
            );
            plist::Value::Dictionary(dict).to_file_xml(&path).unwrap();
            // Pure ownership portion: exact script is accepted by the same args gate.
            let value = plist::Value::from_file(&path).unwrap();
            let args = plist_program_arguments_from_dict(value.as_dictionary().unwrap()).unwrap();
            assert!(
                args.iter()
                    .any(|arg| Path::new(arg) == config.join("watchdog.sh"))
            );
            assert!(
                !args
                    .iter()
                    .any(|arg| Path::new(arg) == config.join("other/watchdog.sh"))
            );
            let _ = fs::remove_dir_all(home);
        }

        #[test]
        fn relative_user_cli_target_is_classified_semantically() {
            let home = Path::new("/Users/tester");
            let cli = home.join(".local/bin/herdr-mcp");
            let config = home.join(".config/herdr-mcp");
            assert_eq!(
                classify_user_cli_target(
                    &cli,
                    Path::new("../../.config/herdr-mcp/runtime/current/herdr-mcp"),
                    &config,
                )
                .unwrap(),
                UserCliDisposition::Owned
            );
            assert!(
                classify_user_cli_target(
                    &cli,
                    Path::new("../../.config/herdr-mcp/foreign"),
                    &config
                )
                .is_err()
            );
            assert_eq!(
                classify_user_cli_target(&cli, Path::new("/opt/local/bin/herdr-mcp"), &config)
                    .unwrap(),
                UserCliDisposition::Preserve
            );
        }

        #[test]
        fn installation_identity_binds_exact_instance_and_config_root() {
            let _guard = crate::test_env::lock();
            let home = temp_home("install-identity");
            let config = home.join(".config/herdr-mcp");
            fs::create_dir_all(&config).unwrap();
            let instance = InstanceId::default_instance();
            let identity = InstallationIdentity {
                schema_version: INSTALL_IDENTITY_SCHEMA,
                instance: None,
                config_root: config.to_string_lossy().into_owned(),
            };
            let bytes = serde_json::to_vec_pretty(&identity).unwrap();
            write_private_atomic(&config.join(INSTALL_IDENTITY_NAME), &bytes).unwrap();
            assert_eq!(
                read_installation_identity(&config, &instance)
                    .unwrap()
                    .unwrap(),
                identity
            );
            assert!(
                read_installation_identity(&config, &InstanceId::parse("uat").unwrap()).is_err()
            );
            let _ = fs::remove_dir_all(home);
        }

        #[test]
        fn preflight_refuses_pre_existing_non_empty_foreign_root_before_any_write() {
            let _guard = crate::test_env::lock();
            let home = temp_home("marker-spoof");
            let instance = InstanceId::default_instance();
            let old_home = std::env::var_os("HOME");
            unsafe { std::env::set_var("HOME", &home) };
            // A pre-existing arbitrary override directory with unrelated content
            // must be refused BEFORE service install writes any product file.
            let foreign = home.join("state/custom-runtime");
            fs::create_dir_all(&foreign).unwrap();
            fs::write(foreign.join("unrelated-data.bin"), b"keep me").unwrap();
            let previous = std::env::var_os("HERDR_MCP_CONFIG_DIR");
            unsafe { std::env::set_var("HERDR_MCP_CONFIG_DIR", &foreign) };
            let paths = RuntimePaths::discover().unwrap();
            let safe = validate_config_root(&home, &paths.config_dir, &instance).unwrap();
            assert!(
                preflight_installation_identity().is_err(),
                "must refuse a pre-existing non-empty foreign root before any write"
            );
            // No product file or marker may have been created.
            assert!(!safe.join(INSTALL_IDENTITY_NAME).exists());
            assert!(!safe.join("state.db").exists());
            assert!(!safe.join("runtime").exists());
            assert!(foreign.join("unrelated-data.bin").exists());
            unsafe {
                match previous {
                    Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                    None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
                }
                match old_home {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
            }
            let _ = fs::remove_dir_all(home);
        }

        #[test]
        fn preflight_allows_fresh_empty_and_existing_owned_roots() {
            let _guard = crate::test_env::lock();
            let home = temp_home("marker-fresh");
            let old_home = std::env::var_os("HOME");
            unsafe { std::env::set_var("HOME", &home) };
            let previous = std::env::var_os("HERDR_MCP_CONFIG_DIR");
            // Fresh/empty root is safe to claim.
            let fresh = home.join("state/fresh-runtime");
            unsafe { std::env::set_var("HERDR_MCP_CONFIG_DIR", &fresh) };
            assert!(preflight_installation_identity().is_ok());
            // A root already carrying product evidence (a prior service install)
            // is claimable even if it is non-empty.
            let evidence = home.join("state/evidence-runtime");
            fs::create_dir_all(&evidence).unwrap();
            fs::create_dir_all(evidence.join("runtime/current")).unwrap();
            unsafe { std::env::set_var("HERDR_MCP_CONFIG_DIR", &evidence) };
            assert!(preflight_installation_identity().is_ok());
            // A root with a valid existing marker is claimable. The marker's
            // config_root is the canonicalized path (as record_installation_identity
            // writes it), so canonicalize before writing.
            let marked = home.join("state/marked-runtime");
            fs::create_dir_all(&marked).unwrap();
            let marked_canon = fs::canonicalize(&marked).unwrap();
            let identity = InstallationIdentity {
                schema_version: INSTALL_IDENTITY_SCHEMA,
                instance: None,
                config_root: marked_canon.to_string_lossy().into_owned(),
            };
            let bytes = serde_json::to_vec_pretty(&identity).unwrap();
            write_private_atomic(&marked.join(INSTALL_IDENTITY_NAME), &bytes).unwrap();
            unsafe { std::env::set_var("HERDR_MCP_CONFIG_DIR", &marked) };
            assert!(preflight_installation_identity().is_ok());
            unsafe {
                match previous {
                    Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                    None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
                }
                match old_home {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
            }
            let _ = fs::remove_dir_all(home);
        }

        #[test]
        fn record_identity_refuses_pre_existing_non_empty_foreign_root() {
            let _guard = crate::test_env::lock();
            let home = temp_home("marker-spoof-record");
            let instance = InstanceId::default_instance();
            let old_home = std::env::var_os("HOME");
            unsafe { std::env::set_var("HOME", &home) };
            let foreign = home.join("state/custom-runtime");
            fs::create_dir_all(&foreign).unwrap();
            fs::write(foreign.join("unrelated-data.bin"), b"keep me").unwrap();
            let previous = std::env::var_os("HERDR_MCP_CONFIG_DIR");
            unsafe { std::env::set_var("HERDR_MCP_CONFIG_DIR", &foreign) };
            let paths = RuntimePaths::discover().unwrap();
            let safe = validate_config_root(&home, &paths.config_dir, &instance).unwrap();
            assert!(
                record_installation_identity().is_err(),
                "must refuse to claim a pre-existing non-empty foreign root"
            );
            assert!(!safe.join(INSTALL_IDENTITY_NAME).exists());
            assert!(foreign.join("unrelated-data.bin").exists());
            unsafe {
                match previous {
                    Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                    None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
                }
                match old_home {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
            }
            let _ = fs::remove_dir_all(home);
        }

        #[test]
        fn record_identity_allows_fresh_or_product_evidence_root() {
            let _guard = crate::test_env::lock();
            let home = temp_home("marker-fresh-record");
            let old_home = std::env::var_os("HOME");
            unsafe { std::env::set_var("HOME", &home) };
            let previous = std::env::var_os("HERDR_MCP_CONFIG_DIR");
            let fresh = home.join("state/fresh-runtime");
            unsafe { std::env::set_var("HERDR_MCP_CONFIG_DIR", &fresh) };
            assert!(record_installation_identity().is_ok());
            assert!(fresh.join(INSTALL_IDENTITY_NAME).exists());
            let evidence = home.join("state/evidence-runtime");
            fs::create_dir_all(&evidence).unwrap();
            fs::create_dir_all(evidence.join("runtime/current")).unwrap();
            unsafe { std::env::set_var("HERDR_MCP_CONFIG_DIR", &evidence) };
            assert!(record_installation_identity().is_ok());
            assert!(evidence.join(INSTALL_IDENTITY_NAME).exists());
            unsafe {
                match previous {
                    Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                    None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
                }
                match old_home {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
            }
            let _ = fs::remove_dir_all(home);
        }

        #[test]
        fn product_mutation_lock_is_single_writer_and_released_on_drop() {
            let home = temp_home("product-lock");
            let instance = InstanceId::default_instance();
            let first = ProductMutationLock::acquire(&home, &instance).unwrap();
            assert!(ProductMutationLock::acquire(&home, &instance).is_err());
            drop(first);
            assert!(ProductMutationLock::acquire(&home, &instance).is_ok());
            let _ = fs::remove_dir_all(home);
        }

        #[test]
        fn reinstall_snapshot_restores_exact_precommit_integration_files() {
            let home = temp_home("reinstall-snapshot");
            let config = home.join(".config/herdr-mcp");
            fs::create_dir_all(home.join("Library/LaunchAgents")).unwrap();
            fs::create_dir_all(&config).unwrap();
            let link = launch_agent_path(&home, LINK_PROD_LABEL);
            let prod_control = config.join("runtime-control-prod.json");
            fs::write(&link, b"old-link").unwrap();
            fs::write(&prod_control, b"old-control").unwrap();
            let snapshot = ReinstallIntegrationSnapshot::capture(
                &home,
                &config,
                &InstanceId::default_instance(),
            )
            .unwrap();
            fs::write(&link, b"new-link").unwrap();
            fs::remove_file(&prod_control).unwrap();
            fs::write(config.join("runtime-control.json"), b"new-extra").unwrap();
            snapshot.restore().unwrap();
            assert_eq!(fs::read(&link).unwrap(), b"old-link");
            assert_eq!(fs::read(&prod_control).unwrap(), b"old-control");
            assert!(!config.join("runtime-control.json").exists());
            let _ = fs::remove_dir_all(home);
        }

        #[test]
        fn uninstall_journal_is_bounded_instance_specific_and_idempotent() {
            let home = temp_home("journal");
            let config = home.join(".config/herdr-mcp");
            let instance = InstanceId::default_instance();
            let proof = vec!["owned-service".to_owned()];
            let (mut journal, resumed) =
                load_or_create_journal(&config, &instance, proof.clone(), None).unwrap();
            assert!(!resumed);
            assert_eq!(journal.ownership_proof, proof);
            mark_phase(&config, &mut journal, PHASE_SERVICE).unwrap();
            mark_phase(&config, &mut journal, PHASE_SERVICE).unwrap();
            assert_eq!(journal.completed, vec![PHASE_SERVICE.to_owned()]);
            let (loaded, resumed) =
                load_or_create_journal(&config, &instance, Vec::new(), None).unwrap();
            assert!(resumed);
            assert_eq!(loaded.completed, journal.completed);
            assert!(
                validate_journal(&loaded, &config, &InstanceId::parse("uat").unwrap()).is_err()
            );
            let _ = fs::remove_dir_all(home);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_exec_fence_is_explicit() {
        let _guard = crate::test_env::lock();
        let previous = std::env::var_os("HERDR_MCP_EXEC_ID");
        unsafe { std::env::set_var("HERDR_MCP_EXEC_ID", "test-exec") };
        assert!(refuse_managed_exec_mutation().is_err());
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HERDR_MCP_EXEC_ID", value),
                None => std::env::remove_var("HERDR_MCP_EXEC_ID"),
            }
        }
    }
}
