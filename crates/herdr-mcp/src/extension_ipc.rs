//! Trusted browser-extension IPC socket lifecycle.
//!
//! The Chrome Native Messaging broker is origin-restricted by Chromium and
//! forwards local requests over this mode-0600 Unix socket. Binding is
//! intentionally opt-in during the Rust migration; production ownership is
//! enabled only after `/mcp` and every `/push/*` route have parity.

use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

#[cfg(unix)]
#[derive(Debug)]
pub struct ExtensionIpcSocket {
    path: PathBuf,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl ExtensionIpcSocket {
    pub async fn bind(path: impl AsRef<Path>) -> Result<(tokio::net::UnixListener, Self), String> {
        let path = path.as_ref();
        prepare_socket_path(path)?;
        let listener = tokio::net::UnixListener::bind(path)
            .map_err(|error| format!("cannot bind extension IPC {}: {error}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("cannot secure extension IPC {}: {error}", path.display()))?;
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| format!("cannot stat extension IPC {}: {error}", path.display()))?;
        if !metadata.file_type().is_socket() {
            return Err(format!(
                "extension IPC {} is not a Unix socket after bind",
                path.display()
            ));
        }
        Ok((
            listener,
            Self {
                path: path.to_path_buf(),
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        ))
    }

    fn remove_if_owned(&self) {
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
impl Drop for ExtensionIpcSocket {
    fn drop(&mut self) {
        self.remove_if_owned();
    }
}

#[cfg(unix)]
fn prepare_socket_path(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "extension IPC path must have a parent directory".to_owned())?;
    if let Ok(metadata) = std::fs::symlink_metadata(parent)
        && metadata.file_type().is_symlink()
    {
        return Err(format!(
            "extension IPC parent {} must not be a symlink",
            parent.display()
        ));
    }
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "cannot create extension IPC parent {}: {error}",
            parent.display()
        )
    })?;

    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "extension IPC {} must not be a symlink",
            path.display()
        ));
    }
    if !metadata.file_type().is_socket() {
        return Err(format!(
            "extension IPC {} exists and is not a Unix socket",
            path.display()
        ));
    }
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return Err(format!(
            "extension IPC {} is already accepting connections",
            path.display()
        ));
    }
    std::fs::remove_file(path).map_err(|error| {
        format!(
            "cannot remove stale extension IPC {}: {error}",
            path.display()
        )
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "hmi-{:x}-{:x}-{nonce:x}.sock",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    async fn wait_until_socket_stale(path: &Path) {
        for _ in 0..200 {
            if std::os::unix::net::UnixStream::connect(path).is_err() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!(
            "socket remained connectable after listener drop: {}",
            path.display()
        );
    }

    #[tokio::test]
    async fn bind_secures_socket_and_drop_removes_owned_inode() {
        let path = temp_path();
        let (listener, guard) = ExtensionIpcSocket::bind(&path).await.unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        drop(listener);
        drop(guard);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn live_socket_is_never_unlinked_by_second_bind() {
        let path = temp_path();
        let (listener, guard) = ExtensionIpcSocket::bind(&path).await.unwrap();
        let client = std::os::unix::net::UnixStream::connect(&path).unwrap();
        drop(client);

        let error = ExtensionIpcSocket::bind(&path).await.unwrap_err();
        assert!(error.contains("already accepting connections"));
        assert!(path.exists());
        drop(listener);
        drop(guard);
    }

    #[tokio::test]
    async fn stale_socket_is_replaced_but_regular_file_is_refused() {
        let path = temp_path();
        let stale = tokio::net::UnixListener::bind(&path).unwrap();
        drop(stale);
        wait_until_socket_stale(&path).await;
        let (replacement, guard) = ExtensionIpcSocket::bind(&path).await.unwrap();
        drop(replacement);
        drop(guard);

        std::fs::write(&path, b"do-not-delete").unwrap();
        let error = ExtensionIpcSocket::bind(&path).await.unwrap_err();
        assert!(error.contains("not a Unix socket"));
        assert_eq!(std::fs::read(&path).unwrap(), b"do-not-delete");
        std::fs::remove_file(&path).unwrap();
    }
}
