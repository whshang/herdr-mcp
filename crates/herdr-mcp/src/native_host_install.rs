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
    targets: Vec<(PathBuf, bool)>,
    backups_dir: PathBuf,
    rollback_file: PathBuf,
    pending_file: PathBuf,
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
            NativeHostCommand::Install => install(&paths)?,
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

#[cfg(target_os = "macos")]
impl InstallPaths {
    fn discover(command: &NativeHostCommand) -> Result<Self, String> {
        let home = home_dir()?;
        let runtime_paths = crate::paths::RuntimePaths::discover()?;
        let source_binary = env::current_exe()
            .map_err(|error| format!("cannot locate current herdr-mcp binary: {error}"))?;
        let native_dir = runtime_paths.config_dir.join("native");
        let wrapper = native_dir.join("herdr-extension-host");
        let targets = install_targets(&home);

        // Rollback (and read-only status) must be able to recover the exact
        // managed origin/paths from durable metadata even when the current
        // worktree, extension path, or live manifest is not intact or missing.
        if (matches!(command, NativeHostCommand::Rollback)
            || matches!(command, NativeHostCommand::Status))
            && let Some(origin) = durable_extension_origin(&native_dir)?
        {
            let extension_id = extension_id_from_origin(&origin)
                .ok_or_else(|| "durable native-host origin is invalid".to_owned())?;
            return Ok(Self {
                source_binary,
                runtime_binary: native_dir.join("herdr-mcp"),
                wrapper,
                extension_path: None,
                extension_id,
                extension_origin: origin,
                targets,
                backups_dir: native_dir.join(BACKUPS_DIR_NAME),
                rollback_file: native_dir.join(ROLLBACK_FILE),
                pending_file: native_dir.join(PENDING_FILE),
            });
        }

        if matches!(command, NativeHostCommand::Install) {
            if let Some(extension_origin) = find_registered_origin(&targets, &wrapper)? {
                let extension_id = extension_id_from_origin(&extension_origin)
                    .ok_or_else(|| "registered native-host origin is invalid".to_owned())?;
                let extension_path = crate::native_host::extension_path_for_install()
                    .ok()
                    .filter(|path| {
                        crate::native_host::chromium_id_for_path(path)
                            .ok()
                            .is_some_and(|id| id == extension_id)
                    });
                return Ok(Self {
                    source_binary,
                    runtime_binary: native_dir.join("herdr-mcp"),
                    wrapper,
                    extension_path,
                    extension_id,
                    extension_origin,
                    targets,
                    backups_dir: native_dir.join(BACKUPS_DIR_NAME),
                    rollback_file: native_dir.join(ROLLBACK_FILE),
                    pending_file: native_dir.join(PENDING_FILE),
                });
            }
            let extension_path = crate::native_host::extension_path_for_install()?;
            return Self::for_layout(
                &extension_path,
                Some(extension_path.clone()),
                source_binary,
                native_dir,
                wrapper,
                targets,
            );
        }

        if let Some(extension_origin) = find_registered_origin(&targets, &wrapper)? {
            let extension_id = extension_id_from_origin(&extension_origin)
                .ok_or_else(|| "registered native-host origin is invalid".to_owned())?;
            let live_path = crate::native_host::extension_path_for_install().ok();
            let extension_path = live_path.filter(|path| {
                crate::native_host::chromium_id_for_path(path)
                    .ok()
                    .is_some_and(|id| id == extension_id)
            });
            return Ok(Self {
                source_binary,
                runtime_binary: native_dir.join("herdr-mcp"),
                wrapper,
                extension_path,
                extension_id,
                extension_origin,
                targets,
                backups_dir: native_dir.join(BACKUPS_DIR_NAME),
                rollback_file: native_dir.join(ROLLBACK_FILE),
                pending_file: native_dir.join(PENDING_FILE),
            });
        }

        let extension_path = crate::native_host::extension_path_for_install()?;
        Self::for_layout(
            &extension_path,
            Some(extension_path.clone()),
            source_binary,
            native_dir,
            wrapper,
            targets,
        )
    }

    #[cfg(test)]
    fn for_values(
        home: &Path,
        extension_path: &Path,
        source_binary: &Path,
    ) -> Result<Self, String> {
        let native_dir = home.join(".config").join("herdr-mcp").join("native");
        let wrapper = native_dir.join("herdr-extension-host");
        Self::for_layout(
            extension_path,
            Some(extension_path.to_path_buf()),
            source_binary.to_path_buf(),
            native_dir,
            wrapper,
            install_targets(home),
        )
    }

    fn for_layout(
        identity_path: &Path,
        extension_path: Option<PathBuf>,
        source_binary: PathBuf,
        native_dir: PathBuf,
        wrapper: PathBuf,
        targets: Vec<(PathBuf, bool)>,
    ) -> Result<Self, String> {
        let extension_id = crate::native_host::chromium_id_for_path(identity_path)?;
        let extension_origin = format!("chrome-extension://{extension_id}/");
        let backups_dir = native_dir.join(BACKUPS_DIR_NAME);
        let rollback_file = native_dir.join(ROLLBACK_FILE);
        let pending_file = native_dir.join(PENDING_FILE);
        Ok(Self {
            source_binary,
            runtime_binary: native_dir.join("herdr-mcp"),
            wrapper,
            extension_path,
            extension_id,
            extension_origin,
            targets,
            backups_dir,
            rollback_file,
            pending_file,
        })
    }
}

#[cfg(target_os = "macos")]
fn install(paths: &InstallPaths) -> Result<Value, String> {
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
    let runtime_binary_ok = is_regular_executable(&paths.runtime_binary);
    let wrapper_ok = wrapper_is_rust(&paths.wrapper);
    let runtime_matches_current = runtime_binary_ok
        && file_sha256(&paths.source_binary)
            .ok()
            .is_some_and(|source| {
                file_sha256(&paths.runtime_binary).ok().as_deref() == Some(source.as_str())
            });
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
    let recovery_required = path_present(&paths.pending_file);
    json!({
        "ok": runtime_binary_ok && wrapper_ok && owned_count > 0,
        "implementation": "rust",
        "host": HOST_NAME,
        "extension_id": paths.extension_id,
        "extension_path": paths.extension_path,
        "extension_origin": paths.extension_origin,
        "runtime_binary": paths.runtime_binary,
        "runtime_binary_ok": runtime_binary_ok,
        "runtime_matches_current": runtime_matches_current,
        "wrapper": paths.wrapper,
        "wrapper_ok": wrapper_ok,
        "owned_manifest_count": owned_count,
        "rollback_available": rollback_available,
        "recovery_required": recovery_required,
        "manifests": manifests,
    })
}

#[cfg(target_os = "macos")]
fn uninstall(paths: &InstallPaths) -> Result<Value, String> {
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
#[cfg(target_os = "macos")]
fn rollback(paths: &InstallPaths) -> Result<Value, String> {
    // First-class rollback after interruption: restore the pending snapshot
    // when an install was interrupted mid-mutation. This recovers the prior
    // state; the previous ready rollback (if any) stays intact.
    if path_present(&paths.pending_file) {
        recover_pending(paths)?;
        return Ok(json!({
            "ok": true,
            "implementation": "rust",
            "host": HOST_NAME,
            "recovered_pending_install": true,
        }));
    }

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
    let snapshot = read_json_file(&backup_dir.join("evidence.json"))?
        .ok_or_else(|| "native-host rollback evidence is missing".to_owned())?;

    // Fail closed if any managed file would be overwritten by a foreign,
    // symlinked, or unowned target.
    reject_non_regular_managed_targets(paths)?;
    validate_cohort_ownership(paths)?;

    // Verify the current on-disk state exactly matches the ready record's
    // activated fingerprint (runtime + wrapper + every manifest), so rollback
    // never restores over a foreign or tampered mutation.
    validate_activated_state(paths, &ready)?;

    let mut restored = Vec::new();
    let mut removed_absent = Vec::new();
    restore_evidence(
        paths,
        &backup_dir,
        &snapshot,
        &mut restored,
        &mut removed_absent,
    )?;

    consume_ready_record(paths)?;
    Ok(json!({
        "ok": true,
        "implementation": "rust",
        "host": HOST_NAME,
        "rollback_id": rollback_id,
        "restored": restored,
        "removed_absent": removed_absent,
    }))
}

/// Recover a pending (interrupted) native-host install transaction by restoring
/// the pre-mutation snapshot and clearing the pending marker. The previous
/// ready rollback (if any) is left untouched. Idempotent: a missing pending
/// record is a no-op.
#[cfg(target_os = "macos")]
fn recover_pending(paths: &InstallPaths) -> Result<(), String> {
    let Some(pending) = read_json_file(&paths.pending_file)? else {
        return Ok(());
    };
    let rollback_id = pending
        .get("rollback_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "native-host pending install has no rollback_id".to_owned())?;
    let backup_dir = paths.backups_dir.join(rollback_id);
    if backup_dir.parent() != Some(paths.backups_dir.as_path()) {
        return Err("native-host pending backup escapes managed backup dir".to_owned());
    }
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
/// marker. The previous ready rollback (if any) is left intact.
#[cfg(target_os = "macos")]
fn abort_pending_install(paths: &InstallPaths) -> Result<(), String> {
    let Some(pending) = read_json_file(&paths.pending_file)? else {
        return Ok(());
    };
    let rollback_id = pending
        .get("rollback_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "native-host pending install has no rollback_id".to_owned())?;
    let backup_dir = paths.backups_dir.join(rollback_id);
    if backup_dir.parent() != Some(paths.backups_dir.as_path()) {
        return Err("native-host pending backup escapes managed backup dir".to_owned());
    }
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
            if view.get("owned").and_then(Value::as_bool) != Some(true) {
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

/// Verify the current on-disk managed state exactly matches the ready record's
/// activated fingerprint (runtime + wrapper + every manifest content/presence),
/// so rollback only restores over the exact owned build and never a tampered or
/// foreign one.
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
    let expected_manifests = ready.get("activated_manifests").and_then(Value::as_array);
    if let Some(expected_manifests) = expected_manifests {
        for entry in expected_manifests {
            let manifest_path = entry
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| "activated manifest record has no path".to_owned())?;
            let expected_present = entry
                .get("present")
                .and_then(Value::as_bool)
                .ok_or_else(|| "activated manifest record has no presence".to_owned())?;
            let path = PathBuf::from(manifest_path);
            let present = path_present(&path);
            if present != expected_present {
                return Err(format!(
                    "native-host manifest {} presence does not match activated fingerprint",
                    path.display()
                ));
            }
            if expected_present {
                let expected_sha = entry
                    .get("sha256")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "activated manifest record has no sha256".to_owned())?;
                if file_sha256(&path).ok().as_deref() != Some(expected_sha) {
                    return Err(format!(
                        "native-host manifest {} is tampered; refusing rollback",
                        path.display()
                    ));
                }
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
                file_sha256(&path).ok()
            } else {
                None
            };
            json!({
                "path": path_str,
                "present": present,
                "sha256": sha256,
            })
        })
        .collect::<Vec<_>>();
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

/// Restore the prior managed regular files/absence from immutable evidence,
/// confined to the exact paths we own, restoring the recorded mode class and
/// verifying each backup is a regular non-symlink file whose recorded SHA is
/// intact before overwrite.
#[cfg(target_os = "macos")]
fn restore_evidence(
    paths: &InstallPaths,
    backup_dir: &Path,
    evidence: &Value,
    restored: &mut Vec<String>,
    removed_absent: &mut Vec<String>,
) -> Result<(), String> {
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
            let expected_sha = entry.get("sha256").and_then(Value::as_str);
            if let Some(expected) = expected_sha
                && file_sha256(&backup_path).ok().as_deref() != Some(expected)
            {
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
    }
    Ok(())
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

/// Remove the ready rollback record (and its now-superseded immutable backup)
/// only after a fully successful rollback restore.
#[cfg(target_os = "macos")]
fn consume_ready_record(paths: &InstallPaths) -> Result<(), String> {
    if let Ok(Some(ready)) = read_json_file(&paths.rollback_file)
        && let Some(id) = ready.get("rollback_id").and_then(Value::as_str)
    {
        let backup_dir = paths.backups_dir.join(id);
        if backup_dir.parent() == Some(paths.backups_dir.as_path()) {
            let _ = fs::remove_dir_all(&backup_dir);
        }
    }
    fs::remove_file(&paths.rollback_file)
        .map_err(|error| format!("cannot consume native-host rollback record: {error}"))
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
fn wrapper_body(paths: &InstallPaths) -> String {
    format!(
        "#!/bin/sh\n{WRAPPER_MARKER}\nexport HERDR_EXTENSION_ORIGIN={}\nexec {} extension-host \"$@\"\n",
        shell_quote(&paths.extension_origin),
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
        .is_some_and(|origins| origins.iter().any(|value| value == &paths.extension_origin));
    let rust_wrapper = wrapper_is_rust(&paths.wrapper);
    let owned = rust_wrapper
        && manifest.get("name").and_then(Value::as_str) == Some(HOST_NAME)
        && manifest.get("type").and_then(Value::as_str) == Some("stdio")
        && host_path == paths.wrapper.to_str()
        && allowed;
    json!({
        "path": path,
        "host_path": host_path,
        "allowed": allowed,
        "rust_wrapper": rust_wrapper,
        "owned": owned,
    })
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

        let removed = uninstall(&paths).unwrap();
        assert_eq!(removed["ok"], true);
        assert!(!stable_manifest.exists());
        assert!(!paths.wrapper.exists());
        assert!(!paths.runtime_binary.exists());
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
    fn pending_install_is_recovered_before_next_install_and_keeps_ready() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        assert!(paths.rollback_file.exists());

        // Simulate an interrupted second install: snapshot + pending marker
        // written but mutation not committed and ready not yet replaced.
        let (id, _dir) = snapshot_evidence(&paths).unwrap();
        write_json_file(
            &paths.pending_file,
            &json!({ "rollback_id": id, "extension_origin": paths.extension_origin }),
            0o600,
        )
        .unwrap();
        let prior_ready = fs::read(&paths.rollback_file).unwrap();

        // A fresh install first recovers the pending transaction, then proceeds.
        install(&paths).unwrap();
        assert!(!paths.pending_file.exists());
        // The ready rollback was replaced by the new install's commit.
        let new_ready = fs::read(&paths.rollback_file).unwrap();
        assert_ne!(prior_ready, new_ready);
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
}
