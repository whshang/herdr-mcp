use crate::cli::QualificationCommand;
use crate::paths::RuntimePaths;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const LOCK_SCHEMA_VERSION: u64 = 1;
const LOCK_RELATIVE_PATH: &str = "qualification/lock.json";
const MAX_LOCK_BYTES: usize = 16 * 1024;
const MAX_TRIGGER_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualificationLock {
    pub locked_at_ms: i64,
    pub pid: u32,
}

pub fn run(command: QualificationCommand) -> Result<ExitCode, String> {
    let paths = RuntimePaths::discover()?;
    match command {
        QualificationCommand::Lock => {
            // Linearize lock creation with service generation mutations. A
            // service transaction holds the same mutation lease before it
            // checks this file, so either the generation change wins first or
            // this lock does; they cannot silently cross.
            #[cfg(target_os = "macos")]
            let _mutation_lease = crate::service_manager::acquire_mutation_lock()?;
            let (lock, already_locked) = lock_in_dir(&paths.config_dir)?;
            print_status(&paths.config_dir, Some(&lock), already_locked)?;
        }
        QualificationCommand::Unlock => {
            #[cfg(target_os = "macos")]
            let _mutation_lease = crate::service_manager::acquire_mutation_lock()?;
            let removed = unlock_in_dir(&paths.config_dir)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "locked": false,
                    "removed": removed,
                    "path": lock_path(&paths.config_dir),
                }))
                .map_err(|error| format!("cannot encode qualification unlock status: {error}"))?
            );
        }
        QualificationCommand::Status => {
            let lock = read_lock(&paths.config_dir)?;
            print_status(&paths.config_dir, lock.as_ref(), false)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn ensure_generation_change_allowed(trigger: &str) -> Result<(), String> {
    let paths = RuntimePaths::discover()?;
    ensure_generation_change_allowed_in(&paths.config_dir, trigger)
}

pub fn ensure_generation_change_allowed_in(config_dir: &Path, trigger: &str) -> Result<(), String> {
    let trigger = normalized_trigger(trigger, "generation_change");
    match read_lock(config_dir) {
        Ok(None) => Ok(()),
        Ok(Some(lock)) => Err(format!(
            "qualification_lock_active: generation change blocked (trigger={trigger}, locked_at_ms={}, pid={}); run `herdr-mcp qualification unlock` after qualification",
            lock.locked_at_ms, lock.pid
        )),
        Err(error) => Err(format!(
            "qualification_lock_unreadable: {error}; generation change blocked fail-closed (trigger={trigger})"
        )),
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn generation_trigger(default: &str) -> String {
    std::env::var("HERDR_MCP_GENERATION_TRIGGER")
        .ok()
        .map(|value| normalized_trigger(&value, default))
        .unwrap_or_else(|| normalized_trigger(default, "generation_change"))
}

fn normalized_trigger(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if !value.is_empty()
        && value.len() <= MAX_TRIGGER_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return value.to_owned();
    }
    fallback
        .trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        .take(MAX_TRIGGER_LEN)
        .collect::<String>()
}

fn print_status(
    config_dir: &Path,
    lock: Option<&QualificationLock>,
    already_locked: bool,
) -> Result<(), String> {
    let value = match lock {
        Some(lock) => json!({
            "ok": true,
            "locked": true,
            "already_locked": already_locked,
            "locked_at_ms": lock.locked_at_ms,
            "pid": lock.pid,
            "path": lock_path(config_dir),
        }),
        None => json!({
            "ok": true,
            "locked": false,
            "already_locked": false,
            "path": lock_path(config_dir),
        }),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&value)
            .map_err(|error| format!("cannot encode qualification status: {error}"))?
    );
    Ok(())
}

fn lock_in_dir(config_dir: &Path) -> Result<(QualificationLock, bool), String> {
    match read_lock(config_dir) {
        Ok(Some(lock)) => return Ok((lock, true)),
        Ok(None) => {}
        Err(error) => {
            return Err(format!(
                "cannot acquire qualification lock because existing lock state is unsafe: {error}"
            ));
        }
    }

    ensure_secure_directory(config_dir)?;
    let qualification_dir = config_dir.join("qualification");
    ensure_secure_directory(&qualification_dir)?;
    let path = lock_path(config_dir);
    let lock = QualificationLock {
        locked_at_ms: now_ms(),
        pid: std::process::id(),
    };
    let bytes = serde_json::to_vec_pretty(&json!({
        "schema_version": LOCK_SCHEMA_VERSION,
        "locked_at_ms": lock.locked_at_ms,
        "pid": lock.pid,
    }))
    .map_err(|error| format!("cannot encode qualification lock: {error}"))?;

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return read_lock(config_dir)?
                .map(|existing| (existing, true))
                .ok_or_else(|| "qualification lock appeared but could not be read".to_owned());
        }
        Err(error) => {
            return Err(format!(
                "cannot create qualification lock {}: {error}",
                path.display()
            ));
        }
    };
    file.write_all(&bytes)
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            format!(
                "cannot persist qualification lock {}: {error}",
                path.display()
            )
        })?;
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        format!(
            "cannot secure qualification lock permissions {}: {error}",
            path.display()
        )
    })?;
    Ok((lock, false))
}

fn unlock_in_dir(config_dir: &Path) -> Result<bool, String> {
    let path = lock_path(config_dir);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "cannot inspect qualification lock {}: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "refusing to remove unsafe qualification lock path {}",
            path.display()
        ));
    }
    fs::remove_file(&path).map_err(|error| {
        format!(
            "cannot remove qualification lock {}: {error}",
            path.display()
        )
    })?;
    Ok(true)
}

fn read_lock(config_dir: &Path) -> Result<Option<QualificationLock>, String> {
    let path = lock_path(config_dir);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot inspect qualification lock {}: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "qualification lock must be a regular non-symlink file: {}",
            path.display()
        ));
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "qualification lock permissions are too broad (expected 0600): {}",
            path.display()
        ));
    }
    if metadata.len() as usize > MAX_LOCK_BYTES {
        return Err(format!(
            "qualification lock exceeds {MAX_LOCK_BYTES} bytes: {}",
            path.display()
        ));
    }
    let bytes = fs::read(&path)
        .map_err(|error| format!("cannot read qualification lock {}: {error}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "qualification lock is malformed JSON {}: {error}",
            path.display()
        )
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| "qualification lock must be a JSON object".to_owned())?;
    if object.get("schema_version").and_then(Value::as_u64) != Some(LOCK_SCHEMA_VERSION) {
        return Err("qualification lock has unsupported schema_version".to_owned());
    }
    let locked_at_ms = object
        .get("locked_at_ms")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| "qualification lock has invalid locked_at_ms".to_owned())?;
    let pid = object
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| "qualification lock has invalid pid".to_owned())?;
    Ok(Some(QualificationLock { locked_at_ms, pid }))
}

fn lock_path(config_dir: &Path) -> PathBuf {
    config_dir.join(LOCK_RELATIVE_PATH)
}

fn ensure_secure_directory(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "qualification directory must be a real directory: {}",
                path.display()
            ));
        }
    } else {
        fs::create_dir_all(path).map_err(|error| {
            format!(
                "cannot create qualification directory {}: {error}",
                path.display()
            )
        })?;
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "cannot secure qualification directory {}: {error}",
            path.display()
        )
    })?;
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn temp_config() -> PathBuf {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("herdr-qualification-{}-{id}", std::process::id()))
    }

    #[test]
    fn held_lock_blocks_generation_change_and_unlock_restores_it() {
        let dir = temp_config();
        let (lock, already_locked) = lock_in_dir(&dir).unwrap();
        assert!(!already_locked);
        assert!(lock.locked_at_ms > 0);
        let error = ensure_generation_change_allowed_in(&dir, "dev_sync").unwrap_err();
        assert!(error.contains("qualification_lock_active"));
        assert!(error.contains("dev_sync"));
        assert!(unlock_in_dir(&dir).unwrap());
        ensure_generation_change_allowed_in(&dir, "dev_sync").unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_lock_fails_closed_but_explicit_unlock_can_recover() {
        let dir = temp_config();
        let qualification_dir = dir.join("qualification");
        ensure_secure_directory(&qualification_dir).unwrap();
        let path = lock_path(&dir);
        fs::write(&path, b"not-json").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let error = ensure_generation_change_allowed_in(&dir, "auto_update").unwrap_err();
        assert!(error.contains("qualification_lock_unreadable"));
        assert!(unlock_in_dir(&dir).unwrap());
        ensure_generation_change_allowed_in(&dir, "auto_update").unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_lock_fails_closed_and_is_never_removed_as_a_lock() {
        use std::os::unix::fs::symlink;
        let dir = temp_config();
        let qualification_dir = dir.join("qualification");
        ensure_secure_directory(&qualification_dir).unwrap();
        let target = dir.join("target.json");
        fs::write(&target, b"{}").unwrap();
        symlink(&target, lock_path(&dir)).unwrap();
        let error = ensure_generation_change_allowed_in(&dir, "service_install").unwrap_err();
        assert!(error.contains("qualification_lock_unreadable"));
        assert!(unlock_in_dir(&dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generation_trigger_is_bounded_and_non_secret_shaped() {
        assert_eq!(normalized_trigger("dev_sync", "fallback"), "dev_sync");
        assert_eq!(
            normalized_trigger("bad trigger with spaces", "fallback"),
            "fallback"
        );
        assert_eq!(normalized_trigger(&"x".repeat(100), "fallback"), "fallback");
    }
}
