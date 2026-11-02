//! Bounded, rotating, content-free on-disk log for the Chrome Native Messaging
//! host.
//!
//! Chrome starts `herdr-mcp extension-host` once per `chrome.runtime.connectNative`
//! port, so one "connection" here is one host process lifetime. Issue #629 needs
//! that lifetime on disk because the runtime's own `server.log` cannot see a host
//! that never reached the runtime: a wrong extension id, a dead IPC socket, or an
//! oversized frame all end before the runtime is involved.
//!
//! Everything written here is metadata: event name, timestamp, host version,
//! extension origin, message type, encoded size, error kind and durations.
//! Message bodies, page content, URLs (and their query strings), headers, tokens
//! and credentials must never be passed to this module.
//!
//! Logging is best-effort. Every failure — uncreatable directory, unwritable
//! file, contention with another host process — drops the event and returns; the
//! native host never blocks on, retries, or fails because of its log.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// One native-host log file never exceeds this many bytes before it rotates.
pub(crate) const NATIVE_HOST_LOG_MAX_BYTES: u64 = 1024 * 1024;
/// Rotated siblings kept beside the active file (`native-host.log.1` and up).
pub(crate) const NATIVE_HOST_LOG_BACKUPS: usize = 2;

const LOG_DIR_NAME: &str = "native-host";
const LOG_FILE_NAME: &str = "native-host.log";
const LOG_LOCK_FILE_NAME: &str = "native-host.log.lock";
/// A validated origin is at most 51 bytes and an error code is shorter, so this
/// bound never truncates a value into an unmatchable prefix while still keeping
/// one event on one bounded line.
const MAX_FIELD_CHARS: usize = 128;

/// One bounded, rotating log file set owned by the native host.
#[derive(Debug, Clone)]
pub(crate) struct NativeHostLog {
    dir: PathBuf,
    max_bytes: u64,
    backups: usize,
}

impl NativeHostLog {
    /// Resolve the log through the runtime's existing config root, so
    /// `HERDR_MCP_CONFIG_DIR`, `XDG_CONFIG_HOME` and the named-instance leaf are
    /// honoured exactly as the service's own state and `server.log` are.
    pub(crate) fn discover() -> Option<Self> {
        let paths = crate::paths::RuntimePaths::discover().ok()?;
        Some(Self::in_config_dir(&paths.config_dir))
    }

    pub(crate) fn in_config_dir(config_dir: &Path) -> Self {
        Self {
            dir: config_dir.join(LOG_DIR_NAME),
            max_bytes: NATIVE_HOST_LOG_MAX_BYTES,
            backups: NATIVE_HOST_LOG_BACKUPS,
        }
    }

    #[cfg(test)]
    fn with_cap(config_dir: &Path, max_bytes: u64, backups: usize) -> Self {
        Self {
            dir: config_dir.join(LOG_DIR_NAME),
            max_bytes,
            backups,
        }
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.dir.join(LOG_FILE_NAME)
    }

    /// Append one content-free event. Callers pass only opaque ids, enum codes,
    /// sizes and durations; nothing here inspects or copies a payload.
    pub(crate) fn record(&self, event: &str, fields: &[(&str, String)]) {
        let _ = self.append(&self.line(event, fields));
    }

    fn line(&self, event: &str, fields: &[(&str, String)]) -> String {
        let mut line = format!(
            "herdr-mcp: native-host ts={} event={}",
            timestamp(),
            sanitize(event)
        );
        for (key, value) in fields {
            line.push(' ');
            line.push_str(&sanitize(key));
            line.push('=');
            line.push_str(&sanitize(value));
        }
        line
    }

    fn append(&self, line: &str) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        secure_dir(&self.dir)?;
        // Rotation renames the active file, so concurrent hosts must not append
        // into a file that another host is rotating. Contention drops the event
        // instead of blocking the browser-facing process.
        let _lock = self.lock()?;
        let bytes = line.as_bytes();
        if let Ok(metadata) = fs::metadata(self.path())
            && metadata.len() > 0
            && metadata.len().saturating_add(bytes.len() as u64) > self.max_bytes
        {
            self.rotate()?;
        }
        let mut file = open_append(&self.path())?;
        file.write_all(bytes)?;
        file.write_all(b"\n")?;
        file.flush()
    }

    fn backup_path(&self, index: usize) -> PathBuf {
        let mut name = self.path().into_os_string();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }

    /// Drop the oldest backup, shift the rest up, and move the active file to
    /// `native-host.log.1`. A rotated set never exceeds
    /// `max_bytes * (backups + 1)`.
    fn rotate(&self) -> io::Result<()> {
        if self.backups == 0 {
            return remove_if_present(&self.path());
        }
        remove_if_present(&self.backup_path(self.backups))?;
        for index in (1..self.backups).rev() {
            let from = self.backup_path(index);
            if fs::symlink_metadata(&from).is_ok() {
                fs::rename(&from, self.backup_path(index + 1))?;
            }
        }
        if fs::symlink_metadata(self.path()).is_ok() {
            fs::rename(self.path(), self.backup_path(1))?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn lock(&self) -> io::Result<LogLock> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(self.dir.join(LOG_LOCK_FILE_NAME))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(LogLock { file })
    }

    #[cfg(not(unix))]
    fn lock(&self) -> io::Result<LogLock> {
        Ok(LogLock)
    }
}

#[cfg(unix)]
struct LogLock {
    file: fs::File,
}

#[cfg(unix)]
impl Drop for LogLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(not(unix))]
struct LogLock;

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The log directory is ours, so it is created (and repaired) as `0700`. The
/// shared config root above it is never modified.
#[cfg(unix)]
fn secure_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.is_dir() {
        return Err(io::Error::other("native-host log path is not a directory"));
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

fn open_append(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let wide = file
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o077 != 0)
            .unwrap_or(false);
        if wide {
            // Repair a log another tool created with a wider mode. A repair
            // failure still leaves the file inside the `0700` directory, so the
            // event is written rather than dropped.
            let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
        }
    }
    Ok(file)
}

fn timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// Keep one event on one greppable line: no control characters, no embedded
/// whitespace that would split a field, and a bounded width.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .take(MAX_FIELD_CHARS)
        .map(|character| {
            if character.is_control() || character.is_whitespace() {
                '_'
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "hmh-log-{label}-{:x}-{:x}-{nonce:x}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn native_host_log_bounds_every_file_and_rotates_oldest_out() {
        const { assert!(NATIVE_HOST_LOG_MAX_BYTES == 1024 * 1024) };
        const { assert!(NATIVE_HOST_LOG_BACKUPS == 2) };

        let root = temp_dir("rotate");
        let log = NativeHostLog::with_cap(&root, 512, 2);
        for sequence in 0..40 {
            log.record("connect", &[("sequence", sequence.to_string())]);
        }

        let active = fs::read_to_string(log.path()).unwrap();
        assert!(active.len() as u64 <= 512, "active file exceeded its cap");
        assert!(active.contains("sequence=39"), "newest event was not kept");
        for index in 1..=2 {
            let backup = log.backup_path(index);
            assert!(backup.is_file(), "missing rotated file {}", index);
            assert!(fs::metadata(&backup).unwrap().len() <= 512);
        }
        assert!(
            !log.backup_path(3).exists(),
            "rotation kept more backups than configured"
        );
        let oldest = fs::read_to_string(log.backup_path(2)).unwrap();
        assert!(
            !oldest.contains("sequence=0 "),
            "oldest event should have been rotated out"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_host_log_files_and_directory_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_dir("modes");
        let log = NativeHostLog::in_config_dir(&root);
        log.record("connect", &[("version", "1.1.0".to_owned())]);

        let dir_mode = fs::metadata(log.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        let file_mode = fs::metadata(log.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        let lock_mode = fs::metadata(root.join("native-host/native-host.log.lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(lock_mode, 0o600);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_host_log_drops_events_when_the_directory_is_unusable() {
        let root = temp_dir("unusable");
        // A regular file where the log directory must be makes `create_dir_all`
        // fail for every event; the host must keep running without a log.
        fs::write(root.join(LOG_DIR_NAME), b"not a directory").unwrap();
        let log = NativeHostLog::in_config_dir(&root);
        log.record("connect", &[("version", "1.1.0".to_owned())]);
        log.record("disconnect", &[("reason", "completed".to_owned())]);
        assert!(!root.join(LOG_DIR_NAME).join(LOG_FILE_NAME).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_host_log_fields_cannot_forge_extra_events() {
        let root = temp_dir("inject");
        let log = NativeHostLog::in_config_dir(&root);
        log.record(
            "connect",
            &[("extension_origin", "\nevent=forged".to_owned())],
        );
        let written = fs::read_to_string(log.path()).unwrap();
        assert_eq!(written.lines().count(), 1, "a field injected a second line");
        assert!(!written.contains(" event=forged"));
        assert!(written.contains("event=connect"));
        fs::remove_dir_all(root).unwrap();
    }
}
