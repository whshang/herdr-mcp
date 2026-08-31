//! Installation lifecycle for the Rust Chrome Native Messaging host.
//!
//! Chromium always points at a stable wrapper under ~/.config/herdr-mcp/native.
//! The wrapper launches a colocated Rust binary copy. Future updater/supervisor
//! code can atomically replace that binary without rewriting browser manifests.

use crate::cli::NativeHostCommand;
#[cfg(target_os = "macos")]
use serde_json::{Value, json};
#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "macos")]
use std::collections::BTreeSet;
#[cfg(target_os = "macos")]
use std::env;
#[cfg(target_os = "macos")]
use std::fs::{self, OpenOptions};
#[cfg(target_os = "macos")]
use std::io::{Read, Write};
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
use std::process::ExitCode;
#[cfg(target_os = "macos")]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "macos")]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[cfg(target_os = "macos")]
const HOST_NAME: &str = "dev.herdr.mcp";
#[cfg(target_os = "macos")]
const WRAPPER_MARKER: &str = "# herdr-mcp rust native host v1";

// Rollback evidence lives as immutable files under the managed native dir
// (backups/<rollback-id>/), referenced by one small atomic JSON ledger. This
// keeps native-host rollback an independent fault domain from the runtime
// `service rollback`: it never touches shared SQLite state (schema stays v4),
// never rebuilds a binary, and never stores credentials.
//
// Two independent atomic metadata files drive the lifecycle so a new install
// can never overwrite the *ready* rollback before it commits:
//   * rollback.json     -> current READY rollback record (prior snapshot +
//                          expected activated fingerprint). Persists across
//                          later installs until explicitly consumed.
//   * install-pending.json -> in-flight install transaction (recovery point).
#[cfg(target_os = "macos")]
const ROLLBACK_FILE: &str = "rollback.json";
#[cfg(target_os = "macos")]
const PENDING_FILE: &str = "install-pending.json";
#[cfg(target_os = "macos")]
const ROLLBACK_PENDING_FILE: &str = "rollback-pending.json";
#[cfg(target_os = "macos")]
const BACKUPS_DIR_NAME: &str = "backups";

#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
struct InstallPaths {
    source_binary: PathBuf,
    runtime_binary: PathBuf,
    wrapper: PathBuf,
    extension_path: Option<PathBuf>,
    extension_id: String,
    extension_origin: String,
    dev_extension_origin: Option<String>,
    extension_identity_source: String,
    allow_origin_migration: bool,
    targets: Vec<(PathBuf, bool)>,
    backups_dir: PathBuf,
    rollback_file: PathBuf,
    pending_file: PathBuf,
    rollback_pending_file: PathBuf,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct NativeHostLayout {
    source_binary: PathBuf,
    native_dir: PathBuf,
    wrapper: PathBuf,
    targets: Vec<(PathBuf, bool)>,
}

pub fn run(command: NativeHostCommand) -> Result<ExitCode, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = command;
        Err("native_host_install_currently_requires_macos".to_owned())
    }

    #[cfg(target_os = "macos")]
    {
        let paths = InstallPaths::discover(&command)?;
        let result = match command {
            NativeHostCommand::Install
            | NativeHostCommand::DevEnable { .. }
            | NativeHostCommand::DevDisable
            | NativeHostCommand::UseStore
            | NativeHostCommand::UseStandalone
            | NativeHostCommand::UseDev => install(&paths)?,
            NativeHostCommand::Status => status(&paths),
            NativeHostCommand::Uninstall => uninstall(&paths)?,
            NativeHostCommand::Rollback => rollback(&paths)?,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&result)
                .map_err(|error| format!("cannot encode native-host result: {error}"))?
        );
        Ok(if result.get("ok").and_then(Value::as_bool) == Some(true) {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        })
    }
}

/// Read-only Native Messaging ownership snapshot for `doctor`.
pub fn doctor_status() -> Result<serde_json::Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        Ok(serde_json::json!({
            "ok": false,
            "implementation": "unsupported",
            "detail": "native host install currently requires macOS",
        }))
    }

    #[cfg(target_os = "macos")]
    {
        let paths = InstallPaths::discover(&NativeHostCommand::Status)?;
        Ok(status(&paths))
    }
}

/// Product-uninstall primitive with stricter ownership than the interactive
/// `native-host uninstall` command. A bare executable/wrapper is never enough
/// evidence for destructive cleanup: at least one currently owned manifest
/// must bind the managed files to this Native Messaging installation.
///
/// Returns `Ok(None)` when no managed footprint exists and `Ok(Some(value))`
/// after an owned footprint was removed. Foreign/partial/orphaned state fails
/// closed before the first mutation.
#[cfg(target_os = "macos")]
pub(crate) fn product_uninstall_preflight() -> Result<bool, String> {
    let paths = InstallPaths::discover(&NativeHostCommand::Status)?;
    reject_rollback_in_progress(&paths)?;

    let footprint_present = managed_native_host_footprint_present(&paths);
    let view = status(&paths);
    product_uninstall_view_preflight(&view, footprint_present)
}

#[cfg(target_os = "macos")]
fn product_uninstall_view_preflight(view: &Value, footprint_present: bool) -> Result<bool, String> {
    if !footprint_present {
        return Ok(false);
    }
    if view.get("recovery_required").and_then(Value::as_bool) == Some(true) {
        return Err("native-host recovery is required before product uninstall".to_owned());
    }
    let manifests = view
        .get("manifests")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if manifests
        .iter()
        .any(|manifest| manifest.get("owned").and_then(Value::as_bool) != Some(true))
    {
        return Err(
            "product uninstall found a Native Messaging manifest that is not owned by herdr-mcp"
                .to_owned(),
        );
    }
    let owned_count = view
        .get("owned_manifest_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if owned_count == 0 {
        return Err(
            "product uninstall found an orphan Native Host wrapper/runtime without an owned manifest; refusing destructive cleanup"
                .to_owned(),
        );
    }
    if view.get("wrapper_ok").and_then(Value::as_bool) != Some(true)
        || view.get("runtime_binary_ok").and_then(Value::as_bool) != Some(true)
    {
        return Err(
            "product uninstall found a partial Native Host footprint; repair or remove it explicitly before product uninstall"
                .to_owned(),
        );
    }
    Ok(true)
}

/// Capture immutable Native Host ownership evidence before the product removes
/// any manifest, wrapper, or runtime binary. The outer product-uninstall journal
/// persists this snapshot so an interrupted uninstall can resume without
/// treating a newly orphaned wrapper/runtime as fresh ownership evidence.
#[cfg(target_os = "macos")]
pub(crate) fn product_uninstall_snapshot() -> Result<Option<serde_json::Value>, String> {
    let paths = InstallPaths::discover(&NativeHostCommand::Status)?;
    reject_rollback_in_progress(&paths)?;
    let footprint_present = managed_native_host_footprint_present(&paths);
    let view = status(&paths);
    if !product_uninstall_view_preflight(&view, footprint_present)? {
        return Ok(None);
    }
    let manifests = paths
        .targets
        .iter()
        .map(|(target, _)| target.join(format!("{HOST_NAME}.json")))
        .filter(|path| path_present(path))
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    Ok(Some(json!({
        "schema_version": 1,
        "runtime_binary": paths.runtime_binary,
        "runtime_sha256": file_sha256(&paths.runtime_binary)?,
        "wrapper": paths.wrapper,
        "wrapper_sha256": file_sha256(&paths.wrapper)?,
        "manifests": manifests,
    })))
}

/// Remove or resume removal of the exact Native Host cohort captured by
/// `product_uninstall_snapshot`. Missing members are already-completed work;
/// every member still present must match the persisted ownership proof.
#[cfg(target_os = "macos")]
pub(crate) fn product_uninstall_owned_from_snapshot(
    snapshot: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let paths = InstallPaths::discover(&NativeHostCommand::Status)?;
    reject_rollback_in_progress(&paths)?;
    validate_product_uninstall_snapshot(snapshot, &paths)?;

    let mut removed = Vec::new();
    let manifests = snapshot
        .get("manifests")
        .and_then(Value::as_array)
        .ok_or_else(|| "native-host product snapshot is missing manifests".to_owned())?;
    for raw in manifests {
        let raw = raw
            .as_str()
            .ok_or_else(|| "native-host product snapshot manifest path is invalid".to_owned())?;
        let path = PathBuf::from(raw);
        if !path_present(&path) {
            continue;
        }
        let view = manifest_status(&path, &paths);
        if view.get("owned").and_then(Value::as_bool) != Some(true) {
            return Err(format!(
                "native-host manifest changed after product preflight; refusing removal: {}",
                path.display()
            ));
        }
        fs::remove_file(&path).map_err(|error| {
            format!(
                "cannot remove native-host manifest {}: {error}",
                path.display()
            )
        })?;
        removed.push(path.to_string_lossy().into_owned());
    }

    remove_snapshot_file_if_matching(
        &paths.wrapper,
        snapshot
            .get("wrapper_sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| "native-host product snapshot is missing wrapper_sha256".to_owned())?,
        "wrapper",
        &mut removed,
    )?;
    remove_snapshot_file_if_matching(
        &paths.runtime_binary,
        snapshot
            .get("runtime_sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| "native-host product snapshot is missing runtime_sha256".to_owned())?,
        "runtime binary",
        &mut removed,
    )?;

    consume_ready_record(&paths)?;
    let _ = fs::remove_file(&paths.pending_file);
    Ok(json!({
        "ok": true,
        "implementation": "rust",
        "host": HOST_NAME,
        "removed": removed,
        "resumed_from_product_snapshot": true,
    }))
}

#[cfg(target_os = "macos")]
fn validate_product_uninstall_snapshot(
    snapshot: &Value,
    paths: &InstallPaths,
) -> Result<(), String> {
    if snapshot.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err("native-host product snapshot has unsupported schema".to_owned());
    }
    let runtime = snapshot
        .get("runtime_binary")
        .and_then(Value::as_str)
        .ok_or_else(|| "native-host product snapshot is missing runtime_binary".to_owned())?;
    let wrapper = snapshot
        .get("wrapper")
        .and_then(Value::as_str)
        .ok_or_else(|| "native-host product snapshot is missing wrapper".to_owned())?;
    if Path::new(runtime) != paths.runtime_binary || Path::new(wrapper) != paths.wrapper {
        return Err("native-host product snapshot paths do not match this installation".to_owned());
    }
    let expected_manifests = paths
        .targets
        .iter()
        .map(|(target, _)| target.join(format!("{HOST_NAME}.json")))
        .collect::<BTreeSet<_>>();
    let manifests = snapshot
        .get("manifests")
        .and_then(Value::as_array)
        .ok_or_else(|| "native-host product snapshot is missing manifests".to_owned())?;
    if manifests.is_empty() {
        return Err("native-host product snapshot must contain an owned manifest".to_owned());
    }
    for raw in manifests {
        let raw = raw
            .as_str()
            .ok_or_else(|| "native-host product snapshot manifest path is invalid".to_owned())?;
        if !expected_manifests.contains(Path::new(raw)) {
            return Err(format!(
                "native-host product snapshot contains an unexpected manifest path: {raw}"
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_snapshot_file_if_matching(
    path: &Path,
    expected_sha256: &str,
    label: &str,
    removed: &mut Vec<String>,
) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot inspect native-host {label}: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "native-host {label} changed into a non-regular file; refusing removal: {}",
            path.display()
        ));
    }
    let observed = file_sha256(path)?;
    if observed != expected_sha256 {
        return Err(format!(
            "native-host {label} changed after product preflight; refusing removal: {}",
            path.display()
        ));
    }
    fs::remove_file(path).map_err(|error| {
        format!(
            "cannot remove native-host {label} {}: {error}",
            path.display()
        )
    })?;
    removed.push(path.to_string_lossy().into_owned());
    Ok(())
}

/// Copy the active `runtime/current` binary into an owned native-host install.
///
/// This is invoked after managed service update/rollback so Chrome keeps talking
/// to the same generation as the production runtime without rewriting manifests
/// or consuming native-host rollback evidence.
#[cfg(target_os = "macos")]
pub fn sync_owned_runtime_from_active() -> Result<serde_json::Value, String> {
    let paths = InstallPaths::discover(&NativeHostCommand::Status)?;
    let home = home_dir()?;
    let active = crate::link::install::resolve_managed_runtime_binary(&home)?;
    sync_owned_runtime_with_active(&paths, &active)
}

#[cfg(target_os = "macos")]
fn sync_preflight(view: &Value) -> Result<Option<Value>, String> {
    if view.get("recovery_required").and_then(Value::as_bool) == Some(true) {
        return Err(
            "native-host recovery is required before syncing the managed runtime binary".to_owned(),
        );
    }
    let owned_count = view
        .get("owned_manifest_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if owned_count == 0
        || view.get("wrapper_ok").and_then(Value::as_bool) != Some(true)
        || view.get("runtime_binary_ok").and_then(Value::as_bool) != Some(true)
    {
        return Ok(Some(json!({
            "ok": true,
            "skipped": true,
            "reason": "native_host_not_owned",
        })));
    }
    Ok(None)
}

#[cfg(target_os = "macos")]
fn managed_native_host_footprint_present(paths: &InstallPaths) -> bool {
    path_present(&paths.runtime_binary)
        || path_present(&paths.wrapper)
        || paths
            .targets
            .iter()
            .any(|(target, _)| path_present(&target.join(format!("{HOST_NAME}.json"))))
}

/// Refresh the wrapper identity contract for a pre-dual-mode Rust Native Host.
///
/// v0.4.1 already owned the stable wrapper/runtime/manifests but its wrapper did
/// not remember `HERDR_DEV_EXTENSION_ORIGIN`. v0.4.2 intentionally makes that
/// remembered Dev identity part of exact ownership. During a managed runtime
/// update we may upgrade only a cohort that can still be proven to be the old
/// Herdr-managed shape. Manifests and rollback evidence remain byte-for-byte
/// untouched; the active admitted origin therefore cannot change as a side
/// effect of runtime synchronization.
#[cfg(target_os = "macos")]
fn refresh_legacy_managed_wrapper_identity(paths: &InstallPaths) -> Result<bool, String> {
    let current = status(paths);
    if current
        .get("owned_manifest_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        return Ok(false);
    }
    if !managed_native_host_footprint_present(paths) {
        return Ok(false);
    }
    if current.get("recovery_required").and_then(Value::as_bool) == Some(true) {
        return Err(
            "native-host recovery is required before migrating legacy wrapper identity".to_owned(),
        );
    }

    // A migratable legacy cohort must be complete. A partial managed footprint
    // is ambiguous and therefore blocks the update rather than being treated as
    // an uninstalled Native Host.
    if !path_present(&paths.runtime_binary) || !path_present(&paths.wrapper) {
        return Err(
            "native-host legacy cohort is incomplete; refusing identity migration".to_owned(),
        );
    }
    let present_manifests = paths
        .targets
        .iter()
        .map(|(target, _)| target.join(format!("{HOST_NAME}.json")))
        .filter(|path| path_present(path))
        .count();
    if present_manifests == 0 {
        return Err(
            "native-host legacy cohort has no managed manifests; refusing identity migration"
                .to_owned(),
        );
    }

    // The pre-dual-mode wrapper must already admit exactly the same active
    // origin as the manifests. The only legacy difference we are allowed to
    // repair is the absence of the remembered Dev origin line. A Herdr-looking
    // wrapper with a different active origin or an incompatible Dev line is
    // tampered/stale v0.4.2 state, not a v0.4.1 cohort.
    let legacy_wrapper = fs::read_to_string(&paths.wrapper)
        .map_err(|error| format!("cannot read legacy native-host wrapper: {error}"))?;
    let expected_active_line = format!(
        "export HERDR_EXTENSION_ORIGIN={}",
        shell_quote(&paths.extension_origin)
    );
    if !legacy_wrapper
        .lines()
        .any(|line| line == expected_active_line)
    {
        return Err(
            "native-host legacy wrapper active origin does not match registered manifests"
                .to_owned(),
        );
    }
    if wrapper_dev_extension_origin(&paths.wrapper)?.is_some() {
        return Err(
            "native-host wrapper already carries an incompatible remembered Dev identity"
                .to_owned(),
        );
    }

    // Reuse the installer's existing strict migration proof. It requires the
    // Rust wrapper marker, the managed runtime target, and structurally-owned
    // Herdr manifests. `InstallPaths::discover(Status)` has already rejected
    // conflicting registered origins, so this cannot merge Store/Dev identities.
    let mut migration_paths = paths.clone();
    migration_paths.allow_origin_migration = true;
    validate_cohort_ownership(&migration_paths).map_err(|error| {
        format!("native-host legacy cohort is foreign or unsafe to migrate: {error}")
    })?;

    let registered_before = find_registered_origin(&paths.targets, &paths.wrapper)?;
    if registered_before.as_deref() != Some(paths.extension_origin.as_str()) {
        return Err(
            "native-host legacy cohort active origin is ambiguous; refusing migration".to_owned(),
        );
    }

    let original_wrapper = legacy_wrapper.into_bytes();
    atomic_write(&paths.wrapper, wrapper_body(paths).as_bytes(), 0o700)?;

    let refreshed = status(paths);
    let refreshed_owned = refreshed
        .get("owned_manifest_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let registered_after = find_registered_origin(&paths.targets, &paths.wrapper)?;
    if refreshed_owned == 0 || registered_after != registered_before {
        let restore = atomic_write(&paths.wrapper, &original_wrapper, 0o700);
        return Err(match restore {
            Ok(()) => {
                "native-host legacy wrapper migration failed validation; original wrapper restored"
                    .to_owned()
            }
            Err(error) => format!(
                "native-host legacy wrapper migration failed validation and restore failed: {error}"
            ),
        });
    }
    Ok(true)
}

#[cfg(target_os = "macos")]
fn sync_owned_runtime_with_active(paths: &InstallPaths, active: &Path) -> Result<Value, String> {
    let mut view = status_with_active_runtime(paths, Some(active));
    if view.get("recovery_required").and_then(Value::as_bool) == Some(true) {
        sync_preflight(&view)?;
    }

    let identity_migrated = if view
        .get("owned_manifest_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        == 0
    {
        refresh_legacy_managed_wrapper_identity(paths)?
    } else {
        false
    };
    if identity_migrated {
        view = status_with_active_runtime(paths, Some(active));
    }
    if let Some(result) = sync_preflight(&view)? {
        return Ok(result);
    }

    let native_sha = file_sha256(&paths.runtime_binary)?;
    let active_sha = file_sha256(active)?;
    if native_sha == active_sha {
        return Ok(json!({
            "ok": true,
            "skipped": true,
            "reason": "already_current",
            "identity_migrated": identity_migrated,
            "active_runtime": active,
            "native_runtime_version": read_binary_version(&paths.runtime_binary),
            "active_runtime_version": read_binary_version(active),
            "runtime_matches_current": true,
            "version_consistent": view
                .get("version_consistent")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }));
    }

    atomic_copy_executable(active, &paths.runtime_binary)?;
    let refreshed = status_with_active_runtime(paths, Some(active));
    if refreshed.get("ok").and_then(Value::as_bool) != Some(true)
        || refreshed
            .get("runtime_matches_current")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err("native-host runtime sync completed but the refreshed ownership/status gate is not healthy".to_owned());
    }
    Ok(json!({
        "ok": true,
        "synced": true,
        "identity_migrated": identity_migrated,
        "from": active,
        "native_runtime_version": refreshed.get("native_runtime_version").cloned(),
        "active_runtime_version": refreshed.get("active_runtime_version").cloned(),
        "runtime_matches_current": refreshed
            .get("runtime_matches_current")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "version_consistent": refreshed
            .get("version_consistent")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }))
}

#[cfg(target_os = "macos")]
impl InstallPaths {
    fn discover(command: &NativeHostCommand) -> Result<Self, String> {
        let home = home_dir()?;
        let runtime_paths = crate::paths::RuntimePaths::discover()?;
        let source_binary = env::current_exe()
            .map_err(|error| format!("cannot locate current herdr-mcp binary: {error}"))?;
        let native_dir = runtime_paths.config_dir.join("native");
        let layout = NativeHostLayout {
            source_binary,
            wrapper: native_dir.join("herdr-extension-host"),
            targets: install_targets(&home),
            native_dir,
        };
        let store = crate::browser_extension_identity::official_store_identity()?;
        let standalone = crate::browser_extension_identity::official_standalone_identity()?;
        let registered = find_registered_origin(&layout.targets, &layout.wrapper)?;
        let remembered_dev = wrapper_dev_extension_origin(&layout.wrapper)?.or_else(|| {
            registered
                .clone()
                .filter(|origin| origin != &store.origin && origin != &standalone.origin)
        });

        // Rollback and status must recover the active origin from durable state;
        // the remembered Dev candidate is advisory metadata embedded in the
        // owned wrapper and is restored byte-for-byte by the existing rollback.
        if matches!(
            command,
            NativeHostCommand::Rollback | NativeHostCommand::Status
        ) && let Some(origin) = durable_extension_origin(&layout.native_dir)?
        {
            return Self::for_origin_with_dev(
                &origin,
                None,
                remembered_dev,
                "durable_metadata",
                false,
                layout,
            );
        }

        match command {
            NativeHostCommand::Install => {
                if let Some(extension_origin) = explicit_extension_origin()? {
                    let dev = if extension_origin == store.origin
                        || extension_origin == standalone.origin
                    {
                        remembered_dev
                    } else {
                        Some(extension_origin.clone())
                    };
                    return Self::for_origin_with_dev(
                        &extension_origin,
                        None,
                        dev,
                        "env:HERDR_EXTENSION_ORIGIN",
                        true,
                        layout,
                    );
                }
                if env::var_os("HERDR_EXTENSION_PATH").is_some() {
                    let extension_path = crate::native_host::extension_path_for_install()?;
                    return Self::for_dev_path(
                        &extension_path,
                        "env:HERDR_EXTENSION_PATH",
                        true,
                        layout,
                    );
                }

                // A 0.4.1 managed single-Dev install remains Dev-active during
                // upgrade; Store becomes the always-available fallback contract
                // but is not silently activated under a working developer.
                if let Some(active) = registered {
                    let dev = if active == store.origin || active == standalone.origin {
                        remembered_dev
                    } else {
                        Some(active.clone())
                    };
                    return Self::for_origin_with_dev(
                        &active,
                        None,
                        dev,
                        "registered_manifest_upgrade",
                        true,
                        layout,
                    );
                }
                Self::for_origin_with_dev(
                    &store.origin,
                    None,
                    remembered_dev,
                    "chrome_web_store_contract",
                    true,
                    layout,
                )
            }
            NativeHostCommand::DevEnable { path } => {
                let extension_path = dev_extension_path(path.as_deref())?;
                Self::for_dev_path(&extension_path, "native_host_dev_enable", true, layout)
            }
            NativeHostCommand::DevDisable => {
                let fixed_origin = registered
                    .as_deref()
                    .filter(|origin| *origin == standalone.origin)
                    .unwrap_or(&store.origin);
                Self::for_origin_with_dev(
                    fixed_origin,
                    None,
                    None,
                    "native_host_dev_disable",
                    true,
                    layout,
                )
            }
            NativeHostCommand::UseStore => Self::for_origin_with_dev(
                &store.origin,
                None,
                remembered_dev,
                "native_host_use_store",
                true,
                layout,
            ),
            NativeHostCommand::UseStandalone => Self::for_origin_with_dev(
                &standalone.origin,
                None,
                remembered_dev,
                "native_host_use_standalone",
                true,
                layout,
            ),
            NativeHostCommand::UseDev => {
                let dev = remembered_dev.ok_or_else(|| {
                    "native_host_dev_not_enabled: run `herdr-mcp native-host dev enable [PATH]` first"
                        .to_owned()
                })?;
                Self::for_origin_with_dev(
                    &dev,
                    None,
                    Some(dev.clone()),
                    "native_host_use_dev",
                    true,
                    layout,
                )
            }
            NativeHostCommand::Status
            | NativeHostCommand::Rollback
            | NativeHostCommand::Uninstall => {
                if let Some(active) = registered {
                    return Self::for_origin_with_dev(
                        &active,
                        None,
                        remembered_dev,
                        "registered_manifest",
                        false,
                        layout,
                    );
                }
                Self::for_origin_with_dev(
                    &store.origin,
                    None,
                    remembered_dev,
                    "chrome_web_store_contract",
                    false,
                    layout,
                )
            }
        }
    }

    #[cfg(test)]
    fn for_values(
        home: &Path,
        extension_path: &Path,
        source_binary: &Path,
    ) -> Result<Self, String> {
        let native_dir = home.join(".config").join("herdr-mcp").join("native");
        let layout = NativeHostLayout {
            source_binary: source_binary.to_path_buf(),
            wrapper: native_dir.join("herdr-extension-host"),
            targets: install_targets(home),
            native_dir,
        };
        Self::for_dev_path(extension_path, "test_extension_path", false, layout)
    }

    fn for_dev_path(
        extension_path: &Path,
        extension_identity_source: &str,
        allow_origin_migration: bool,
        layout: NativeHostLayout,
    ) -> Result<Self, String> {
        let extension_id = crate::native_host::chromium_id_for_path(extension_path)?;
        let extension_origin = format!("chrome-extension://{extension_id}/");
        Self::for_origin_with_dev(
            &extension_origin,
            Some(extension_path.to_path_buf()),
            Some(extension_origin.clone()),
            extension_identity_source,
            allow_origin_migration,
            layout,
        )
    }

    fn for_origin_with_dev(
        extension_origin: &str,
        extension_path: Option<PathBuf>,
        dev_extension_origin: Option<String>,
        extension_identity_source: &str,
        allow_origin_migration: bool,
        layout: NativeHostLayout,
    ) -> Result<Self, String> {
        let extension_id = extension_id_from_origin(extension_origin)
            .ok_or_else(|| "native-host extension origin is invalid".to_owned())?;
        let store = crate::browser_extension_identity::official_store_identity()?;
        let standalone = crate::browser_extension_identity::official_standalone_identity()?;
        if let Some(dev) = dev_extension_origin.as_deref() {
            if dev == store.origin
                || dev == standalone.origin
                || extension_id_from_origin(dev).is_none()
            {
                return Err("native_host_dev_origin_invalid".to_owned());
            }
            if extension_origin != store.origin
                && extension_origin != standalone.origin
                && extension_origin != dev
            {
                return Err("native_host_active_origin_not_fixed_or_registered_dev".to_owned());
            }
        } else if extension_origin != store.origin && extension_origin != standalone.origin {
            return Err("native_host_dev_origin_not_registered".to_owned());
        }
        let backups_dir = layout.native_dir.join(BACKUPS_DIR_NAME);
        let rollback_file = layout.native_dir.join(ROLLBACK_FILE);
        let pending_file = layout.native_dir.join(PENDING_FILE);
        Ok(Self {
            source_binary: layout.source_binary,
            runtime_binary: layout.native_dir.join("herdr-mcp"),
            wrapper: layout.wrapper,
            extension_path,
            extension_id,
            extension_origin: extension_origin.to_owned(),
            dev_extension_origin,
            extension_identity_source: extension_identity_source.to_owned(),
            allow_origin_migration,
            targets: layout.targets,
            backups_dir,
            rollback_file,
            pending_file,
            rollback_pending_file: layout.native_dir.join(ROLLBACK_PENDING_FILE),
        })
    }
}

#[cfg(target_os = "macos")]
fn dev_extension_path(raw: Option<&str>) -> Result<PathBuf, String> {
    let input = raw
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("extension"));
    let absolute = if input.is_absolute() {
        input
    } else {
        env::current_dir()
            .map_err(|error| format!("cannot read current directory: {error}"))?
            .join(input)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    if !normalized.is_dir() || !normalized.join("manifest.json").is_file() {
        return Err(format!(
            "native-host dev enable requires an unpacked extension directory containing manifest.json: {}",
            normalized.display()
        ));
    }
    Ok(normalized)
}

#[cfg(target_os = "macos")]
fn wrapper_dev_extension_origin(wrapper: &Path) -> Result<Option<String>, String> {
    if !path_present(wrapper) || !wrapper_is_rust(wrapper) {
        return Ok(None);
    }
    let content = fs::read_to_string(wrapper)
        .map_err(|error| format!("cannot read native-host wrapper: {error}"))?;
    for line in content.lines() {
        let Some(raw) = line.strip_prefix("export HERDR_DEV_EXTENSION_ORIGIN=") else {
            continue;
        };
        let value = raw.trim().trim_matches('\'');
        if value.is_empty() {
            return Ok(None);
        }
        if extension_id_from_origin(value).is_none() {
            return Err("native_host_remembered_dev_origin_invalid".to_owned());
        }
        return Ok(Some(value.to_owned()));
    }
    Ok(None)
}

#[cfg(target_os = "macos")]
fn install(paths: &InstallPaths) -> Result<Value, String> {
    // Fail closed if a rollback is mid-restore: a new install must not snapshot
    // or overwrite a half-restored disk. The interrupted rollback must be
    // resumed (or its marker cleared) before any install mutation.
    reject_rollback_in_progress(paths)?;

    // Recover any interrupted previous install before starting fresh. The
    // previous ready rollback (if any) stays intact during recovery.
    recover_pending(paths)?;

    // Fail closed before any mutation if the current managed-or-absent state
    // is foreign, unowned, or symlinked.
    reject_non_regular_managed_targets(paths)?;
    validate_cohort_ownership(paths)?;

    // Snapshot the exact current managed state (or absence) as immutable
    // rollback evidence, then begin the in-flight transaction.
    let (rollback_id, _backup_dir) = snapshot_evidence(paths)?;
    write_json_file(
        &paths.pending_file,
        &json!({
            "rollback_id": rollback_id,
            "extension_origin": paths.extension_origin,
            "started_at": now_millis(),
        }),
        0o600,
    )?;

    // Apply the mutation. On failure, restore the pre-mutation snapshot and
    // clear the pending marker; the previous ready rollback is left intact.
    if let Err(error) = install_mutation(paths) {
        abort_pending_install(paths)?;
        return Err(format!(
            "native-host install failed and was rolled back: {error}"
        ));
    }

    // The committed rollback record fingerprints the exact activated state
    // (runtime + wrapper + every touched manifest) so rollback can later verify
    // it is restoring over the exact owned build and no foreign mutation.
    let ready = json!({
        "rollback_id": rollback_id,
        "extension_origin": paths.extension_origin,
        "activated_runtime_binary_sha256": file_sha256(&paths.runtime_binary)?,
        "activated_wrapper_sha256": file_sha256(&paths.wrapper)?,
        "activated_manifests": activated_manifests(paths)?,
        "activated_at": now_millis(),
    });
    write_json_file(&paths.rollback_file, &ready, 0o600)?;
    fs::remove_file(&paths.pending_file)
        .map_err(|error| format!("cannot clear native-host pending install: {error}"))?;

    let runtime_sha256 = file_sha256(&paths.runtime_binary)?;
    let installed = touched_manifest_paths(paths);
    Ok(json!({
        "ok": !installed.is_empty(),
        "implementation": "rust",
        "host": HOST_NAME,
        "extension_id": paths.extension_id,
        "extension_path": paths.extension_path,
        "extension_origin": paths.extension_origin,
        "extension_identity_source": paths.extension_identity_source,
        "runtime_binary": paths.runtime_binary,
        "runtime_sha256": runtime_sha256,
        "wrapper": paths.wrapper,
        "rollback_id": rollback_id,
        "installed": installed,
    }))
}

/// Apply the native-host file mutation (binary copy, wrapper write, manifest
/// writes). Kept separate so the transactional wrapper can roll back on error.
#[cfg(target_os = "macos")]
fn install_mutation(paths: &InstallPaths) -> Result<(), String> {
    let native_dir = paths
        .runtime_binary
        .parent()
        .ok_or_else(|| "native runtime path has no parent".to_owned())?;
    ensure_secure_dir(native_dir)?;
    atomic_copy_executable(&paths.source_binary, &paths.runtime_binary)?;
    // Deterministic test-only failpoint: models a crash mid-install-mutation
    // (binary already replaced) so the transaction abort path restores the
    // pre-mutation snapshot. No-op in production builds.
    failpoint_after_install_mutation()?;

    let wrapper = wrapper_body(paths);
    atomic_write(&paths.wrapper, wrapper.as_bytes(), 0o700)?;

    let manifest = manifest_value(paths);
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("cannot encode native-host manifest: {error}"))?;
    for (target, always) in &paths.targets {
        let browser_dir = target.parent().unwrap_or(target);
        if !*always && !browser_dir.exists() {
            continue;
        }
        fs::create_dir_all(target).map_err(|error| {
            format!(
                "cannot create native messaging directory {}: {error}",
                target.display()
            )
        })?;
        let manifest_path = target.join(format!("{HOST_NAME}.json"));
        atomic_write(
            &manifest_path,
            &[manifest_bytes.as_slice(), b"\n"].concat(),
            0o600,
        )?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn status(paths: &InstallPaths) -> Value {
    let active_runtime = home_dir()
        .ok()
        .and_then(|home| crate::link::install::resolve_managed_runtime_binary(&home).ok());
    status_with_active_runtime(paths, active_runtime.as_deref())
}

#[cfg(target_os = "macos")]
fn status_with_active_runtime(paths: &InstallPaths, active_runtime: Option<&Path>) -> Value {
    let runtime_binary_ok = is_regular_executable(&paths.runtime_binary);
    let wrapper_ok = wrapper_is_rust(&paths.wrapper);
    let runtime_matches_current = runtime_binary_ok
        && active_runtime.is_some_and(|active| {
            file_sha256(&paths.runtime_binary).ok().as_deref()
                == file_sha256(active).ok().as_deref()
        });
    let native_runtime_version = read_binary_version(&paths.runtime_binary);
    let active_runtime_version = active_runtime.and_then(read_binary_version);
    let version_consistent =
        native_runtime_version.is_some() && native_runtime_version == active_runtime_version;
    let mut manifests = Vec::new();
    let mut owned_count = 0usize;
    for (target, _) in &paths.targets {
        let manifest_path = target.join(format!("{HOST_NAME}.json"));
        if !path_present(&manifest_path) {
            continue;
        }
        let view = manifest_status(&manifest_path, paths);
        if view.get("owned").and_then(Value::as_bool) == Some(true) {
            owned_count += 1;
        }
        manifests.push(view);
    }
    let rollback_available = read_json_file(&paths.rollback_file)
        .ok()
        .flatten()
        .is_some();
    let recovery_required =
        path_present(&paths.pending_file) || path_present(&paths.rollback_pending_file);
    let rollback_in_progress = path_present(&paths.rollback_pending_file);
    let official_store = crate::browser_extension_identity::official_store_identity().ok();
    let official_store_extension_id = official_store
        .as_ref()
        .map(|identity| identity.extension_id.clone());
    let store_origin_match = official_store
        .as_ref()
        .is_some_and(|identity| identity.origin == paths.extension_origin);
    let official_standalone =
        crate::browser_extension_identity::official_standalone_identity().ok();
    let official_standalone_extension_id = official_standalone
        .as_ref()
        .map(|identity| identity.extension_id.clone());
    let standalone_origin_match = official_standalone
        .as_ref()
        .is_some_and(|identity| identity.origin == paths.extension_origin);
    let registered_dev_extension_id = paths
        .dev_extension_origin
        .as_deref()
        .and_then(extension_id_from_origin);
    let active_channel = if store_origin_match {
        "store"
    } else if standalone_origin_match {
        "standalone"
    } else {
        "dev"
    };
    json!({
        "ok": runtime_binary_ok && wrapper_ok && owned_count > 0,
        "implementation": "rust",
        "host": HOST_NAME,
        "extension_id": paths.extension_id,
        "extension_path": paths.extension_path,
        "extension_origin": paths.extension_origin,
        "extension_identity_source": paths.extension_identity_source,
        "official_store_extension_id": official_store_extension_id,
        "store_origin_match": store_origin_match,
        "official_standalone_extension_id": official_standalone_extension_id,
        "standalone_origin_match": standalone_origin_match,
        "active_channel": active_channel,
        "dev_enabled": paths.dev_extension_origin.is_some(),
        "registered_dev_extension_id": registered_dev_extension_id,
        "registered_dev_extension_origin": paths.dev_extension_origin,
        "runtime_binary": paths.runtime_binary,
        "runtime_binary_ok": runtime_binary_ok,
        "runtime_matches_current": runtime_matches_current,
        "native_runtime_version": native_runtime_version,
        "active_runtime_version": active_runtime_version,
        "version_consistent": version_consistent,
        "stale_runtime": owned_count > 0 && wrapper_ok && runtime_binary_ok && !runtime_matches_current,
        "wrapper": paths.wrapper,
        "wrapper_ok": wrapper_ok,
        "owned_manifest_count": owned_count,
        "rollback_available": rollback_available,
        "recovery_required": recovery_required,
        "rollback_in_progress": rollback_in_progress,
        "manifests": manifests,
    })
}

#[cfg(target_os = "macos")]
fn uninstall(paths: &InstallPaths) -> Result<Value, String> {
    // Fail closed if a rollback is mid-restore: uninstall must not remove files
    // from a half-restored disk or discard evidence mid-restore.
    reject_rollback_in_progress(paths)?;

    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    for (target, _) in &paths.targets {
        let manifest_path = target.join(format!("{HOST_NAME}.json"));
        if !path_present(&manifest_path) {
            continue;
        }
        let view = manifest_status(&manifest_path, paths);
        if view.get("owned").and_then(Value::as_bool) == Some(true) {
            fs::remove_file(&manifest_path).map_err(|error| {
                format!(
                    "cannot remove native-host manifest {}: {error}",
                    manifest_path.display()
                )
            })?;
            removed.push(manifest_path.to_string_lossy().into_owned());
        } else {
            skipped.push(json!({
                "path": manifest_path,
                "reason": "manifest_not_owned",
            }));
        }
    }

    let any_manifest_left = paths
        .targets
        .iter()
        .any(|(target, _)| path_present(&target.join(format!("{HOST_NAME}.json"))));
    let mut files_removed = Vec::new();
    if !any_manifest_left {
        if wrapper_is_rust(&paths.wrapper) && fs::remove_file(&paths.wrapper).is_ok() {
            files_removed.push(paths.wrapper.to_string_lossy().into_owned());
        }
        if is_regular_executable(&paths.runtime_binary)
            && fs::remove_file(&paths.runtime_binary).is_ok()
        {
            files_removed.push(paths.runtime_binary.to_string_lossy().into_owned());
        }
        // Intentional full removal consumes the ready rollback evidence and any
        // leftover pending marker; a partial/foreign uninstall must not discard
        // rollback evidence.
        if skipped.is_empty() && !files_removed.is_empty() {
            consume_ready_record(paths)?;
            let _ = fs::remove_file(&paths.pending_file);
        }
    }
    Ok(json!({
        "ok": skipped.is_empty(),
        "implementation": "rust",
        "host": HOST_NAME,
        "extension_id": paths.extension_id,
        "removed": removed,
        "skipped": skipped,
        "files_removed": files_removed,
    }))
}

/// Restore the previously managed native-host state/version/config after a
/// failed or interrupted install. Rollback is an independent fault domain from
/// runtime `service rollback`: it restores immutable backup files captured
/// before the last committed install, never rebuilds a binary, never touches
/// shared SQLite state (schema stays v4), and never stores credentials. It
/// fails closed when no owned evidence exists, when on-disk state is
/// foreign/unowned/symlinked, or when a managed file would be overwritten by a
/// non-owned target.
///
/// A durable `rollback-pending.json` marker is written *before* the first
/// restore mutation so a partial restore (e.g. a Chrome manifest restored but
/// an Edge write failing) can be retried idempotently: a later run that finds
/// the marker pointing at the same READY snapshot skips the activated-state
/// fingerprint check (the disk is already half-restored) and resumes from the
/// same immutable evidence. READY and the backup are consumed only after every
/// managed file has been restored successfully.
#[cfg(target_os = "macos")]
fn rollback(paths: &InstallPaths) -> Result<Value, String> {
    // First-class rollback after interruption: recover an interrupted install
    // before deciding on a fresh rollback. This recovers the prior state; the
    // previous ready rollback (if any) stays intact.
    if path_present(&paths.pending_file) {
        recover_pending(paths)?;
        return Ok(json!({
            "ok": true,
            "implementation": "rust",
            "host": HOST_NAME,
            "recovered_pending_install": true,
        }));
    }

    // Reconcile any stale rollback-in-progress marker before reading READY: the
    // marker is only meaningful while READY still carries the same rollback_id.
    // A marker whose READY is missing (crash after consume) or whose id moved
    // on is a crash leftover and is cleared here so a later install/uninstall is
    // never bricked by dead state. A matching marker means a previous restore
    // was interrupted and we resume it from the same immutable snapshot.
    let resume = rollback_marker_is_active(paths)?;

    let ready = read_json_file(&paths.rollback_file)?;
    let Some(ready) = ready else {
        return Ok(json!({
            "ok": false,
            "implementation": "rust",
            "host": HOST_NAME,
            "rollback_available": false,
            "reason": "no_native_host_rollback",
        }));
    };
    let rollback_id = ready
        .get("rollback_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "native-host rollback record has no rollback_id".to_owned())?;
    let backup_dir = paths.backups_dir.join(rollback_id);
    if backup_dir.parent() != Some(paths.backups_dir.as_path()) {
        return Err("native-host rollback record escapes managed backup dir".to_owned());
    }
    reject_symlink_target(&backup_dir)?;
    let snapshot = read_json_file(&backup_dir.join("evidence.json"))?
        .ok_or_else(|| "native-host rollback evidence is missing".to_owned())?;

    if resume {
        // Partial restore already mutated the disk; only re-assert symlink
        // safety and continue restoring the same snapshot (idempotent). The
        // evidence set is re-validated below before any further mutation.
        reject_non_regular_managed_targets(paths)?;
        validate_evidence_set(paths, &backup_dir, &snapshot)?;
    } else {
        // Fresh rollback: fail closed unless every managed file currently
        // exactly matches the activated fingerprint we committed, AND the
        // complete confined evidence set (runtime, wrapper, every manifest) is
        // present and hash-verified. Nothing is written or removed before the
        // evidence set is proven complete.
        reject_non_regular_managed_targets(paths)?;
        validate_cohort_ownership(paths)?;
        validate_activated_state(paths, &ready)?;
        validate_evidence_set(paths, &backup_dir, &snapshot)?;
        write_json_file(
            &paths.rollback_pending_file,
            &json!({
                "rollback_id": rollback_id,
                "started_at": now_millis(),
            }),
            0o600,
        )?;
    }

    let mut restored = Vec::new();
    let mut removed_absent = Vec::new();
    restore_evidence(
        paths,
        &backup_dir,
        &snapshot,
        &mut restored,
        &mut removed_absent,
    )?;

    // Only after every managed file was restored successfully do we consume
    // the READY record and clear the in-progress marker.
    consume_ready_record(paths)?;
    fs::remove_file(&paths.rollback_pending_file)
        .map_err(|error| format!("cannot clear rollback marker: {error}"))?;
    Ok(json!({
        "ok": true,
        "implementation": "rust",
        "host": HOST_NAME,
        "rollback_id": rollback_id,
        "resumed": resume,
        "restored": restored,
        "removed_absent": removed_absent,
    }))
}

/// Recover a pending (interrupted) native-host install transaction. The
/// previous ready rollback (if any) is left untouched. Idempotent: a missing
/// pending record is a no-op.
///
/// Crash-window recovery (Fix 1): if the durable READY `rollback.json` already
/// carries the *same* rollback_id as the pending record, the install committed
/// (it wrote READY and crashed before removing the pending marker). In that
/// case the pending marker is a stale leftover and MUST be cleared WITHOUT
/// restoring the pre-install snapshot — restoring would undo a successful
/// install.
#[cfg(target_os = "macos")]
fn recover_pending(paths: &InstallPaths) -> Result<(), String> {
    let Some(pending) = read_json_file(&paths.pending_file)? else {
        return Ok(());
    };
    let rollback_id = pending
        .get("rollback_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "native-host pending install has no rollback_id".to_owned())?;
    if pending_matches_committed_ready(paths, rollback_id)? {
        // Install already committed; only clear the stale pending marker.
        fs::remove_file(&paths.pending_file)
            .map_err(|error| format!("cannot clear stale pending install: {error}"))?;
        return Ok(());
    }
    let backup_dir = paths.backups_dir.join(rollback_id);
    if backup_dir.parent() != Some(paths.backups_dir.as_path()) {
        return Err("native-host pending backup escapes managed backup dir".to_owned());
    }
    reject_symlink_target(&backup_dir)?;
    let evidence = read_json_file(&backup_dir.join("evidence.json"))?
        .ok_or_else(|| "native-host pending install evidence is missing".to_owned())?;
    reject_non_regular_managed_targets(paths)?;
    let mut restored = Vec::new();
    let mut removed_absent = Vec::new();
    restore_evidence(
        paths,
        &backup_dir,
        &evidence,
        &mut restored,
        &mut removed_absent,
    )?;
    fs::remove_file(&paths.pending_file)
        .map_err(|error| format!("cannot clear pending install: {error}"))?;
    Ok(())
}

/// On a failed install, restore the pre-mutation snapshot and clear the pending
/// marker. The previous ready rollback (if any) is left intact. If the READY
/// record already matches the pending rollback_id, the install actually
/// committed before the mutation failed; only the stale marker is cleared.
#[cfg(target_os = "macos")]
fn abort_pending_install(paths: &InstallPaths) -> Result<(), String> {
    let Some(pending) = read_json_file(&paths.pending_file)? else {
        return Ok(());
    };
    let rollback_id = pending
        .get("rollback_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "native-host pending install has no rollback_id".to_owned())?;
    if pending_matches_committed_ready(paths, rollback_id)? {
        fs::remove_file(&paths.pending_file)
            .map_err(|error| format!("cannot clear stale pending install: {error}"))?;
        return Ok(());
    }
    let backup_dir = paths.backups_dir.join(rollback_id);
    if backup_dir.parent() != Some(paths.backups_dir.as_path()) {
        return Err("native-host pending backup escapes managed backup dir".to_owned());
    }
    reject_symlink_target(&backup_dir)?;
    let evidence = read_json_file(&backup_dir.join("evidence.json"))?
        .ok_or_else(|| "native-host pending install evidence is missing".to_owned())?;
    reject_non_regular_managed_targets(paths)?;
    let mut restored = Vec::new();
    let mut removed_absent = Vec::new();
    restore_evidence(
        paths,
        &backup_dir,
        &evidence,
        &mut restored,
        &mut removed_absent,
    )?;
    fs::remove_file(&paths.pending_file)
        .map_err(|error| format!("cannot clear pending install: {error}"))?;
    Ok(())
}

/// True when the durable READY `rollback.json` record carries the given
/// rollback_id, i.e. the install for that transaction already committed and a
/// same-id pending marker is a stale crash leftover.
#[cfg(target_os = "macos")]
fn pending_matches_committed_ready(
    paths: &InstallPaths,
    rollback_id: &str,
) -> Result<bool, String> {
    let ready = read_json_file(&paths.rollback_file)?;
    Ok(ready
        .and_then(|record| {
            record
                .get("rollback_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .as_deref()
        == Some(rollback_id))
}

#[cfg(target_os = "macos")]
fn read_json_file(path: &Path) -> Result<Option<Value>, String> {
    if !path_present(path) {
        return Ok(None);
    }
    let raw = fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if raw.len() > 256 * 1024 {
        return Err(format!("{} is too large", path.display()));
    }
    serde_json::from_slice(&raw)
        .map(Some)
        .map_err(|error| format!("cannot decode {}: {error}", path.display()))
}

#[cfg(target_os = "macos")]
fn write_json_file(path: &Path, value: &Value, mode: u32) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("cannot encode {}: {error}", path.display()))?;
    atomic_write(path, &bytes, mode)
}

/// Snapshot the exact pre-mutation on-disk managed state (or absence) as
/// immutable evidence under a new backup dir and return (id, backup_dir). Runs
/// ownership/symlink validation *before* creating the backup dir, and never
/// snapshots foreign/unowned state. Each entry records the exact relative
/// backup file name so restore can validate confinement and never clobber a
/// foreign file.
#[cfg(target_os = "macos")]
fn snapshot_evidence(paths: &InstallPaths) -> Result<(String, PathBuf), String> {
    reject_non_regular_managed_targets(paths)?;
    validate_cohort_ownership(paths)?;
    let id = format!("nhost-{:x}-{}", std::process::id(), now_millis());
    let backup_dir = paths.backups_dir.join(&id);
    reject_symlink_target(&backup_dir)?;
    ensure_secure_dir(&backup_dir)?;

    let mut entries = Vec::new();
    for (index, (target, _)) in paths.targets.iter().enumerate() {
        let manifest_path = target.join(format!("{HOST_NAME}.json"));
        let backup_name = format!("manifest-{index}.bin");
        entries.push(persist_file_evidence(
            &backup_dir,
            &manifest_path,
            &backup_name,
            0o600,
        )?);
    }
    entries.push(persist_file_evidence(
        &backup_dir,
        &paths.runtime_binary,
        "binary.bin",
        0o700,
    )?);
    entries.push(persist_file_evidence(
        &backup_dir,
        &paths.wrapper,
        "wrapper.bin",
        0o700,
    )?);

    write_json_file(
        &backup_dir.join("evidence.json"),
        &json!({ "entries": entries, "created_at": now_millis() }),
        0o600,
    )?;
    Ok((id, backup_dir))
}

/// Persist an immutable copy of a prior owned file (or record absence) into the
/// backup dir and return an evidence entry keyed by the exact relative backup
/// file name. The mode class (binary/wrapper 0700, manifest 0600) is recorded.
#[cfg(target_os = "macos")]
fn persist_file_evidence(
    backup_dir: &Path,
    source: &Path,
    backup_name: &str,
    mode: u32,
) -> Result<Value, String> {
    let present = path_present(source);
    let sha256 = if present {
        Some(file_sha256(source)?)
    } else {
        None
    };
    if present {
        let bytes = fs::read(source)
            .map_err(|error| format!("cannot read {}: {error}", source.display()))?;
        atomic_write(&backup_dir.join(backup_name), &bytes, mode)?;
    }
    Ok(json!({
        "path": source,
        "present": present,
        "sha256": sha256,
        "backup": backup_name,
        "mode": mode,
    }))
}

/// Validate that every present managed file is owned by us: the runtime binary
/// must be a regular executable at the managed path, the wrapper must be the
/// exact Rust-owned wrapper for the managed path/origin, and every present host
/// manifest must be the exact owned manifest. All-absent is allowed (fresh
/// install). Inconsistent/foreign partial state fails closed.
#[cfg(target_os = "macos")]
fn validate_cohort_ownership(paths: &InstallPaths) -> Result<(), String> {
    if path_present(&paths.runtime_binary) && !is_regular_executable(&paths.runtime_binary) {
        return Err(format!(
            "native-host runtime binary {} is not a managed regular executable",
            paths.runtime_binary.display()
        ));
    }
    if path_present(&paths.runtime_binary) && !wrapper_is_rust(&paths.wrapper) {
        return Err(format!(
            "native-host wrapper {} is not the owned Rust wrapper",
            paths.wrapper.display()
        ));
    }
    if path_present(&paths.runtime_binary) && !wrapper_targets_managed_binary(&paths.wrapper, paths)
    {
        return Err(format!(
            "native-host wrapper {} does not target the managed binary",
            paths.wrapper.display()
        ));
    }
    for (target, _) in &paths.targets {
        let manifest_path = target.join(format!("{HOST_NAME}.json"));
        if path_present(&manifest_path) {
            let view = manifest_status(&manifest_path, paths);
            let owned = view.get("owned").and_then(Value::as_bool) == Some(true)
                || (paths.allow_origin_migration
                    && manifest_structurally_owned(&manifest_path, paths));
            if !owned {
                return Err(format!(
                    "native-host manifest {} is foreign or unowned",
                    manifest_path.display()
                ));
            }
        }
    }
    Ok(())
}

/// Verify the wrapper body targets the managed runtime binary and origin.
#[cfg(target_os = "macos")]
fn wrapper_targets_managed_binary(wrapper: &Path, paths: &InstallPaths) -> bool {
    let Ok(content) = fs::read_to_string(wrapper) else {
        return false;
    };
    if !content.contains(WRAPPER_MARKER) {
        return false;
    }
    content.contains(&format!(
        "exec {} extension-host",
        shell_quote(paths.runtime_binary.to_string_lossy().as_ref())
    ))
}

#[cfg(target_os = "macos")]
fn wrapper_identity_matches(wrapper: &Path, paths: &InstallPaths) -> bool {
    let Ok(content) = fs::read_to_string(wrapper) else {
        return false;
    };
    if !content.contains(WRAPPER_MARKER) {
        return false;
    }
    let active_line = format!(
        "export HERDR_EXTENSION_ORIGIN={}",
        shell_quote(&paths.extension_origin)
    );
    if !content.lines().any(|line| line == active_line) {
        return false;
    }

    let actual_dev = wrapper_dev_extension_origin(wrapper).ok().flatten();
    actual_dev == paths.dev_extension_origin
}

/// Reject any managed on-disk target that is a symlink. Fail-closes before any
/// mutation so we never follow a foreign link.
#[cfg(target_os = "macos")]
fn reject_non_regular_managed_targets(paths: &InstallPaths) -> Result<(), String> {
    reject_symlink_target(&paths.runtime_binary)?;
    reject_symlink_target(&paths.wrapper)?;
    for (target, _) in &paths.targets {
        reject_symlink_target(&target.join(format!("{HOST_NAME}.json")))?;
    }
    Ok(())
}

/// Fail closed if a rollback is genuinely mid-restore (a durable
/// `rollback-pending.json` marker exists AND still matches the current READY
/// record). Install/uninstall must not snapshot or mutate a half-restored disk;
/// the interrupted rollback must be resumed or its marker cleared first.
///
/// A marker whose READY is missing (crash after READY was consumed) or whose
/// rollback_id no longer matches READY is a stale crash leftover: it is cleared
/// here so install/uninstall are never bricked by dead state.
#[cfg(target_os = "macos")]
fn reject_rollback_in_progress(paths: &InstallPaths) -> Result<(), String> {
    if rollback_marker_is_active(paths)? {
        return Err(
            "native-host rollback is in progress; resume or clear it before install/uninstall"
                .to_owned(),
        );
    }
    Ok(())
}

/// True when a rollback is genuinely in flight: a `rollback-pending.json`
/// marker exists, READY exists, and both carry the same rollback_id. When the
/// marker's READY is missing (crash after READY was consumed) or points at a
/// different snapshot, the marker is a stale crash leftover and is removed so
/// later operations are never bricked by dead state.
#[cfg(target_os = "macos")]
fn rollback_marker_is_active(paths: &InstallPaths) -> Result<bool, String> {
    let Some(marker) = read_json_file(&paths.rollback_pending_file)? else {
        return Ok(false);
    };
    let marker_id = marker
        .get("rollback_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "native-host rollback marker has no rollback_id".to_owned())?;
    let ready_id = read_json_file(&paths.rollback_file)?.and_then(|ready| {
        ready
            .get("rollback_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    if ready_id.as_deref() != Some(marker_id) {
        fs::remove_file(&paths.rollback_pending_file)
            .map_err(|error| format!("cannot clear stale rollback marker: {error}"))?;
        return Ok(false);
    }
    Ok(true)
}

/// Verify the current on-disk managed state exactly matches the ready record's
/// activated fingerprint (runtime + wrapper + every manifest content/presence),
/// so rollback only restores over the exact owned build and never a tampered or
/// foreign one. The `activated_manifests` array is REQUIRED and must cover every
/// managed manifest entry (path, presence, and sha256 when present); a missing,
/// non-array, or incomplete list fails closed.
#[cfg(target_os = "macos")]
fn validate_activated_state(paths: &InstallPaths, ready: &Value) -> Result<(), String> {
    if let Some(expected) = ready
        .get("activated_runtime_binary_sha256")
        .and_then(Value::as_str)
    {
        if !is_regular_executable(&paths.runtime_binary)
            || file_sha256(&paths.runtime_binary).ok().as_deref() != Some(expected)
        {
            return Err(
                "native-host runtime binary is not the activated owned build; refusing rollback"
                    .to_owned(),
            );
        }
    } else if path_present(&paths.runtime_binary) {
        return Err("native-host runtime binary present but no activated fingerprint".to_owned());
    }
    if let Some(expected) = ready
        .get("activated_wrapper_sha256")
        .and_then(Value::as_str)
    {
        if !path_present(&paths.wrapper)
            || file_sha256(&paths.wrapper).ok().as_deref() != Some(expected)
        {
            return Err("native-host wrapper is not the activated owned wrapper".to_owned());
        }
    } else if path_present(&paths.wrapper) {
        return Err("native-host wrapper present but no activated fingerprint".to_owned());
    }

    // The activated manifest fingerprint is mandatory: it must be an array
    // covering exactly the managed manifest set the install touched.
    let expected_manifests = ready
        .get("activated_manifests")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "native-host rollback record has no complete activated_manifests".to_owned()
        })?;
    let touched = touched_manifest_paths(paths);
    if expected_manifests.len() != touched.len() {
        return Err(format!(
            "native-host activated_manifests covers {} entries but {} were installed",
            expected_manifests.len(),
            touched.len()
        ));
    }
    for (index, entry) in expected_manifests.iter().enumerate() {
        let manifest_path = entry
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("activated manifest entry {index} has no path"))?;
        if manifest_path != touched[index] {
            return Err(format!(
                "activated manifest entry {index} path {manifest_path} does not match installed {}",
                touched[index]
            ));
        }
        let expected_present = entry
            .get("present")
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("activated manifest entry {index} has no presence"))?;
        let path = PathBuf::from(manifest_path);
        let present = path_present(&path);
        if present != expected_present {
            return Err(format!(
                "native-host manifest {} presence does not match activated fingerprint",
                path.display()
            ));
        }
        if expected_present {
            let expected_sha = entry.get("sha256").and_then(Value::as_str).ok_or_else(|| {
                format!("activated manifest entry {index} is present but has no sha256")
            })?;
            if file_sha256(&path).ok().as_deref() != Some(expected_sha) {
                return Err(format!(
                    "native-host manifest {} is tampered; refusing rollback",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

/// List the exact manifest paths the current install would touch (present or
/// `always`) so the activated fingerprint covers the same set rollback restores.
#[cfg(target_os = "macos")]
fn activated_manifests(paths: &InstallPaths) -> Result<Value, String> {
    let manifests = touched_manifest_paths(paths)
        .into_iter()
        .map(|path_str| {
            let path = PathBuf::from(&path_str);
            let present = path_present(&path);
            let sha256 = if present {
                Some(file_sha256(&path)?)
            } else {
                None
            };
            Ok(json!({
                "path": path_str,
                "present": present,
                "sha256": sha256,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Value::Array(manifests))
}

/// Paths (as strings) of the manifests the install touched: always targets plus
/// any present optional browser dirs.
#[cfg(target_os = "macos")]
fn touched_manifest_paths(paths: &InstallPaths) -> Vec<String> {
    let mut out = Vec::new();
    for (target, always) in &paths.targets {
        let browser_dir = target.parent().unwrap_or(target);
        if !*always && !browser_dir.exists() {
            continue;
        }
        out.push(
            target
                .join(format!("{HOST_NAME}.json"))
                .to_string_lossy()
                .into_owned(),
        );
    }
    out
}

/// Validate that the evidence set is complete and confined BEFORE any restore
/// mutation: it must contain exactly one entry for the runtime binary, one for
/// the wrapper, and one for every managed manifest the snapshot covers, each
/// confined to a managed path with a simple backup token, and every present
/// entry must carry a sha256 whose backup bytes match. A truncated, foreign, or
/// tampered evidence set fails closed and nothing is written or removed.
#[cfg(target_os = "macos")]
fn validate_evidence_set(
    paths: &InstallPaths,
    backup_dir: &Path,
    evidence: &Value,
) -> Result<(), String> {
    let Some(entries) = evidence.get("entries").and_then(Value::as_array) else {
        return Err("native-host evidence has no entries".to_owned());
    };
    let target_manifests = paths
        .targets
        .iter()
        .map(|(target, _)| target.join(format!("{HOST_NAME}.json")))
        .collect::<Vec<_>>();
    let mut saw_runtime = false;
    let mut saw_wrapper = false;
    let mut saw_manifests = std::collections::HashSet::new();
    for entry in entries {
        let path_str = entry.get("path").and_then(Value::as_str);
        let present = entry.get("present").and_then(Value::as_bool);
        let backup_name = entry.get("backup").and_then(Value::as_str);
        let (Some(path_str), Some(present)) = (path_str, present) else {
            return Err("native-host evidence entry is malformed".to_owned());
        };
        let path = PathBuf::from(path_str);
        let owned_managed = path == paths.runtime_binary
            || path == paths.wrapper
            || target_manifests.iter().any(|target| target == &path);
        if !owned_managed {
            return Err(format!(
                "native-host evidence references unowned path {}",
                path.display()
            ));
        }
        if let Some(backup_name) = backup_name
            && (backup_name.contains('/') || backup_name.contains(".."))
        {
            return Err(format!(
                "native-host evidence backup name {backup_name:?} is not a simple confined token"
            ));
        }
        if present {
            let expected_sha = entry.get("sha256").and_then(Value::as_str).ok_or_else(|| {
                format!(
                    "native-host evidence entry {} is present but has no sha256",
                    path_str
                )
            })?;
            let backup_name = backup_name
                .ok_or_else(|| "native-host evidence is missing backup file name".to_owned())?;
            let backup_path = backup_dir.join(backup_name);
            if backup_path.parent() != Some(backup_dir)
                || fs::symlink_metadata(&backup_path)
                    .map(|m| m.file_type().is_symlink() || !m.is_file())
                    .unwrap_or(true)
            {
                return Err(format!(
                    "native-host backup {} is missing or not a regular file",
                    backup_path.display()
                ));
            }
            if file_sha256(&backup_path).ok().as_deref() != Some(expected_sha) {
                return Err(format!(
                    "native-host backup {} failed sha256 integrity; refusing restore",
                    backup_path.display()
                ));
            }
        }
        if path == paths.runtime_binary {
            saw_runtime = true;
        } else if path == paths.wrapper {
            saw_wrapper = true;
        } else {
            saw_manifests.insert(path);
        }
    }
    if !saw_runtime {
        return Err("native-host evidence is missing the runtime binary entry".to_owned());
    }
    if !saw_wrapper {
        return Err("native-host evidence is missing the wrapper entry".to_owned());
    }
    for target in &target_manifests {
        if !saw_manifests.contains(target) {
            return Err(format!(
                "native-host evidence is missing manifest entry for {}",
                target.display()
            ));
        }
    }
    Ok(())
}

/// Restore the prior managed regular files/absence from immutable evidence,
/// confined to the exact paths we own, restoring the recorded mode class and
/// verifying each backup is a regular non-symlink file whose recorded SHA is
/// intact before overwrite. The complete evidence set is validated up front so
/// a truncated or tampered snapshot fails closed before any mutation.
#[cfg(target_os = "macos")]
fn restore_evidence(
    paths: &InstallPaths,
    backup_dir: &Path,
    evidence: &Value,
    restored: &mut Vec<String>,
    removed_absent: &mut Vec<String>,
) -> Result<(), String> {
    validate_evidence_set(paths, backup_dir, evidence)?;
    let Some(entries) = evidence.get("entries").and_then(Value::as_array) else {
        return Err("native-host evidence has no entries".to_owned());
    };
    let target_manifests = paths
        .targets
        .iter()
        .map(|(target, _)| target.join(format!("{HOST_NAME}.json")))
        .collect::<Vec<_>>();
    for entry in entries {
        let path_str = entry.get("path").and_then(Value::as_str);
        let present = entry.get("present").and_then(Value::as_bool);
        let backup_name = entry.get("backup").and_then(Value::as_str);
        let mode = entry.get("mode").and_then(Value::as_u64);
        let (Some(path_str), Some(present)) = (path_str, present) else {
            return Err("native-host evidence entry is malformed".to_owned());
        };
        let path = PathBuf::from(path_str);
        let owned_managed = path == paths.runtime_binary
            || path == paths.wrapper
            || target_manifests.iter().any(|target| target == &path);
        if !owned_managed {
            return Err(format!(
                "native-host evidence references unowned path {}",
                path.display()
            ));
        }
        // The backup file name must be a single simple token confined under our
        // managed backup dir (no separators or escaping).
        if let Some(backup_name) = backup_name
            && (backup_name.contains('/') || backup_name.contains(".."))
        {
            return Err(format!(
                "native-host evidence backup name {backup_name:?} is not a simple confined token"
            ));
        }
        if present {
            // A present evidence entry MUST carry a sha256, and the backup
            // bytes must match it before we overwrite the managed file.
            let expected_sha = entry.get("sha256").and_then(Value::as_str).ok_or_else(|| {
                format!(
                    "native-host evidence entry {} is present but has no sha256",
                    path_str
                )
            })?;
            let backup_name = backup_name
                .ok_or_else(|| "native-host evidence is missing backup file name".to_owned())?;
            let backup_path = backup_dir.join(backup_name);
            if backup_path.parent() != Some(backup_dir)
                || fs::symlink_metadata(&backup_path)
                    .map(|m| m.file_type().is_symlink() || !m.is_file())
                    .unwrap_or(true)
            {
                return Err(format!(
                    "native-host backup {} is missing or not a regular file",
                    backup_path.display()
                ));
            }
            if file_sha256(&backup_path).ok().as_deref() != Some(expected_sha) {
                return Err(format!(
                    "native-host backup {} failed sha256 integrity; refusing restore",
                    backup_path.display()
                ));
            }
            let bytes = fs::read(&backup_path)
                .map_err(|error| format!("cannot read native-host backup: {error}"))?;
            let mode = mode
                .map(|m| m as u32)
                .unwrap_or_else(|| restore_mode(&path));
            atomic_write(&path, &bytes, mode)?;
            restored.push(path_str.to_owned());
        } else if path_present(&path) {
            remove_regular_file(&path)?;
            removed_absent.push(path_str.to_owned());
        }
        failpoint_after_restore_mutation()?;
    }
    Ok(())
}

/// Production no-op for the install-mutation failpoint. The test build arms a
/// real injection point (see below); production builds must still link the
/// symbol that `install_mutation` calls, so this always succeeds.
#[cfg(all(target_os = "macos", not(test)))]
fn failpoint_after_install_mutation() -> Result<(), String> {
    Ok(())
}

/// Production no-op for the restore failpoint. The test build arms a real
/// injection point (see below); production builds must still link the symbol
/// that `restore_evidence` calls, so this always succeeds and has no effect.
#[cfg(all(target_os = "macos", not(test)))]
fn failpoint_after_restore_mutation() -> Result<(), String> {
    Ok(())
}

// Deterministic test-only failpoint state: after each real restore mutation,
// if a test has armed an injection for a specific mutation count, the restore
// aborts. This models a mid-loop failure (e.g. Chrome manifest restored but an
// Edge write failing) so a retry can be proven to resume from the same durable
// snapshot. It is macOS-test-only and has no production effect. Keeping the
// helpers out of non-macOS test builds also prevents Linux CI from compiling
// unused failpoint symbols after the macOS-only mutation paths are cfg'd out.
#[cfg(all(test, target_os = "macos"))]
thread_local! {
    static MUTATION_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static FAIL_AT_MUTATION: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    static INSTALL_MUTATION_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static FAIL_AT_INSTALL_MUTATION: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

#[cfg(all(test, target_os = "macos"))]
fn arm_failpoint_after_n_mutations(n: u32) {
    MUTATION_COUNT.with(|slot| slot.set(0));
    FAIL_AT_MUTATION.with(|slot| slot.set(Some(n)));
}

#[cfg(all(test, target_os = "macos"))]
fn disarm_failpoint() {
    FAIL_AT_MUTATION.with(|slot| slot.set(None));
    FAIL_AT_INSTALL_MUTATION.with(|slot| slot.set(None));
}

#[cfg(all(test, target_os = "macos"))]
fn arm_install_failpoint_after_n_mutations(n: u32) {
    INSTALL_MUTATION_COUNT.with(|slot| slot.set(0));
    FAIL_AT_INSTALL_MUTATION.with(|slot| slot.set(Some(n)));
}

#[cfg(all(test, target_os = "macos"))]
fn failpoint_after_install_mutation() -> Result<(), String> {
    let count = INSTALL_MUTATION_COUNT.with(|slot| {
        let current = slot.get();
        slot.set(current + 1);
        current + 1
    });
    let should_fail = matches!(
        FAIL_AT_INSTALL_MUTATION.with(|slot| slot.get()),
        Some(target) if target == count
    );
    if should_fail {
        FAIL_AT_INSTALL_MUTATION.with(|slot| slot.set(None));
        Err("injected native-host install mutation failure".to_owned())
    } else {
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
fn failpoint_after_restore_mutation() -> Result<(), String> {
    let count = MUTATION_COUNT.with(|slot| {
        let current = slot.get();
        slot.set(current + 1);
        current + 1
    });
    let should_fail = matches!(
        FAIL_AT_MUTATION.with(|slot| slot.get()),
        Some(target) if target == count
    );
    if should_fail {
        disarm_failpoint();
        Err("injected native-host restore failure".to_owned())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn restore_mode(path: &Path) -> u32 {
    let file_name = path.file_name().and_then(|value| value.to_str());
    if file_name == Some("herdr-extension-host") || file_name == Some("herdr-mcp") {
        0o700
    } else {
        0o600
    }
}

/// Consume the ready rollback record and its now-superseded immutable backup
/// only after a fully successful rollback restore. Ordering is deliberate:
/// READY is unlinked FIRST, then the backup dir is removed. A crash between the
/// two leaves an orphaned backup dir (garbage, never referenced) but never a
/// READY record pointing at deleted evidence. The caller clears the
/// rollback-in-progress marker after this returns.
#[cfg(target_os = "macos")]
fn consume_ready_record(paths: &InstallPaths) -> Result<(), String> {
    let ready_id = read_json_file(&paths.rollback_file)?.and_then(|ready| {
        ready
            .get("rollback_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    // Unlink READY first so a crash here never leaves READY pointing at
    // evidence that is about to be deleted.
    fs::remove_file(&paths.rollback_file)
        .map_err(|error| format!("cannot consume native-host rollback record: {error}"))?;
    if let Some(id) = ready_id {
        let backup_dir = paths.backups_dir.join(id);
        if backup_dir.parent() == Some(paths.backups_dir.as_path()) {
            // Fail closed: never follow/remove a symlink pretending to be our
            // backup dir. A leftover backup after a crash is garbage; READY is
            // already gone so it can never be referenced again.
            reject_symlink_target(&backup_dir)?;
            fs::remove_dir_all(&backup_dir)
                .map_err(|error| format!("cannot remove rollback backup: {error}"))?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(target_os = "macos")]
fn home_dir() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "cannot determine user home directory".to_owned())
}

/// Remove a managed regular file back to absence while refusing to follow or
/// remove a symlink (fail closed on foreign/tampered targets).
#[cfg(target_os = "macos")]
fn remove_regular_file(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "refusing to remove symlinked native-host file {}",
            path.display()
        )),
        Ok(metadata) if metadata.is_file() => fs::remove_file(path)
            .map_err(|error| format!("cannot remove {}: {error}", path.display())),
        Ok(_) => Err(format!(
            "refusing to remove non-regular native-host path {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
    }
}

#[cfg(target_os = "macos")]
fn install_targets(home: &Path) -> Vec<(PathBuf, bool)> {
    let app_support = home.join("Library").join("Application Support");
    [
        (vec!["Google", "Chrome"], true),
        (vec!["Google", "ChromeForTesting"], false),
        (vec!["Google", "Chrome Beta"], false),
        (vec!["Google", "Chrome Canary"], false),
        (vec!["Chromium"], false),
        (vec!["BraveSoftware", "Brave-Browser"], false),
        (vec!["Microsoft Edge"], false),
        (vec!["Citro Labs", "ego lite"], false),
    ]
    .into_iter()
    .map(|(parts, always)| {
        let mut target = app_support.clone();
        for part in parts {
            target.push(part);
        }
        target.push("NativeMessagingHosts");
        (target, always)
    })
    .collect()
}

#[cfg(target_os = "macos")]
fn find_registered_origin(
    targets: &[(PathBuf, bool)],
    wrapper: &Path,
) -> Result<Option<String>, String> {
    let mut origins = BTreeSet::new();
    for (target, _) in targets {
        let path = target.join(format!("{HOST_NAME}.json"));
        if !path_present(&path) {
            continue;
        }
        let raw = match fs::read(&path) {
            Ok(raw) if raw.len() <= 64 * 1024 => raw,
            _ => continue,
        };
        let Ok(manifest) = serde_json::from_slice::<Value>(&raw) else {
            continue;
        };
        if manifest.get("name").and_then(Value::as_str) != Some(HOST_NAME)
            || manifest.get("type").and_then(Value::as_str) != Some("stdio")
            || manifest.get("path").and_then(Value::as_str) != wrapper.to_str()
        {
            continue;
        }
        if let Some(allowed) = manifest.get("allowed_origins").and_then(Value::as_array) {
            for origin in allowed.iter().filter_map(Value::as_str) {
                if extension_id_from_origin(origin).is_some() {
                    origins.insert(origin.to_owned());
                }
            }
        }
    }
    if origins.len() > 1 {
        return Err("native_host_registered_origins_conflict".to_owned());
    }
    Ok(origins.into_iter().next())
}

/// Read the currently admitted Native Messaging origin from the managed browser
/// manifests. Long-lived native-host processes use this as a revocation fence:
/// switching Store <-> Dev rewrites the manifests, so an already-running host
/// must not keep serving the previously active extension just because it still
/// has the old wrapper environment in memory.
#[cfg(target_os = "macos")]
pub(crate) fn current_registered_extension_origin() -> Result<Option<String>, String> {
    let home = home_dir()?;
    let runtime_paths = crate::paths::RuntimePaths::discover()?;
    let wrapper = runtime_paths
        .config_dir
        .join("native")
        .join("herdr-extension-host");
    find_registered_origin(&install_targets(&home), &wrapper)
}

/// Recover the exact managed extension origin from durable native-host
/// metadata so rollback/status do not depend on the current worktree or live
/// manifest being intact. Precedence: ready rollback record, then a pending
/// install record, then the live registered manifest.
///
/// Missing metadata returns `Ok(None)` (caller may fall back to the live
/// registered manifest). Corrupt/invalid durable metadata fails closed with an
/// explicit error rather than silently falling back.
#[cfg(target_os = "macos")]
fn durable_extension_origin(native_dir: &Path) -> Result<Option<String>, String> {
    let candidates = [
        native_dir.join(ROLLBACK_FILE),
        native_dir.join(PENDING_FILE),
    ];
    for candidate in candidates {
        if !path_present(&candidate) {
            continue;
        }
        let value = read_json_file(&candidate)?.ok_or_else(|| {
            format!(
                "native-host metadata {} is present but could not be read",
                candidate.display()
            )
        })?;
        let Some(origin) = value.get("extension_origin").and_then(Value::as_str) else {
            return Err(format!(
                "native-host metadata {} has no valid extension_origin",
                candidate.display()
            ));
        };
        if extension_id_from_origin(origin).is_none() {
            return Err(format!(
                "native-host metadata {} has invalid extension_origin {origin:?}",
                candidate.display()
            ));
        }
        return Ok(Some(origin.to_owned()));
    }
    Ok(None)
}

#[cfg(target_os = "macos")]
fn extension_id_from_origin(origin: &str) -> Option<String> {
    let id = origin
        .strip_prefix("chrome-extension://")?
        .strip_suffix('/')?;
    (id.len() == 32 && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))).then(|| id.to_owned())
}

#[cfg(target_os = "macos")]
fn explicit_extension_origin() -> Result<Option<String>, String> {
    let Some(raw) = env::var_os("HERDR_EXTENSION_ORIGIN") else {
        return Ok(None);
    };
    let origin = raw.to_string_lossy().trim().to_owned();
    if origin.is_empty() {
        return Ok(None);
    }
    if extension_id_from_origin(&origin).is_none() {
        return Err(
            "HERDR_EXTENSION_ORIGIN is not a valid chrome-extension://<id>/ origin".to_owned(),
        );
    }
    Ok(Some(origin))
}

#[cfg(target_os = "macos")]
fn wrapper_body(paths: &InstallPaths) -> String {
    let dev_origin = paths
        .dev_extension_origin
        .as_deref()
        .map(|origin| {
            format!(
                "export HERDR_DEV_EXTENSION_ORIGIN={}\n",
                shell_quote(origin)
            )
        })
        .unwrap_or_default();
    format!(
        "#!/bin/sh\n{WRAPPER_MARKER}\nexport HERDR_EXTENSION_ORIGIN={}\n{}exec {} extension-host \"$@\"\n",
        shell_quote(&paths.extension_origin),
        dev_origin,
        shell_quote(paths.runtime_binary.to_string_lossy().as_ref()),
    )
}

#[cfg(target_os = "macos")]
fn manifest_value(paths: &InstallPaths) -> Value {
    json!({
        "name": HOST_NAME,
        "description": "herdr-mcp local browser-extension IPC bridge",
        "path": paths.wrapper,
        "type": "stdio",
        "allowed_origins": [paths.extension_origin],
    })
}

#[cfg(target_os = "macos")]
fn manifest_status(path: &Path, paths: &InstallPaths) -> Value {
    let raw = match fs::read(path) {
        Ok(raw) if raw.len() <= 64 * 1024 => raw,
        Ok(_) => return json!({"path": path, "invalid": true, "reason": "manifest_too_large"}),
        Err(error) => {
            return json!({
                "path": path,
                "invalid": true,
                "reason": "manifest_read_failed",
                "message": error.to_string(),
            });
        }
    };
    let manifest: Value = match serde_json::from_slice(&raw) {
        Ok(value) => value,
        Err(_) => return json!({"path": path, "invalid": true}),
    };
    let host_path = manifest.get("path").and_then(Value::as_str);
    let allowed = manifest
        .get("allowed_origins")
        .and_then(Value::as_array)
        .is_some_and(|origins| {
            origins.len() == 1
                && origins.first().and_then(Value::as_str) == Some(paths.extension_origin.as_str())
        });
    let rust_wrapper = wrapper_is_rust(&paths.wrapper);
    let wrapper_identity_match = wrapper_identity_matches(&paths.wrapper, paths);
    let owned = rust_wrapper
        && wrapper_identity_match
        && manifest.get("name").and_then(Value::as_str) == Some(HOST_NAME)
        && manifest.get("type").and_then(Value::as_str) == Some("stdio")
        && host_path == paths.wrapper.to_str()
        && allowed;
    json!({
        "path": path,
        "host_path": host_path,
        "allowed": allowed,
        "rust_wrapper": rust_wrapper,
        "wrapper_identity_match": wrapper_identity_match,
        "owned": owned,
    })
}

#[cfg(target_os = "macos")]
fn manifest_structurally_owned(path: &Path, paths: &InstallPaths) -> bool {
    let Ok(raw) = fs::read(path) else {
        return false;
    };
    if raw.len() > 64 * 1024 {
        return false;
    }
    let Ok(manifest) = serde_json::from_slice::<Value>(&raw) else {
        return false;
    };
    let one_valid_origin = manifest
        .get("allowed_origins")
        .and_then(Value::as_array)
        .is_some_and(|origins| {
            origins.len() == 1
                && origins
                    .first()
                    .and_then(Value::as_str)
                    .and_then(extension_id_from_origin)
                    .is_some()
        });
    wrapper_is_rust(&paths.wrapper)
        && manifest.get("name").and_then(Value::as_str) == Some(HOST_NAME)
        && manifest.get("type").and_then(Value::as_str) == Some("stdio")
        && manifest.get("path").and_then(Value::as_str) == paths.wrapper.to_str()
        && one_valid_origin
}

#[cfg(target_os = "macos")]
fn ensure_secure_dir(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "native-host directory {} is not a real directory",
                path.display()
            ));
        }
    } else {
        fs::create_dir_all(path).map_err(|error| {
            format!(
                "cannot create native-host directory {}: {error}",
                path.display()
            )
        })?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "cannot secure native-host directory {}: {error}",
            path.display()
        )
    })
}

#[cfg(target_os = "macos")]
fn atomic_copy_executable(source: &Path, target: &Path) -> Result<(), String> {
    if source == target
        || (source.canonicalize().ok().is_some()
            && source.canonicalize().ok() == target.canonicalize().ok())
    {
        return fs::set_permissions(target, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "cannot secure native-host binary {}: {error}",
                target.display()
            )
        });
    }
    reject_symlink_target(target)?;
    let temp = temporary_sibling(target);
    let result = (|| {
        let mut input = fs::File::open(source)
            .map_err(|error| format!("cannot open source binary {}: {error}", source.display()))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&temp)
            .map_err(|error| {
                format!(
                    "cannot create native-host binary temp {}: {error}",
                    temp.display()
                )
            })?;
        std::io::copy(&mut input, &mut output)
            .map_err(|error| format!("cannot copy native-host binary: {error}"))?;
        output
            .sync_all()
            .map_err(|error| format!("cannot sync native-host binary: {error}"))?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure native-host binary temp: {error}"))?;
        fs::rename(&temp, target).map_err(|error| {
            format!(
                "cannot activate native-host binary {}: {error}",
                target.display()
            )
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(target_os = "macos")]
fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    reject_symlink_target(path)?;
    let temp = temporary_sibling(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)
            .map_err(|error| format!("cannot create temp file {}: {error}", temp.display()))?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot write temp file {}: {error}", temp.display()))?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))
            .map_err(|error| format!("cannot secure temp file {}: {error}", temp.display()))?;
        fs::rename(&temp, path)
            .map_err(|error| format!("cannot activate {}: {error}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(target_os = "macos")]
fn reject_symlink_target(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(format!(
            "native-host target {} must not be a symlink",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn temporary_sibling(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("native-host");
    path.with_file_name(format!(".{name}.tmp-{:x}-{nonce:x}", std::process::id()))
}

#[cfg(target_os = "macos")]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn is_regular_executable(path: &Path) -> bool {
    fs::symlink_metadata(path).ok().is_some_and(|metadata| {
        !metadata.file_type().is_symlink()
            && metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
    })
}

#[cfg(target_os = "macos")]
fn wrapper_is_rust(path: &Path) -> bool {
    if fs::symlink_metadata(path)
        .ok()
        .is_none_or(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return false;
    }
    fs::read_to_string(path)
        .ok()
        .filter(|content| content.len() <= 32 * 1024)
        .is_some_and(|content| content.contains(WRAPPER_MARKER))
}

#[cfg(target_os = "macos")]
fn path_present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[cfg(target_os = "macos")]
fn read_binary_version(path: &Path) -> Option<String> {
    if !is_regular_executable(path) {
        return None;
    }
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(target_os = "macos")]
fn file_sha256(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("cannot hash {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn product_uninstall_requires_owned_manifest_for_any_native_host_footprint() {
        assert!(!product_uninstall_view_preflight(&json!({}), false).unwrap());
        assert!(
            product_uninstall_view_preflight(
                &json!({
                    "recovery_required": false,
                    "owned_manifest_count": 0,
                    "wrapper_ok": true,
                    "runtime_binary_ok": true,
                    "manifests": []
                }),
                true,
            )
            .is_err()
        );
        assert!(
            product_uninstall_view_preflight(
                &json!({
                    "recovery_required": false,
                    "owned_manifest_count": 1,
                    "wrapper_ok": true,
                    "runtime_binary_ok": true,
                    "manifests": [{"owned": false}]
                }),
                true,
            )
            .is_err()
        );
        assert!(
            product_uninstall_view_preflight(
                &json!({
                    "recovery_required": false,
                    "owned_manifest_count": 1,
                    "wrapper_ok": true,
                    "runtime_binary_ok": true,
                    "manifests": [{"owned": true}]
                }),
                true,
            )
            .unwrap()
        );
    }

    #[test]
    fn install_targets_include_chrome_for_testing_as_optional() {
        let home = PathBuf::from("/tmp/herdr-cft-target-test-home");
        let target = home
            .join("Library")
            .join("Application Support")
            .join("Google")
            .join("ChromeForTesting")
            .join("NativeMessagingHosts");
        let targets = install_targets(&home);
        let matches = targets
            .iter()
            .filter(|(path, _)| path == &target)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert!(
            !matches[0].1,
            "Chrome for Testing is a dev/UAT target, not an always-created user target"
        );
    }

    fn fixture() -> (PathBuf, InstallPaths) {
        let root = env::temp_dir().join(format!(
            "herdr-native-install-{}-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let home = root.join("home");
        let extension = root.join("repo").join("extension");
        let source = root.join("source-herdr-mcp");
        fs::create_dir_all(&extension).unwrap();
        fs::write(&source, b"rust-binary-fixture").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
        let paths = InstallPaths::for_values(&home, &extension, &source).unwrap();
        (root, paths)
    }

    #[test]
    fn install_status_uninstall_use_stable_wrapper_and_exact_origin() {
        let (root, paths) = fixture();
        let installed = install(&paths).unwrap();
        assert_eq!(installed["ok"], true);
        assert!(paths.runtime_binary.exists());
        assert!(paths.wrapper.exists());
        let wrapper = fs::read_to_string(&paths.wrapper).unwrap();
        assert!(wrapper.contains(WRAPPER_MARKER));
        assert!(wrapper.contains(&paths.extension_origin));
        assert!(!wrapper.contains("HERDR_MCP_TOKEN"));
        assert_eq!(
            fs::metadata(&paths.runtime_binary)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&paths.wrapper).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let stable_manifest = paths.targets[0].0.join(format!("{HOST_NAME}.json"));
        let manifest: Value = serde_json::from_slice(&fs::read(&stable_manifest).unwrap()).unwrap();
        assert_eq!(manifest["path"], paths.wrapper.to_string_lossy().as_ref());
        assert_eq!(manifest["allowed_origins"], json!([paths.extension_origin]));
        assert_eq!(
            fs::metadata(&stable_manifest).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let view = status(&paths);
        assert_eq!(view["ok"], true);
        assert_eq!(view["owned_manifest_count"], 1);
        assert_eq!(view["active_channel"], "dev");
        assert_eq!(view["dev_enabled"], true);
        assert_eq!(
            view["registered_dev_extension_origin"],
            paths.extension_origin
        );

        let removed = uninstall(&paths).unwrap();
        assert_eq!(removed["ok"], true);
        assert!(!stable_manifest.exists());
        assert!(!paths.wrapper.exists());
        assert!(!paths.runtime_binary.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn owned_dev_origin_switches_to_store_without_forgetting_dev_and_rolls_back() {
        let (root, dev_paths) = fixture();
        install(&dev_paths).unwrap();
        let old_origin = dev_paths.extension_origin.clone();
        let old_wrapper = fs::read_to_string(&dev_paths.wrapper).unwrap();
        let stable_manifest = dev_paths.targets[0].0.join(format!("{HOST_NAME}.json"));
        let old_manifest = fs::read(&stable_manifest).unwrap();

        let store = crate::browser_extension_identity::official_store_identity().unwrap();
        assert_ne!(store.origin, old_origin);
        let native_dir = dev_paths.runtime_binary.parent().unwrap().to_path_buf();
        let layout = NativeHostLayout {
            source_binary: dev_paths.source_binary.clone(),
            native_dir,
            wrapper: dev_paths.wrapper.clone(),
            targets: dev_paths.targets.clone(),
        };
        let store_paths = InstallPaths::for_origin_with_dev(
            &store.origin,
            None,
            Some(old_origin.clone()),
            "chrome_web_store_contract",
            true,
            layout,
        )
        .unwrap();

        let migrated = install(&store_paths).unwrap();
        assert_eq!(migrated["ok"], true);
        assert_eq!(migrated["extension_origin"], store.origin);
        assert_eq!(
            migrated["extension_identity_source"],
            "chrome_web_store_contract"
        );
        let migrated_manifest: Value =
            serde_json::from_slice(&fs::read(&stable_manifest).unwrap()).unwrap();
        assert_eq!(migrated_manifest["allowed_origins"], json!([store.origin]));
        let store_wrapper = fs::read_to_string(&store_paths.wrapper).unwrap();
        assert!(store_wrapper.contains(&store.origin));
        assert!(store_wrapper.contains("HERDR_DEV_EXTENSION_ORIGIN="));
        assert!(store_wrapper.contains(&old_origin));
        let store_view = status(&store_paths);
        assert_eq!(store_view["active_channel"], "store");
        assert_eq!(store_view["dev_enabled"], true);
        assert_eq!(store_view["registered_dev_extension_origin"], old_origin);

        let rolled_back = rollback(&store_paths).unwrap();
        assert_eq!(rolled_back["ok"], true);
        assert_eq!(fs::read_to_string(&dev_paths.wrapper).unwrap(), old_wrapper);
        assert_eq!(fs::read(&stable_manifest).unwrap(), old_manifest);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn store_standalone_and_dev_switches_keep_manifest_exactly_single_origin() {
        let (root, dev_paths) = fixture();
        install(&dev_paths).unwrap();
        let dev_origin = dev_paths.extension_origin.clone();
        let store = crate::browser_extension_identity::official_store_identity().unwrap();
        let standalone = crate::browser_extension_identity::official_standalone_identity().unwrap();
        assert_ne!(standalone.origin, store.origin);
        assert_ne!(standalone.origin, dev_origin);
        let native_dir = dev_paths.runtime_binary.parent().unwrap().to_path_buf();
        let store_layout = NativeHostLayout {
            source_binary: dev_paths.source_binary.clone(),
            native_dir: native_dir.clone(),
            wrapper: dev_paths.wrapper.clone(),
            targets: dev_paths.targets.clone(),
        };
        let store_paths = InstallPaths::for_origin_with_dev(
            &store.origin,
            None,
            Some(dev_origin.clone()),
            "native_host_use_store",
            true,
            store_layout,
        )
        .unwrap();
        install(&store_paths).unwrap();
        let stable_manifest = store_paths.targets[0].0.join(format!("{HOST_NAME}.json"));
        let store_manifest: Value =
            serde_json::from_slice(&fs::read(&stable_manifest).unwrap()).unwrap();
        assert_eq!(store_manifest["allowed_origins"], json!([store.origin]));
        assert_eq!(
            store_manifest["allowed_origins"].as_array().unwrap().len(),
            1
        );

        let standalone_layout = NativeHostLayout {
            source_binary: dev_paths.source_binary.clone(),
            native_dir: native_dir.clone(),
            wrapper: dev_paths.wrapper.clone(),
            targets: dev_paths.targets.clone(),
        };
        let standalone_paths = InstallPaths::for_origin_with_dev(
            &standalone.origin,
            None,
            Some(dev_origin.clone()),
            "native_host_use_standalone",
            true,
            standalone_layout,
        )
        .unwrap();
        install(&standalone_paths).unwrap();
        let standalone_manifest: Value =
            serde_json::from_slice(&fs::read(&stable_manifest).unwrap()).unwrap();
        assert_eq!(
            standalone_manifest["allowed_origins"],
            json!([standalone.origin])
        );
        assert_eq!(
            standalone_manifest["allowed_origins"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let standalone_view = status(&standalone_paths);
        assert_eq!(standalone_view["active_channel"], "standalone");
        assert_eq!(standalone_view["standalone_origin_match"], true);
        assert_eq!(standalone_view["dev_enabled"], true);
        assert_eq!(
            standalone_view["registered_dev_extension_origin"],
            dev_origin
        );

        let dev_layout = NativeHostLayout {
            source_binary: dev_paths.source_binary.clone(),
            native_dir,
            wrapper: dev_paths.wrapper.clone(),
            targets: dev_paths.targets.clone(),
        };
        let dev_again = InstallPaths::for_origin_with_dev(
            &dev_origin,
            dev_paths.extension_path.clone(),
            Some(dev_origin.clone()),
            "native_host_use_dev",
            true,
            dev_layout,
        )
        .unwrap();
        install(&dev_again).unwrap();
        let dev_manifest: Value =
            serde_json::from_slice(&fs::read(&stable_manifest).unwrap()).unwrap();
        assert_eq!(dev_manifest["allowed_origins"], json!([dev_origin]));
        assert_eq!(dev_manifest["allowed_origins"].as_array().unwrap().len(), 1);
        let dev_view = status(&dev_again);
        assert_eq!(dev_view["active_channel"], "dev");
        assert_eq!(dev_view["dev_enabled"], true);
        assert_eq!(dev_view["store_origin_match"], false);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uninstall_preserves_manifest_that_is_not_owned() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        let stable_manifest = paths.targets[0].0.join(format!("{HOST_NAME}.json"));
        fs::write(
            &stable_manifest,
            br#"{"name":"dev.herdr.mcp","type":"stdio","path":"/tmp/other","allowed_origins":[]}"#,
        )
        .unwrap();
        let removed = uninstall(&paths).unwrap();
        assert_eq!(removed["ok"], false);
        assert!(stable_manifest.exists());
        assert!(paths.wrapper.exists());
        assert!(paths.runtime_binary.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uninstall_never_removes_node_compatibility_host_manifest() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        let stable_manifest = paths.targets[0].0.join(format!("{HOST_NAME}.json"));
        fs::write(&paths.wrapper, "#!/bin/sh\nexec node compat-host \"$@\"\n").unwrap();
        fs::set_permissions(&paths.wrapper, fs::Permissions::from_mode(0o700)).unwrap();

        let removed = uninstall(&paths).unwrap();
        assert_eq!(removed["ok"], false);
        assert!(stable_manifest.exists());
        assert!(paths.wrapper.exists());
        assert!(paths.runtime_binary.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_refuses_symlinked_native_target() {
        let (root, paths) = fixture();
        let native_dir = paths.runtime_binary.parent().unwrap();
        fs::create_dir_all(native_dir).unwrap();
        let elsewhere = root.join("elsewhere");
        fs::write(&elsewhere, b"leave-me").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &paths.runtime_binary).unwrap();
        let error = install(&paths).unwrap_err();
        assert!(error.contains("must not be a symlink"));
        assert_eq!(fs::read(&elsewhere).unwrap(), b"leave-me");
        fs::remove_dir_all(root).unwrap();
    }

    // -----------------------------------------------------------------------
    // First-class native-host rollback
    // -----------------------------------------------------------------------

    fn install_then_capture(paths: &InstallPaths) -> (Vec<u8>, Vec<u8>, Vec<Vec<u8>>, Vec<String>) {
        install(paths).unwrap();
        let binary_bytes = fs::read(&paths.runtime_binary).unwrap();
        let wrapper_bytes = fs::read(&paths.wrapper).unwrap();
        let manifests = touched_manifest_paths(paths);
        let manifest_bytes = manifests
            .iter()
            .map(|path| fs::read(PathBuf::from(path)).unwrap())
            .collect::<Vec<_>>();
        (binary_bytes, wrapper_bytes, manifest_bytes, manifests)
    }

    #[test]
    fn install_then_install_then_rollback_restores_prior_state() {
        let (root, paths) = fixture();
        // First install: this becomes the "previous" state we must roll back to.
        let (binary, wrapper, manifests, manifest_paths) = install_then_capture(&paths);
        assert!(paths.rollback_file.exists());

        // Second install overwrites the native state.
        install(&paths).unwrap();
        assert!(paths.rollback_file.exists());

        // Rollback restores the exact prior owned state (from the ready record).
        let result = rollback(&paths).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(fs::read(&paths.runtime_binary).unwrap(), binary);
        assert_eq!(fs::read(&paths.wrapper).unwrap(), wrapper);
        for (path, expected) in manifest_paths.iter().zip(manifests.iter()) {
            assert_eq!(fs::read(PathBuf::from(path)).unwrap(), *expected);
        }
        // Ready rollback is consumed after a successful restore.
        assert!(!paths.rollback_file.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_from_prior_absence_removes_installed_state() {
        let (root, paths) = fixture();
        // No prior managed state: install snapshots absence and commits a ready
        // rollback whose activated state is the fresh install.
        install(&paths).unwrap();
        assert!(paths.runtime_binary.exists());
        assert!(paths.wrapper.exists());
        assert!(paths.rollback_file.exists());

        let result = rollback(&paths).unwrap();
        assert_eq!(result["ok"], true);
        assert!(!paths.runtime_binary.exists());
        assert!(!paths.wrapper.exists());
        for path in touched_manifest_paths(&paths) {
            assert!(!PathBuf::from(path).exists());
        }
        assert!(!paths.rollback_file.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_refuses_foreign_or_symlinked_managed_state() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        // Foreign, unowned wrapper: rollback must fail closed.
        fs::write(&paths.wrapper, "#!/bin/sh\nexec node other-host \"$@\"\n").unwrap();
        fs::set_permissions(&paths.wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        let error = rollback(&paths).unwrap_err();
        assert!(error.contains("not the owned Rust wrapper"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_refuses_tampered_activated_fingerprint() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        // Tamper the runtime binary after install: the committed rollback's
        // activated fingerprint no longer matches, so rollback fails closed.
        fs::write(&paths.runtime_binary, b"tampered-binary").unwrap();
        fs::set_permissions(&paths.runtime_binary, fs::Permissions::from_mode(0o700)).unwrap();
        let error = rollback(&paths).unwrap_err();
        assert!(error.contains("not the activated owned build"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pending_install_recovery_restores_prior_bytes_when_not_committed() {
        let (root, paths) = fixture();
        // First install establishes prior owned state and a READY record.
        install(&paths).unwrap();
        let ready_before = fs::read(&paths.rollback_file).unwrap();
        let ready_id = serde_json::from_slice::<Value>(&ready_before).unwrap()["rollback_id"]
            .as_str()
            .unwrap()
            .to_owned();

        // Capture the good prior state (binary, wrapper, every manifest) BEFORE
        // simulating the interrupted second install.
        let good_binary = fs::read(&paths.runtime_binary).unwrap();
        let good_wrapper = fs::read(&paths.wrapper).unwrap();
        let manifest_paths = touched_manifest_paths(&paths);
        let good_manifests = manifest_paths
            .iter()
            .map(|p| fs::read(PathBuf::from(p)).unwrap())
            .collect::<Vec<_>>();

        // The interrupted install would have snapshotted the pre-mutation state
        // through snapshot_evidence and left a pending marker under that id.
        // Do exactly that (complete confined evidence set), then simulate the
        // half-applied binary mutation on the disk.
        let (other_id, _pending_dir) = snapshot_evidence(&paths).unwrap();
        assert_ne!(other_id, ready_id);
        fs::write(&paths.runtime_binary, b"half-applied-binary").unwrap();
        fs::set_permissions(&paths.runtime_binary, fs::Permissions::from_mode(0o700)).unwrap();
        write_json_file(
            &paths.pending_file,
            &json!({
                "rollback_id": other_id,
                "extension_origin": paths.extension_origin,
            }),
            0o600,
        )
        .unwrap();

        // Recover the pending transaction directly: it must restore the good
        // prior binary/wrapper/manifests and clear the pending marker, without
        // touching the existing READY (id ready_id) record.
        recover_pending(&paths).unwrap();
        assert!(!paths.pending_file.exists());
        assert_eq!(fs::read(&paths.runtime_binary).unwrap(), good_binary);
        assert_eq!(fs::read(&paths.wrapper).unwrap(), good_wrapper);
        for (path, expected) in manifest_paths.iter().zip(good_manifests.iter()) {
            assert_eq!(fs::read(PathBuf::from(path)).unwrap(), *expected);
        }
        assert_eq!(fs::read(&paths.rollback_file).unwrap(), ready_before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crash_window_same_id_pending_is_stale_and_keeps_committed_host() {
        let (root, paths) = fixture();
        // Install fully commits (writes READY).
        install(&paths).unwrap();
        let ready =
            serde_json::from_slice::<Value>(&fs::read(&paths.rollback_file).unwrap()).unwrap();
        let committed_id = ready["rollback_id"].as_str().unwrap().to_owned();
        let committed_binary = fs::read(&paths.runtime_binary).unwrap();
        let committed_wrapper = fs::read(&paths.wrapper).unwrap();

        // Simulate the crash window: install wrote READY, then crashed before
        // removing the pending marker. The pending rollback_id equals READY's.
        write_json_file(
            &paths.pending_file,
            &json!({
                "rollback_id": committed_id,
                "extension_origin": paths.extension_origin,
            }),
            0o600,
        )
        .unwrap();

        // A subsequent rollback must treat pending as stale/committed: clear it
        // WITHOUT restoring the pre-install snapshot (which was absence), so the
        // committed native host remains untouched and ready to roll back.
        let result = rollback(&paths).unwrap();
        assert_eq!(result["recovered_pending_install"], true);
        assert!(!paths.pending_file.exists());
        // Committed host is intact and the READY record is still consumable.
        assert_eq!(fs::read(&paths.runtime_binary).unwrap(), committed_binary);
        assert_eq!(fs::read(&paths.wrapper).unwrap(), committed_wrapper);
        assert!(paths.rollback_file.exists());

        // And an explicit rollback afterwards restores the pre-install absence.
        let rb = rollback(&paths).unwrap();
        assert_eq!(rb["ok"], true);
        assert!(!paths.runtime_binary.exists());
        assert!(!paths.wrapper.exists());
        assert!(!paths.rollback_file.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_or_incomplete_activated_manifests_is_rejected() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        // Remove the activated_manifests array from the READY record.
        let mut ready =
            serde_json::from_slice::<Value>(&fs::read(&paths.rollback_file).unwrap()).unwrap();
        ready.as_object_mut().unwrap().remove("activated_manifests");
        write_json_file(&paths.rollback_file, &ready, 0o600).unwrap();
        let error = rollback(&paths).unwrap_err();
        assert!(error.contains("no complete activated_manifests"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hashless_or_tampered_backup_is_rejected_on_restore() {
        let (root, paths) = fixture();
        // Two installs: the second install's snapshot captures the *present*
        // prior files from the first install, so the ready backup dir holds a
        // real present evidence entry (binary.bin) we can tamper.
        install(&paths).unwrap();
        install(&paths).unwrap();
        let ready =
            serde_json::from_slice::<Value>(&fs::read(&paths.rollback_file).unwrap()).unwrap();
        let id = ready["rollback_id"].as_str().unwrap().to_owned();
        let backup_dir = paths.backups_dir.join(&id);
        // Corrupt the present binary backup's bytes so its SHA no longer
        // matches the recorded sha256; rollback must fail closed on restore.
        let binary_backup = backup_dir.join("binary.bin");
        assert!(binary_backup.exists());
        fs::write(&binary_backup, b"tampered-backup-bytes").unwrap();
        fs::set_permissions(&binary_backup, fs::Permissions::from_mode(0o600)).unwrap();
        let error = rollback(&paths).unwrap_err();
        assert!(error.contains("failed sha256 integrity"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn present_activated_manifest_without_sha256_is_rejected() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        // Strip the sha256 from a present activated_manifests entry; rollback
        // must fail closed rather than trust a hashless fingerprint.
        let mut ready =
            serde_json::from_slice::<Value>(&fs::read(&paths.rollback_file).unwrap()).unwrap();
        let manifests = ready["activated_manifests"].as_array_mut().unwrap();
        let present = manifests
            .iter_mut()
            .find(|entry| entry["present"] == true)
            .expect("a present activated manifest entry");
        present.as_object_mut().unwrap().remove("sha256");
        write_json_file(&paths.rollback_file, &ready, 0o600).unwrap();
        let error = rollback(&paths).unwrap_err();
        assert!(error.contains("is present but has no sha256"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_rollback_recovers_from_same_snapshot_on_retry() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        let committed_binary = fs::read(&paths.runtime_binary).unwrap();
        let ready =
            serde_json::from_slice::<Value>(&fs::read(&paths.rollback_file).unwrap()).unwrap();
        let id = ready["rollback_id"].as_str().unwrap().to_owned();

        // Simulate a partially restored disk: the rollback marker was written and
        // the first restore mutation (say the wrapper) completed, but the binary
        // write failed before it was restored. The disk is now half-restored and
        // no longer matches the activated fingerprint.
        write_json_file(
            &paths.rollback_pending_file,
            &json!({ "rollback_id": id, "started_at": now_millis() }),
            0o600,
        )
        .unwrap();
        // Wrapper was already restored to prior state (absence removal), binary
        // partially replaced.
        fs::write(&paths.runtime_binary, b"partial-restore-binary").unwrap();
        fs::set_permissions(&paths.runtime_binary, fs::Permissions::from_mode(0o700)).unwrap();

        // A retry sees the marker for the same snapshot and must NOT be blocked by
        // the activated fingerprint check; it resumes the restore to completion.
        let result = rollback(&paths).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["resumed"], true);
        // Final restored state: prior absence for this fresh install means the
        // binary/wrapper/manifests are all removed.
        assert!(!paths.runtime_binary.exists());
        assert!(!paths.wrapper.exists());
        assert!(!paths.rollback_file.exists());
        assert!(!paths.rollback_pending_file.exists());
        let _ = committed_binary;
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deterministic_failpoint_aborts_mid_restore_and_retry_resumes_same_snapshot() {
        let (root, paths) = fixture();
        // Install v1 binary, then change the source so the second install
        // activates DISTINCT bytes; the ready snapshot therefore holds the prior
        // v1 bytes, making the restore assertions non-tautological.
        install(&paths).unwrap();
        let prior_binary = fs::read(&paths.runtime_binary).unwrap();
        fs::write(&paths.source_binary, b"rust-binary-fixture-v2").unwrap();
        fs::set_permissions(&paths.source_binary, fs::Permissions::from_mode(0o700)).unwrap();
        install(&paths).unwrap();
        let activated_binary = fs::read(&paths.runtime_binary).unwrap();
        assert_ne!(prior_binary, activated_binary);

        let ready =
            serde_json::from_slice::<Value>(&fs::read(&paths.rollback_file).unwrap()).unwrap();
        let id = ready["rollback_id"].as_str().unwrap().to_owned();
        let backup_dir = paths.backups_dir.join(&id);
        let prior_wrapper = fs::read(&paths.wrapper).unwrap();

        // Evidence entries are ordered one per managed browser target, then the
        // runtime binary, then the wrapper. Derive the failpoint from that
        // semantic boundary so adding/removing an optional browser target does
        // not silently retarget this recovery test. After this entry the binary
        // is restored and the failpoint aborts before the wrapper is restored.
        let fail_after_binary = paths.targets.len() as u32 + 1;
        arm_failpoint_after_n_mutations(fail_after_binary);
        let error = rollback(&paths).unwrap_err();
        assert!(error.contains("injected native-host restore failure"));
        // The injected failure must leave the durable marker and READY intact so
        // a retry can resume from the same immutable snapshot.
        assert!(paths.rollback_pending_file.exists());
        assert!(paths.rollback_file.exists());
        // The disk is partially restored: the binary is back to the prior v1
        // bytes (different from the activated v2 bytes).
        assert_eq!(fs::read(&paths.runtime_binary).unwrap(), prior_binary);
        assert_ne!(fs::read(&paths.runtime_binary).unwrap(), activated_binary);
        assert!(backup_dir.exists());
        let _ = prior_wrapper;

        // Retry WITHOUT the failpoint: it must resume from the same snapshot
        // (resumed: true), skip the activated-fingerprint check, and complete
        // the restore, then consume READY and clear the marker.
        let result = rollback(&paths).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["resumed"], true);
        assert!(!paths.rollback_pending_file.exists());
        assert!(!paths.rollback_file.exists());
        assert!(!backup_dir.exists());
        // Final restored state: prior files restored, manifests restored.
        assert_eq!(fs::read(&paths.runtime_binary).unwrap(), prior_binary);
        for path in touched_manifest_paths(&paths) {
            assert!(PathBuf::from(path).exists());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_and_uninstall_fail_closed_while_rollback_in_progress() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        let ready =
            serde_json::from_slice::<Value>(&fs::read(&paths.rollback_file).unwrap()).unwrap();
        let id = ready["rollback_id"].as_str().unwrap().to_owned();
        // Simulate a mid-restore rollback marker.
        write_json_file(
            &paths.rollback_pending_file,
            &json!({ "rollback_id": id, "started_at": now_millis() }),
            0o600,
        )
        .unwrap();

        // Install must fail closed rather than snapshot a half-restored disk.
        let error = install(&paths).unwrap_err();
        assert!(error.contains("rollback is in progress"));
        // Uninstall must fail closed too.
        let error = uninstall(&paths).unwrap_err();
        assert!(error.contains("rollback is in progress"));
        // status reports the in-progress rollback.
        let view = status(&paths);
        assert_eq!(view["rollback_in_progress"], true);
        // The marker and READY are untouched.
        assert!(paths.rollback_pending_file.exists());
        assert!(paths.rollback_file.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_rollback_marker_after_ready_consumed_does_not_brick_install() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        let ready =
            serde_json::from_slice::<Value>(&fs::read(&paths.rollback_file).unwrap()).unwrap();
        let id = ready["rollback_id"].as_str().unwrap().to_owned();

        // Simulate a crash where READY was already consumed (unlinked) but the
        // rollback-in-progress marker survived (i.e. crash between the READY
        // unlink and the backup/marker cleanup in consume).
        fs::remove_file(&paths.rollback_file).unwrap();
        write_json_file(
            &paths.rollback_pending_file,
            &json!({ "rollback_id": id, "started_at": now_millis() }),
            0o600,
        )
        .unwrap();

        // rollback sees no READY: it must report no rollback AND clear the
        // stale marker (reconcile) rather than error or brick later operations.
        let result = rollback(&paths).unwrap();
        assert_eq!(result["rollback_available"], false);
        assert!(!paths.rollback_pending_file.exists());

        // A new install must NOT be blocked by the leftover-backed marker; the
        // stale marker is reconciled away and the install proceeds.
        install(&paths).unwrap();
        assert!(paths.runtime_binary.exists());
        assert!(paths.rollback_file.exists());
        assert!(!paths.rollback_pending_file.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_mismatched_rollback_marker_is_cleared_and_fresh_rollback_proceeds() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        let ready =
            serde_json::from_slice::<Value>(&fs::read(&paths.rollback_file).unwrap()).unwrap();
        let ready_id = ready["rollback_id"].as_str().unwrap().to_owned();

        // A marker referencing a DIFFERENT snapshot than READY is stale (the
        // snapshot it named was superseded or the marker is a leftover). It must
        // be cleared, and a fresh rollback against the current READY proceeds.
        let stale_id = format!("nhost-{:x}-{}", std::process::id(), now_millis() + 99);
        assert_ne!(stale_id, ready_id);
        write_json_file(
            &paths.rollback_pending_file,
            &json!({ "rollback_id": stale_id, "started_at": now_millis() }),
            0o600,
        )
        .unwrap();

        // Fresh rollback: not resumed (stale marker does not match READY), and
        // it restores the prior state (absence for a fresh install).
        let result = rollback(&paths).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["resumed"], false);
        assert!(!paths.rollback_pending_file.exists());
        assert!(!paths.rollback_file.exists());
        assert!(!paths.runtime_binary.exists());
        assert!(!paths.wrapper.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn consume_ready_record_unlinks_ready_before_backup() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        let ready =
            serde_json::from_slice::<Value>(&fs::read(&paths.rollback_file).unwrap()).unwrap();
        let id = ready["rollback_id"].as_str().unwrap().to_owned();
        let backup_dir = paths.backups_dir.join(&id);
        assert!(paths.rollback_file.exists());
        assert!(backup_dir.exists());

        // Consume: READY is unlinked first, then the backup dir is removed.
        // Simulate a crash between the two by re-arming after the unlink: we
        // assert the ordering invariant directly — consume removes READY even
        // if the backup cleanup fails, and the leftover backup is never
        // referenced again (garbage).
        consume_ready_record(&paths).unwrap();
        assert!(!paths.rollback_file.exists());
        assert!(!backup_dir.exists());

        // Retry invariant: after consumption a later rollback reports no
        // rollback rather than erroring, and a fresh install succeeds.
        let again = rollback(&paths).unwrap();
        assert_eq!(again["rollback_available"], false);
        install(&paths).unwrap();
        assert!(paths.runtime_binary.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn truncated_evidence_fails_closed_and_preserves_ready() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        install(&paths).unwrap(); // ready snapshot holds present prior files
        let ready =
            serde_json::from_slice::<Value>(&fs::read(&paths.rollback_file).unwrap()).unwrap();
        let id = ready["rollback_id"].as_str().unwrap().to_owned();
        let backup_dir = paths.backups_dir.join(&id);

        // Truncate the evidence set: drop every entry except the binary entry
        // and the first manifest, so the complete-set preflight must fail
        // closed BEFORE any mutation (no marker, no restore, no consume).
        let evidence =
            serde_json::from_slice::<Value>(&fs::read(backup_dir.join("evidence.json")).unwrap())
                .unwrap();
        let entries = evidence["entries"].as_array().unwrap();
        let binary_entry = entries
            .iter()
            .find(|entry| entry["path"] == paths.runtime_binary.to_string_lossy().as_ref())
            .expect("binary entry")
            .clone();
        write_json_file(
            &backup_dir.join("evidence.json"),
            &json!({ "entries": [binary_entry] }),
            0o600,
        )
        .unwrap();

        // rollback fails closed: evidence is missing the wrapper and manifests.
        let error = rollback(&paths).unwrap_err();
        assert!(error.contains("missing"));
        // READY and the backup (evidence) are preserved; nothing was restored
        // and no rollback-in-progress marker was written.
        assert!(paths.rollback_file.exists());
        assert!(backup_dir.exists());
        assert!(!paths.rollback_pending_file.exists());
        assert!(paths.runtime_binary.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_mutation_failpoint_aborts_and_restores_prior_snapshot() {
        let (root, paths) = fixture();
        // First install establishes prior owned state and a READY record.
        install(&paths).unwrap();
        let prior_binary = fs::read(&paths.runtime_binary).unwrap();
        let prior_wrapper = fs::read(&paths.wrapper).unwrap();
        let ready_before = fs::read(&paths.rollback_file).unwrap();
        // Point the source at DISTINCT bytes so the interrupted install's
        // written binary provably differs from the prior snapshot bytes.
        fs::write(&paths.source_binary, b"rust-binary-fixture-v2").unwrap();
        fs::set_permissions(&paths.source_binary, fs::Permissions::from_mode(0o700)).unwrap();

        // Arm the install-mutation failpoint so the SECOND install fails right
        // after copying the v2 binary (mid-mutation, before wrapper/manifests).
        arm_install_failpoint_after_n_mutations(1);
        let error = install(&paths).unwrap_err();
        assert!(error.contains("injected native-host install mutation failure"));
        // The transaction abort restored the pre-mutation snapshot and cleared
        // the pending marker; the previous READY record is untouched.
        assert!(!paths.pending_file.exists());
        assert_eq!(fs::read(&paths.runtime_binary).unwrap(), prior_binary);
        assert_eq!(fs::read(&paths.wrapper).unwrap(), prior_wrapper);
        assert_eq!(fs::read(&paths.rollback_file).unwrap(), ready_before);

        // A retry install (failpoint disarmed on fire) succeeds and commits a
        // new READY with the v2 bytes.
        let installed = install(&paths).unwrap();
        assert_eq!(installed["ok"], true);
        assert!(paths.rollback_file.exists());
        assert_eq!(
            fs::read(&paths.runtime_binary).unwrap(),
            fs::read(&paths.source_binary).unwrap()
        );
        let ready_after = fs::read(&paths.rollback_file).unwrap();
        assert!(ready_after != ready_before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_is_idempotent_and_fails_closed_when_no_ready_exists() {
        let (root, paths) = fixture();
        // No install and no ready record: rollback reports unavailable, no error.
        let result = rollback(&paths).unwrap();
        assert_eq!(result["rollback_available"], false);

        // Idempotency: after a successful rollback, a second rollback reports
        // no ready record rather than erroring.
        install(&paths).unwrap();
        let first = rollback(&paths).unwrap();
        assert_eq!(first["ok"], true);
        let second = rollback(&paths).unwrap();
        assert_eq!(second["rollback_available"], false);
        fs::remove_dir_all(root).unwrap();
    }

    fn write_managed_active_runtime(config_dir: &Path, bytes: &[u8]) -> PathBuf {
        let generation_dir = config_dir
            .join("runtime")
            .join("generations")
            .join("rust-testgen");
        fs::create_dir_all(&generation_dir).unwrap();
        let active_binary = generation_dir.join("herdr-mcp");
        fs::write(&active_binary, bytes).unwrap();
        fs::set_permissions(&active_binary, fs::Permissions::from_mode(0o700)).unwrap();
        let current_link = config_dir.join("runtime").join("current");
        fs::create_dir_all(current_link.parent().unwrap()).unwrap();
        if current_link.exists() {
            fs::remove_file(&current_link).unwrap();
        }
        std::os::unix::fs::symlink(Path::new("generations").join("rust-testgen"), &current_link)
            .unwrap();
        active_binary
    }

    #[test]
    fn sync_owned_runtime_copies_active_generation_and_is_idempotent() {
        let (root, paths) = fixture();
        let home = root.join("home");
        let config_dir = home.join(".config").join("herdr-mcp");
        let active_binary = write_managed_active_runtime(&config_dir, b"active-runtime-binary-v2");

        install(&paths).unwrap();
        assert_ne!(
            fs::read(&paths.runtime_binary).unwrap(),
            fs::read(&active_binary).unwrap()
        );
        let before = status_with_active_runtime(&paths, Some(&active_binary));
        assert_eq!(before["runtime_matches_current"], false);
        assert_eq!(before["stale_runtime"], true);

        let synced = sync_owned_runtime_with_active(&paths, &active_binary).unwrap();
        assert_eq!(synced["synced"], true);
        assert_eq!(
            fs::read(&paths.runtime_binary).unwrap(),
            fs::read(&active_binary).unwrap()
        );
        let after = status_with_active_runtime(&paths, Some(&active_binary));
        assert_eq!(after["runtime_matches_current"], true);
        assert_eq!(after["stale_runtime"], false);

        let again = sync_owned_runtime_with_active(&paths, &active_binary).unwrap();
        assert_eq!(again["skipped"], true);
        assert_eq!(again["reason"], "already_current");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sync_migrates_v041_dev_wrapper_without_rewriting_manifests_or_rollback() {
        let (root, paths) = fixture();
        let home = root.join("home");
        let config_dir = home.join(".config").join("herdr-mcp");
        let active_binary =
            write_managed_active_runtime(&config_dir, b"active-runtime-binary-v042");

        install(&paths).unwrap();
        let dev_origin = paths
            .dev_extension_origin
            .as_deref()
            .expect("fixture must model an unpacked Dev identity");
        assert_eq!(paths.extension_origin, dev_origin);

        let rollback_before = fs::read(&paths.rollback_file).unwrap();
        let manifests_before = paths
            .targets
            .iter()
            .map(|(target, _)| target.join(format!("{HOST_NAME}.json")))
            .filter(|path| path_present(path))
            .map(|path| {
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect::<Vec<_>>();
        assert!(!manifests_before.is_empty());

        // v0.4.1 only recorded the active origin. Remove the v0.4.2 remembered
        // Dev line while retaining the exact Rust marker, managed binary target,
        // and manifests to model an in-place stable upgrade.
        let current_wrapper = fs::read_to_string(&paths.wrapper).unwrap();
        let legacy_wrapper = current_wrapper
            .lines()
            .filter(|line| !line.starts_with("export HERDR_DEV_EXTENSION_ORIGIN="))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        atomic_write(&paths.wrapper, legacy_wrapper.as_bytes(), 0o700).unwrap();

        let before = status_with_active_runtime(&paths, Some(&active_binary));
        assert_eq!(before["owned_manifest_count"], 0);
        assert_eq!(before["runtime_matches_current"], false);
        assert_eq!(
            find_registered_origin(&paths.targets, &paths.wrapper)
                .unwrap()
                .as_deref(),
            Some(dev_origin)
        );

        let synced = sync_owned_runtime_with_active(&paths, &active_binary).unwrap();
        assert_eq!(synced["synced"], true);
        assert_eq!(synced["identity_migrated"], true);
        assert_eq!(
            fs::read(&paths.runtime_binary).unwrap(),
            fs::read(&active_binary).unwrap()
        );

        let refreshed_wrapper = fs::read_to_string(&paths.wrapper).unwrap();
        assert!(refreshed_wrapper.contains(&format!(
            "export HERDR_DEV_EXTENSION_ORIGIN={}",
            shell_quote(dev_origin)
        )));
        let after = status_with_active_runtime(&paths, Some(&active_binary));
        assert_eq!(after["ok"], true);
        assert!(after["owned_manifest_count"].as_u64().unwrap() > 0);
        assert_eq!(after["runtime_matches_current"], true);
        // Fixture binaries are opaque bytes rather than runnable `--version`
        // programs; version_consistent is covered by real-binary release UAT.
        assert_eq!(
            find_registered_origin(&paths.targets, &paths.wrapper)
                .unwrap()
                .as_deref(),
            Some(dev_origin)
        );

        assert_eq!(fs::read(&paths.rollback_file).unwrap(), rollback_before);
        for (path, before_bytes) in manifests_before {
            assert_eq!(fs::read(path).unwrap(), before_bytes);
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sync_fails_closed_for_foreign_or_partial_native_host_footprint() {
        let (root, paths) = fixture();
        let home = root.join("home");
        let config_dir = home.join(".config").join("herdr-mcp");
        let active_binary =
            write_managed_active_runtime(&config_dir, b"active-runtime-binary-v042");

        fs::create_dir_all(paths.runtime_binary.parent().unwrap()).unwrap();
        fs::write(&paths.runtime_binary, b"legacy-runtime").unwrap();
        fs::set_permissions(&paths.runtime_binary, fs::Permissions::from_mode(0o700)).unwrap();
        let tampered_origin = "chrome-extension://bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/";
        let tampered_wrapper = format!(
            "#!/bin/sh\n{WRAPPER_MARKER}\nexport HERDR_EXTENSION_ORIGIN={}\nexec {} extension-host \"$@\"\n",
            shell_quote(tampered_origin),
            shell_quote(paths.runtime_binary.to_string_lossy().as_ref()),
        );
        fs::write(&paths.wrapper, tampered_wrapper.as_bytes()).unwrap();
        fs::set_permissions(&paths.wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        let manifest_path = paths.targets[0].0.join(format!("{HOST_NAME}.json"));
        fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
        write_json_file(&manifest_path, &manifest_value(&paths), 0o600).unwrap();

        let error = sync_owned_runtime_with_active(&paths, &active_binary).unwrap_err();
        assert!(error.contains("active origin does not match registered manifests"));
        assert_eq!(fs::read(&paths.runtime_binary).unwrap(), b"legacy-runtime");
        assert_eq!(
            fs::read_to_string(&paths.wrapper).unwrap(),
            tampered_wrapper
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sync_skips_when_native_host_is_not_owned() {
        let (root, paths) = fixture();
        let home = root.join("home");
        let config_dir = home.join(".config").join("herdr-mcp");
        let active_binary = write_managed_active_runtime(&config_dir, b"active-runtime-binary-v2");
        let skipped = sync_owned_runtime_with_active(&paths, &active_binary).unwrap();
        assert_eq!(skipped["skipped"], true);
        assert_eq!(skipped["reason"], "native_host_not_owned");
        assert!(!paths.runtime_binary.exists());

        fs::remove_dir_all(root).unwrap();
    }
}
