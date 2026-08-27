//! Stable user CLI entrypoint (`~/.local/bin/herdr-mcp`).
//!
//! Target architecture: the PATH command resolves the installed active runtime
//! (`runtime/current/herdr-mcp`), never a git checkout or `target/` artifact.
//! `service install` / update activation maintains this link.

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

/// `~/.local/bin/herdr-mcp` path under the given home.
pub fn user_cli_path(home: &Path) -> PathBuf {
    home.join(".local").join("bin").join("herdr-mcp")
}

/// Ensure `~/.local/bin/herdr-mcp` is a symlink to the active runtime binary
/// (`…/runtime/current/herdr-mcp`). Replaces a missing path, a prior symlink
/// (including the repo Bash bridge), or a regular-file bootstrap copy.
///
/// Refuses directories and other non-file/non-symlink nodes.
pub fn ensure_link(home: &Path, current_binary: &Path) -> Result<UserCliLink, String> {
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

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(&target, &temp).map_err(|error| {
            format!(
                "cannot create temporary user CLI symlink {}: {error}",
                temp.display()
            )
        })?;
    }

    #[cfg(not(unix))]
    {
        let _ = (&target, &temp);
        return Err("user CLI linking requires unix symlinks".to_owned());
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

/// Remove `~/.local/bin/herdr-mcp` only when it is our managed symlink to
/// `current_binary`. Foreign binaries and unrelated symlinks are left alone.
pub fn remove_link_if_owned(home: &Path, current_binary: &Path) -> Result<bool, String> {
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
