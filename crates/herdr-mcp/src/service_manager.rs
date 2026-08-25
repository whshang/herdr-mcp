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

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use crate::paths::RuntimePaths;
    use crate::state_store::{ServiceRollbackRecord, StateStore};
    use plist::{Dictionary, Value as PlistValue};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::env;
    use std::ffi::OsStr;
    use std::fs::{self, File, OpenOptions};
    use std::io::{Cursor, Read, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
    use std::path::{Component, Path, PathBuf};
    use std::process::{Command, Output};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    const SERVICE_LABEL: &str = "dev.herdr-mcp.server";
    const WATCHDOG_LABEL: &str = "dev.herdr-mcp.watchdog";
    const SERVICE_IMPL: &str = "rust-v1";
    const DEFAULT_PORT: u16 = 8772;
    const HEALTH_BUDGET: Duration = Duration::from_secs(10);
    const LAUNCHD_ABSENT_BUDGET: Duration = Duration::from_secs(2);
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
        backups_dir: PathBuf,
        extension_socket: PathBuf,
        herdr_socket: PathBuf,
        log_path: PathBuf,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
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
        let result = match command {
            ServiceCommand::Install { adopt_node } => install(&paths, adopt_node)?,
            ServiceCommand::Status => status(&paths)?,
            ServiceCommand::Start => start(&paths)?,
            ServiceCommand::Stop => stop(&paths)?,
            ServiceCommand::Restart => restart(&paths)?,
            ServiceCommand::Rollback => rollback(&paths)?,
            ServiceCommand::Uninstall => uninstall(&paths)?,
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
                backups_dir: config_dir.join("backups"),
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

    fn install(paths: &ServicePaths, adopt_node: bool) -> Result<Value, String> {
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

        let activation = (|| -> Result<(), String> {
            if rollback.watchdog_was_loaded {
                bootout(WATCHDOG_LABEL)?;
            }
            if rollback.watchdog_plist.is_some() {
                remove_regular_file(&paths.watchdog_plist)?;
            }
            if rollback.server_was_loaded {
                bootout(SERVICE_LABEL)?;
            }
            atomic_write(&paths.plist, &new_plist, 0o600)?;
            switch_current(paths, &generation)?;
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
            let rollback_error = rollback_install(paths, &rollback).err();
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
            return Err(match rollback_error {
                Some(rollback_error) => format!(
                    "Rust service activation failed: {error}; rollback also failed: {rollback_error}"
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

        Ok(json!({
            "ok": true,
            "implementation": "rust",
            "label": SERVICE_LABEL,
            "generation": generation.generation_id,
            "sha256": generation.sha256,
            "runtime_binary": generation.binary,
            "current_binary": paths.current_binary,
            "plist": paths.plist,
            "extension_socket": paths.extension_socket,
            "adopted_node": existing.kind == ServiceKind::Node,
            "retired_legacy_watchdog": rollback.watchdog_plist.is_some(),
            "backups": backups,
            "rollback_id": rollback_id,
            "rollback_ready": rollback_id.is_some(),
            "evidence_recorded": evidence_recorded,
        }))
    }

    fn status(paths: &ServicePaths) -> Result<Value, String> {
        let bytes = read_optional_bounded(&paths.plist, 256 * 1024)?;
        let descriptor = describe_service(bytes.as_deref(), paths)?;
        let loaded = is_loaded(SERVICE_LABEL);
        let health = if descriptor.kind == ServiceKind::Rust && loaded {
            health_once(DEFAULT_PORT)
        } else {
            false
        };
        let generation = descriptor.env.get("HERDR_MCP_RUNTIME_GENERATION").cloned();
        Ok(json!({
            "ok": descriptor.kind == ServiceKind::Rust && loaded && health,
            "label": SERVICE_LABEL,
            "implementation": match descriptor.kind {
                ServiceKind::Missing => "missing",
                ServiceKind::Rust => "rust",
                ServiceKind::Node => "node",
                ServiceKind::Other => "other",
            },
            "loaded": loaded,
            "healthy": health,
            "generation": generation,
            "current_target": current_target(paths)?.map(|path| path.to_string_lossy().into_owned()),
            "plist": paths.plist,
            "extension_socket": paths.extension_socket,
            "legacy_watchdog_present": paths.watchdog_plist.exists(),
            "legacy_watchdog_loaded": is_loaded(WATCHDOG_LABEL),
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

    fn rollback(paths: &ServicePaths) -> Result<Value, String> {
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
            .begin_latest_service_rollback()?
            .ok_or_else(|| "no ready service rollback is available".to_owned())?;

        let release_claim = |store: &StateStore, reason: String| -> Result<Value, String> {
            let release =
                store.finish_service_rollback(&rollback.rollback_id, "ready", now_ms_i64());
            Err(match release {
                Ok(()) => reason,
                Err(error) => {
                    format!("{reason}; rollback claim could not be released safely: {error}")
                }
            })
        };

        if rollback.activated_generation_id != current_generation {
            return release_claim(
                &store,
                format!(
                    "ready rollback {} targets generation {}, but current service is {}",
                    rollback.rollback_id, rollback.activated_generation_id, current_generation
                ),
            );
        }

        let Some(server_backup) = rollback.server_plist_backup.as_deref() else {
            return release_claim(
                &store,
                format!(
                    "rollback {} has no server plist backup",
                    rollback.rollback_id
                ),
            );
        };
        let source_bytes = match read_owned_backup(paths, server_backup) {
            Ok(bytes) => bytes,
            Err(error) => return release_claim(&store, error),
        };
        let source = match describe_service(Some(&source_bytes), paths) {
            Ok(source) => source,
            Err(error) => return release_claim(&store, error),
        };
        let source_kind = service_kind_name(&source.kind);
        if source_kind != rollback.source_kind {
            return release_claim(
                &store,
                format!(
                    "rollback source mismatch: ledger={}, backup={source_kind}",
                    rollback.source_kind
                ),
            );
        }
        if !matches!(source.kind, ServiceKind::Node | ServiceKind::Rust) {
            return release_claim(
                &store,
                "rollback server backup is not an owned Node/Rust service".to_owned(),
            );
        }

        let watchdog_bytes = match rollback.watchdog_plist_backup.as_deref() {
            Some(path) => match read_owned_backup(paths, path) {
                Ok(bytes) => {
                    if !watchdog_is_legacy_owned(Some(&bytes))? {
                        return release_claim(
                            &store,
                            "rollback watchdog backup is not legacy herdr-mcp watchdog".to_owned(),
                        );
                    }
                    Some(bytes)
                }
                Err(error) => return release_claim(&store, error),
            },
            None => None,
        };
        if rollback.watchdog_was_loaded && watchdog_bytes.is_none() {
            return release_claim(
                &store,
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
            return release_claim(
                &store,
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

        let apply = (|| -> Result<(), String> {
            if current_was_loaded {
                bootout(SERVICE_LABEL)?;
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
            )
            .err();
            let state = if restore_error.is_some() {
                "rollback_failed"
            } else {
                "ready"
            };
            let _ = store.finish_service_rollback(&rollback.rollback_id, state, now_ms_i64());
            return Err(match restore_error {
                Some(restore_error) => format!(
                    "service rollback failed: {error}; restoring current Rust service also failed: {restore_error}"
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
        Ok(json!({
            "ok": true,
            "action": "rollback",
            "rollback_id": rollback.rollback_id,
            "from_generation": rollback.activated_generation_id,
            "restored_implementation": rollback.source_kind,
            "restored_loaded": rollback.server_was_loaded,
            "restored_watchdog": rollback.watchdog_was_loaded,
            "evidence_recorded": evidence_recorded,
        }))
    }

    fn uninstall(paths: &ServicePaths) -> Result<Value, String> {
        let descriptor = require_rust_service(paths)?;
        if is_loaded(SERVICE_LABEL) {
            bootout(SERVICE_LABEL)?;
        }
        remove_regular_file(&paths.plist)?;
        remove_current_if_owned(paths)?;
        let evidence_recorded = record_action(paths, "uninstall", "ok", &descriptor, None);
        Ok(json!({
            "ok": true,
            "action": "uninstall",
            "label": SERVICE_LABEL,
            "generations_preserved": true,
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
        Ok(args.iter().any(|value| value.ends_with("watchdog.sh")) && args.contains(&"once"))
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

    fn rollback_install(paths: &ServicePaths, rollback: &RollbackState) -> Result<(), String> {
        if is_loaded(SERVICE_LABEL) {
            bootout(SERVICE_LABEL)?;
        }
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
    ) -> Result<(), String> {
        if is_loaded(WATCHDOG_LABEL) {
            bootout(WATCHDOG_LABEL)?;
        }
        remove_regular_file(&paths.watchdog_plist)?;
        if is_loaded(SERVICE_LABEL) {
            bootout(SERVICE_LABEL)?;
        }
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
        if !is_loaded(label) {
            return Ok(());
        }
        run_launchctl([OsStr::new("bootout"), OsStr::new(&target(label))])?;
        wait_launchd_absent(label, LAUNCHD_ABSENT_BUDGET)
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
    }
}
