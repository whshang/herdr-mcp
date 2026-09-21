//! Stable user CLI entrypoint (`~/.local/bin/herdr-mcp`).
//!
//! Target architecture: the PATH entry resolves the installed active runtime
//! (`runtime/current/herdr-mcp`), never a git checkout, `target/` artifact, or
//! a fixed generation. `service install` / update activation maintains this
//! entry.
//!
//! Ownership is per-platform:
//! - Unix: `~/.local/bin/herdr-mcp` is a symlink to the active runtime binary.
//! - Windows: `%USERPROFILE%\.local\bin\herdr-mcp.cmd` is a small forwarding
//!   script to `runtime\current\herdr-mcp.exe`, and `%USERPROFILE%\.local\bin`
//!   is registered in the current user's PATH (HKCU `Environment`, idempotent).
//!   No admin elevation and no system-wide PATH mutation is used.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserCliLink {
    pub path: PathBuf,
    pub target: PathBuf,
    pub changed: bool,
}

/// `~/.local/bin/herdr-mcp[.cmd]` path under the given home.
/// On Windows the stable entry is a forwarding batch file.
pub fn user_cli_path(home: &Path) -> PathBuf {
    let name = if cfg!(windows) {
        "herdr-mcp.cmd"
    } else {
        "herdr-mcp"
    };
    home.join(".local").join("bin").join(name)
}

/// Ensure the stable user CLI entry resolves to the active runtime binary
/// (`…/runtime/current/herdr-mcp[.exe]`). Replaces a missing path, a prior
/// entry, or a regular-file bootstrap copy.
///
/// On Windows also registers the entry directory in the current user PATH in an
/// idempotent, case-insensitive way.
///
/// Refuses directories and other non-file/non-symlink nodes.
pub fn ensure_link(home: &Path, current_binary: &Path) -> Result<UserCliLink, String> {
    #[cfg(target_os = "windows")]
    {
        ensure_windows_cmd(home, current_binary)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let path = user_cli_path(home);
        let target = absolute_target(current_binary)?;

        if let Some(existing) = read_existing_symlink(&path)?
            && existing == target
        {
            return Ok(UserCliLink {
                path,
                target,
                changed: false,
            });
        }

        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {}
            Ok(_) => {
                return Err(format!(
                    "refusing to replace non-file user CLI path {}",
                    path.display()
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot inspect user CLI path {}: {error}",
                    path.display()
                ));
            }
        }

        let bin_dir = path
            .parent()
            .ok_or_else(|| "user CLI path has no parent directory".to_owned())?;
        fs::create_dir_all(bin_dir).map_err(|error| {
            format!(
                "cannot create user CLI directory {}: {error}",
                bin_dir.display()
            )
        })?;

        let temp = bin_dir.join(format!(
            ".herdr-mcp-link-{}-{}",
            std::process::id(),
            now_ms()
        ));
        if temp.exists() || fs::symlink_metadata(&temp).is_ok() {
            return Err(format!(
                "temporary user CLI link already exists: {}",
                temp.display()
            ));
        }

        {
            use std::os::unix::fs::symlink;
            symlink(&target, &temp).map_err(|error| {
                format!(
                    "cannot create temporary user CLI symlink {}: {error}",
                    temp.display()
                )
            })?;
        }

        if let Err(error) = fs::rename(&temp, &path) {
            let _ = fs::remove_file(&temp);
            return Err(format!(
                "cannot activate user CLI symlink {}: {error}",
                path.display()
            ));
        }

        Ok(UserCliLink {
            path,
            target,
            changed: true,
        })
    }
}

/// Remove the stable user CLI entry only when it is our managed entry to
/// `current_binary`. Foreign binaries and unrelated symlinks/entries are left
/// alone. On Windows also removes only our PATH directory entry.
pub fn remove_link_if_owned(home: &Path, current_binary: &Path) -> Result<bool, String> {
    #[cfg(target_os = "windows")]
    {
        remove_windows_cmd_if_owned(home, current_binary)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let path = user_cli_path(home);
        let target = absolute_target(current_binary)?;
        match read_existing_symlink(&path)? {
            Some(existing) if existing == target => {
                fs::remove_file(&path).map_err(|error| {
                    format!(
                        "cannot remove owned user CLI symlink {}: {error}",
                        path.display()
                    )
                })?;
                Ok(true)
            }
            Some(_) | None => Ok(false),
        }
    }
}

fn absolute_target(current_binary: &Path) -> Result<PathBuf, String> {
    if current_binary.is_absolute() {
        Ok(current_binary.to_path_buf())
    } else {
        Err(format!(
            "user CLI target must be absolute: {}",
            current_binary.display()
        ))
    }
}

fn read_existing_symlink(path: &Path) -> Result<Option<PathBuf>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = fs::read_link(path).map_err(|error| {
                format!("cannot read user CLI symlink {}: {error}", path.display())
            })?;
            Ok(Some(target))
        }
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot inspect user CLI path {}: {error}",
            path.display()
        )),
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Windows stable user CLI ownership
// ---------------------------------------------------------------------------
//
// Windows has no unprivileged symlink guarantee, so the stable entry is a
// forwarding batch file that resolves `runtime\current\herdr-mcp.exe` and a
// current-user PATH registration (HKCU `Environment`, no admin elevation).
// Ownership is scoped: `ensure_windows_cmd` creates/registers, and
// `remove_windows_cmd_if_owned` removes only the entry and PATH segment that
// match our runtime/current target.

#[cfg(target_os = "windows")]
use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ,
    REG_OPTION_NON_VOLATILE, RegCloseKey, RegCreateKeyExW, RegQueryValueExW, RegSetValueExW,
};

#[cfg(target_os = "windows")]
const ENVIRONMENT_REGISTRY_KEY: &str = r"Environment";
#[cfg(target_os = "windows")]
const PATH_VALUE_NAME: &str = "Path";

#[cfg(target_os = "windows")]
fn wide_z(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The batch payload that forwards all arguments to `runtime\current\herdr-mcp.exe`.
#[cfg(target_os = "windows")]
fn cmd_payload(target: &Path) -> String {
    format!("@echo off\r\n\"{}\" %*\r\n", target.to_string_lossy())
}

#[cfg(target_os = "windows")]
fn ensure_windows_cmd(home: &Path, current_binary: &Path) -> Result<UserCliLink, String> {
    let path = user_cli_path(home);
    let target = absolute_target(current_binary)?;
    let payload = cmd_payload(&target);
    let bin_dir = path
        .parent()
        .ok_or_else(|| "user CLI path has no parent directory".to_owned())?;

    let mut changed = false;

    // Reject a directory occupying the entry path (parity with unix guard).
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() => {
            return Err(format!(
                "refusing to replace non-file user CLI path {}",
                path.display()
            ));
        }
        Ok(_) | Err(_) => {}
    }

    match fs::read_to_string(&path) {
        Ok(existing) if existing == payload => {}
        Ok(_) | Err(_) => {
            fs::create_dir_all(bin_dir).map_err(|error| {
                format!(
                    "cannot create user CLI directory {}: {error}",
                    bin_dir.display()
                )
            })?;
            let temp = bin_dir.join(format!(
                ".herdr-mcp-cmd-{}-{}",
                std::process::id(),
                now_ms()
            ));
            fs::write(&temp, payload.as_bytes()).map_err(|error| {
                format!(
                    "cannot write temporary user CLI entry {}: {error}",
                    temp.display()
                )
            })?;
            if let Err(error) = fs::rename(&temp, &path) {
                let _ = fs::remove_file(&temp);
                return Err(format!(
                    "cannot activate user CLI entry {}: {error}",
                    path.display()
                ));
            }
            changed = true;
        }
    }

    if register_user_path(bin_dir, home)? {
        changed = true;
    }

    Ok(UserCliLink {
        path,
        target,
        changed,
    })
}

#[cfg(target_os = "windows")]
fn remove_windows_cmd_if_owned(home: &Path, current_binary: &Path) -> Result<bool, String> {
    let path = user_cli_path(home);
    let target = absolute_target(current_binary)?;
    let payload = cmd_payload(&target);

    let owned = match fs::read_to_string(&path) {
        Ok(existing) => existing == payload,
        Err(_) => false,
    };
    if !owned {
        return Ok(false);
    }

    let bin_dir = path
        .parent()
        .ok_or_else(|| "user CLI path has no parent directory".to_owned())?;
    fs::remove_file(&path).map_err(|error| {
        format!(
            "cannot remove owned user CLI entry {}: {error}",
            path.display()
        )
    })?;
    unregister_user_path(bin_dir, home)?;
    Ok(true)
}

/// Append the entry directory to the current user PATH if not already present.
/// Returns true when a new PATH segment was added. Idempotent and
/// case-insensitive (matching both a literal and `%USERPROFILE%`-expandable
/// form); never mutates the system PATH.
#[cfg(target_os = "windows")]
fn register_user_path(bin_dir: &Path, home: &Path) -> Result<bool, String> {
    let current = read_environment_string(PATH_VALUE_NAME)?;
    let candidate = normalized_segment(bin_dir);
    let profile_form = normalized_profile_form(bin_dir, home);
    if path_segment_present(&current, &candidate, &profile_form) {
        return Ok(false);
    }
    let updated = if current.trim().is_empty() {
        format!("{};", candidate)
    } else {
        let mut value = current.trim().to_owned();
        if !value.ends_with(';') {
            value.push(';');
        }
        value.push_str(&candidate);
        value.push(';');
        value
    };
    write_environment_string(PATH_VALUE_NAME, &updated)?;
    Ok(true)
}

/// Remove only our managed PATH segment. Foreign segments are preserved; a
/// PATH that did not contain our directory is left unchanged.
#[cfg(target_os = "windows")]
fn unregister_user_path(bin_dir: &Path, home: &Path) -> Result<(), String> {
    let current = read_environment_string(PATH_VALUE_NAME)?;
    let candidate = normalized_segment(bin_dir);
    let profile_form = normalized_profile_form(bin_dir, home);
    if !path_segment_present(&current, &candidate, &profile_form) {
        return Ok(());
    }
    let updated = current
        .split(';')
        .filter(|segment| !segment_eq(segment, &candidate, &profile_form))
        .collect::<Vec<_>>()
        .join(";");
    write_environment_string(PATH_VALUE_NAME, &updated)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn normalized_segment(bin_dir: &Path) -> String {
    // Trim a trailing separator and normalize case-insensitive comparisons.
    bin_dir
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_owned()
}

/// The `%USERPROFILE%`-expandable spelling of the same directory, so a PATH
/// previously written with the environment variable form is still treated as
/// already-present (idempotent against repeat installs).
#[cfg(target_os = "windows")]
fn normalized_profile_form(bin_dir: &Path, home: &Path) -> Option<String> {
    let literal = bin_dir.to_string_lossy();
    let home_literal = home.to_string_lossy();
    literal
        .strip_prefix(home_literal.as_ref())
        .map(|suffix| format!("%USERPROFILE%{}", suffix))
}

#[cfg(target_os = "windows")]
fn segment_eq(segment: &str, candidate: &str, profile_form: &Option<String>) -> bool {
    let trimmed = segment.trim().trim_end_matches(['\\', '/']);
    if trimmed.eq_ignore_ascii_case(candidate) {
        return true;
    }
    profile_form
        .as_deref()
        .map(|form| trimmed.eq_ignore_ascii_case(form))
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn path_segment_present(path_value: &str, candidate: &str, profile_form: &Option<String>) -> bool {
    path_value
        .split(';')
        .any(|segment| segment_eq(segment, candidate, profile_form))
}

#[cfg(target_os = "windows")]
fn read_environment_string(name: &str) -> Result<String, String> {
    let subkey = wide_z(ENVIRONMENT_REGISTRY_KEY);
    let name_w = wide_z(name);
    let mut key: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        return Err(format!(
            "cannot open HKCU Environment key for read: Win32 error {status}"
        ));
    }
    let mut value_type = 0u32;
    let mut byte_len = 0u32;
    let query_status = unsafe {
        RegQueryValueExW(
            key,
            name_w.as_ptr(),
            std::ptr::null(),
            &mut value_type,
            std::ptr::null_mut(),
            &mut byte_len,
        )
    };
    if query_status == ERROR_FILE_NOT_FOUND {
        unsafe { RegCloseKey(key) };
        return Ok(String::new());
    }
    if query_status != 0 || byte_len == 0 || byte_len > 32 * 1024 || byte_len % 2 != 0 {
        unsafe { RegCloseKey(key) };
        return Err(format!(
            "cannot query HKCU Environment value '{name}': Win32 error {query_status}"
        ));
    }
    let mut data = vec![0u16; byte_len as usize / 2];
    let mut actual_len = byte_len;
    let read_status = unsafe {
        RegQueryValueExW(
            key,
            name_w.as_ptr(),
            std::ptr::null(),
            &mut value_type,
            data.as_mut_ptr().cast::<u8>(),
            &mut actual_len,
        )
    };
    unsafe { RegCloseKey(key) };
    if read_status != 0 {
        return Err(format!(
            "cannot read HKCU Environment value '{name}': Win32 error {read_status}"
        ));
    }
    if actual_len % 2 != 0 || actual_len as usize > data.len() * 2 {
        return Err(format!(
            "HKCU Environment value '{name}' returned an invalid length"
        ));
    }
    data.truncate(actual_len as usize / 2);
    while data.last() == Some(&0) {
        data.pop();
    }
    String::from_utf16(&data)
        .map_err(|_| format!("HKCU Environment value '{name}' is not valid UTF-16"))
}

#[cfg(target_os = "windows")]
fn write_environment_string(name: &str, value: &str) -> Result<(), String> {
    let subkey = wide_z(ENVIRONMENT_REGISTRY_KEY);
    let name_w = wide_z(name);
    let value_w = wide_z(value);
    let mut key: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        return Err(format!(
            "cannot open HKCU Environment key for write: Win32 error {status}"
        ));
    }
    let bytes = value_w
        .len()
        .checked_mul(2)
        .and_then(|len| u32::try_from(len).ok())
        .ok_or_else(|| "HKCU Environment value is too large".to_owned())?;
    let write_status = unsafe {
        RegSetValueExW(
            key,
            name_w.as_ptr(),
            0,
            REG_EXPAND_SZ,
            value_w.as_ptr().cast::<u8>(),
            bytes,
        )
    };
    unsafe { RegCloseKey(key) };
    if write_status != 0 {
        return Err(format!(
            "cannot set HKCU Environment value '{name}': Win32 error {write_status}"
        ));
    }
    Ok(())
}

/// Note: we deliberately do not broadcast the user-environment change from
/// the install/upgrade path. Broadcasting `WM_SETTINGCHANGE` synchronously (or
/// from a process whose parent is waiting on it) can deadlock the install. New
/// shells inherit the PATH straight from the persisted HKCU `Environment`
/// value, which is what the UAT requires.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn home(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-user-cli-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn current_binary(home: &Path) -> PathBuf {
        let binary = home
            .join(".config")
            .join("herdr-mcp")
            .join("runtime")
            .join("current")
            .join("herdr-mcp");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(&binary, b"runtime-binary").unwrap();
        binary
    }

    #[test]
    fn creates_symlink_to_runtime_current_when_missing() {
        let home = home("create");
        let target = current_binary(&home);

        let result = ensure_link(&home, &target).unwrap();
        assert!(result.changed);
        assert_eq!(result.path, user_cli_path(&home));
        assert_eq!(result.target, target);
        assert_eq!(fs::read_link(&result.path).unwrap(), target);

        let again = ensure_link(&home, &target).unwrap();
        assert!(!again.changed);
        assert_eq!(fs::read_link(&again.path).unwrap(), target);
    }

    #[test]
    fn replaces_repo_bash_bridge_symlink() {
        let home = home("repo-bridge");
        let target = current_binary(&home);
        let path = user_cli_path(&home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let repo_bridge = home.join("Documents/herdr-mcp/bin/herdr-mcp");
        fs::create_dir_all(repo_bridge.parent().unwrap()).unwrap();
        fs::write(&repo_bridge, b"#!/bin/bash\n").unwrap();
        symlink(&repo_bridge, &path).unwrap();

        let result = ensure_link(&home, &target).unwrap();
        assert!(result.changed);
        assert_eq!(fs::read_link(&path).unwrap(), target);
    }

    #[test]
    fn replaces_bootstrap_regular_file_copy() {
        let home = home("regular-file");
        let target = current_binary(&home);
        let path = user_cli_path(&home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"bootstrap-binary-copy").unwrap();

        let result = ensure_link(&home, &target).unwrap();
        assert!(result.changed);
        assert_eq!(fs::read_link(&path).unwrap(), target);
    }

    #[test]
    fn refuses_directory_at_user_cli_path() {
        let home = home("directory");
        let target = current_binary(&home);
        let path = user_cli_path(&home);
        fs::create_dir_all(&path).unwrap();

        let error = ensure_link(&home, &target).unwrap_err();
        assert!(error.contains("refusing to replace non-file"));
        assert!(path.is_dir());
    }

    #[test]
    fn remove_only_owned_symlink() {
        let owned_home = home("remove-owned");
        let target = current_binary(&owned_home);
        ensure_link(&owned_home, &target).unwrap();
        assert!(remove_link_if_owned(&owned_home, &target).unwrap());
        assert!(fs::symlink_metadata(user_cli_path(&owned_home)).is_err());

        let foreign_home = home("remove-foreign");
        let target = current_binary(&foreign_home);
        let path = user_cli_path(&foreign_home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let foreign = foreign_home.join("other-bin");
        fs::write(&foreign, b"other").unwrap();
        symlink(&foreign, &path).unwrap();
        assert!(!remove_link_if_owned(&foreign_home, &target).unwrap());
        assert_eq!(fs::read_link(&path).unwrap(), foreign);
    }

    #[test]
    fn rejects_relative_target() {
        let home = home("relative");
        let error = ensure_link(&home, Path::new("runtime/current/herdr-mcp")).unwrap_err();
        assert!(error.contains("must be absolute"));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn path_registration_is_idempotent_literal_and_profile_form() {
        let home = Path::new("C:\\Users\\example");
        let bin_dir = home.join(".local").join("bin");
        let candidate = normalized_segment(&bin_dir);
        let profile_form = normalized_profile_form(&bin_dir, home);

        // Fresh PATH: not present.
        assert!(!path_segment_present(
            "C:\\Windows;C:\\Tools",
            &candidate,
            &profile_form
        ));
        // Literal already present (repeat install must not append again).
        assert!(path_segment_present(
            "C:\\Windows;C:\\USERS\\EXAMPLE\\.local\\bin;C:\\Tools",
            &candidate,
            &profile_form
        ));
        // %USERPROFILE% expandable form already present: also idempotent.
        assert!(path_segment_present(
            "C:\\Windows;%USERPROFILE%\\.local\\bin;C:\\Tools",
            &candidate,
            &profile_form
        ));
    }

    #[test]
    fn unregistration_removes_only_our_segment() {
        let home = Path::new("C:\\Users\\example");
        let bin_dir = home.join(".local").join("bin");
        let candidate = normalized_segment(&bin_dir);
        let profile_form = normalized_profile_form(&bin_dir, home);
        let current = "C:\\Windows;c:\\users\\example\\.local\\bin;C:\\Tools";
        assert!(path_segment_present(current, &candidate, &profile_form));
        let updated = current
            .split(';')
            .filter(|segment| !segment_eq(segment, &candidate, &profile_form))
            .collect::<Vec<_>>()
            .join(";");
        assert_eq!(updated, "C:\\Windows;C:\\Tools");
    }
}
