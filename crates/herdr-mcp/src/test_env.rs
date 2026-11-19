use std::ffi::OsString;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Serialize tests that mutate process-global environment variables.
///
/// Rust's test harness runs modules concurrently inside one process. Every test
/// that calls `std::env::set_var` or `remove_var` must hold this crate-wide
/// guard for the complete mutation/restore window.
pub(crate) fn lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// #936: the backend a default test runtime admitted at startup.
///
/// Test routers carry this unless the test explicitly adopts another backend,
/// so an unrelated test's env window cannot silently change a router's pin.
pub(crate) fn tui_backend_selection() -> crate::runtime_adapter::RuntimeBackendSelection {
    crate::runtime_adapter::RuntimeBackendSelection {
        backend: crate::runtime_adapter::RuntimeBackend::Tui,
        source: "test",
        herdr_core_socket: None,
    }
}

/// #936: the selection a fresh runtime process would admit from the ambient
/// env/config, for tests that adopt a non-default backend. Call it while
/// holding [`lock`], because only the env-mutating tests change the selection.
/// A failed read stays a failure; it must not invent a TUI pin.
pub(crate) fn admitted_backend_selection() -> crate::runtime_adapter::RuntimeBackendSelection {
    crate::runtime_adapter::resolve_process_backend_selection()
        .expect("test runtime backend selection")
}

/// #768: the admission inputs a default router fixture pins.
///
/// The fixture writes its own `tui` config under its temp root and pins that
/// path, so a concurrent test's `HERDR_MCP_CONFIG_DIR` or backend mutation
/// cannot redirect this router's drift check.
pub(crate) fn pinned_tui_admission(root: &Path) -> crate::runtime_adapter::AdmittedBackendConfig {
    let config_file = root.join("config.json");
    std::fs::create_dir_all(root).expect("fixture config dir");
    std::fs::write(&config_file, br#"{"runtime":{"backend":"tui"}}"#).expect("fixture config");
    crate::runtime_adapter::AdmittedBackendConfig::from_parts(
        config_file,
        crate::instance::InstanceId::default_instance(),
        crate::runtime_adapter::EnvOverrides::default(),
    )
}

/// #768: the admission inputs for a test that deliberately adopts the backend
/// its own env window selects. Call it while holding [`lock`].
pub(crate) fn ambient_admission() -> crate::runtime_adapter::AdmittedBackendConfig {
    let paths = crate::paths::RuntimePaths::discover().expect("test runtime paths");
    crate::runtime_adapter::AdmittedBackendConfig::capture(&paths)
        .expect("test admitted backend config")
}

/// Points process admission at a TUI config under `dir` until dropped.
///
/// Direct `tools/call` tests pin [`tui_backend_selection`] and must resolve
/// the same backend. Restoring on drop keeps a later test from observing this
/// config, including when an assertion fails. Hold [`lock`] across the guard.
pub(crate) struct TuiConfigGuard {
    previous_config_dir: Option<OsString>,
    previous_backend: Option<OsString>,
}

impl TuiConfigGuard {
    pub(crate) fn install(dir: &Path) -> Self {
        let previous_config_dir = std::env::var_os("HERDR_MCP_CONFIG_DIR");
        let previous_backend = std::env::var_os("HERDR_MCP_RUNTIME_BACKEND");
        let config_dir = dir.join("config");
        std::fs::create_dir_all(&config_dir).expect("tui config dir");
        std::fs::write(
            config_dir.join("config.json"),
            br#"{"runtime":{"backend":"tui"}}"#,
        )
        .expect("tui config");
        unsafe {
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &config_dir);
            std::env::remove_var("HERDR_MCP_RUNTIME_BACKEND");
        }
        Self {
            previous_config_dir,
            previous_backend,
        }
    }
}

impl Drop for TuiConfigGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous_config_dir.take() {
                Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
            match self.previous_backend.take() {
                Some(value) => std::env::set_var("HERDR_MCP_RUNTIME_BACKEND", value),
                None => std::env::remove_var("HERDR_MCP_RUNTIME_BACKEND"),
            }
        }
    }
}

/// #1007: cleanup guard for Unix socket files that tests bind under
/// `env::temp_dir()` or `/tmp` to stay below `SUN_LEN`.
///
/// `Drop` removes the socket file, ignoring `NotFound` so tests that already
/// unlink the socket themselves keep passing. `dir` removes a directory the
/// test created only to hold a short socket path.
pub(crate) struct TempUnixSocketGuard {
    path: std::path::PathBuf,
    remove_dir: bool,
}

impl TempUnixSocketGuard {
    /// Guard a socket file: `Drop` runs `fs::remove_file`, ignoring `NotFound`.
    pub(crate) fn file(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: path.into(),
            remove_dir: false,
        }
    }

    /// Guard a directory created solely for socket files: `Drop` runs
    /// `fs::remove_dir_all`, ignoring `NotFound`.
    pub(crate) fn dir(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: path.into(),
            remove_dir: true,
        }
    }
}

impl Drop for TempUnixSocketGuard {
    fn drop(&mut self) {
        let result = if self.remove_dir {
            std::fs::remove_dir_all(&self.path)
        } else {
            std::fs::remove_file(&self.path)
        };
        if let Err(error) = result {
            // `NotFound` is fine (the test may have cleaned up itself); every
            // other removal failure stays non-fatal so teardown never masks
            // the real assertion that failed first.
            let _ = error;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TempUnixSocketGuard;
    use std::path::{Path, PathBuf};

    fn short_temp_path(name: &str) -> PathBuf {
        // `/tmp` keeps sockets below SUN_LEN, exactly like the real fixtures.
        std::env::temp_dir().join(format!(
            "herdr-guard-{}-{}-{name}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    /// user keeps /tmp clean through a failing test | Given a bound socket file
    /// guarded by `TempUnixSocketGuard` | When the owning test panics | Then the
    /// socket is removed during unwind instead of being left behind (#1007).
    #[test]
    fn user_keeps_tmp_clean_when_a_test_panics() {
        let socket = short_temp_path("panic");
        std::fs::write(&socket, b"").unwrap();
        let guard = TempUnixSocketGuard::file(&socket);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = guard;
            assert!(socket.exists(), "the socket must exist before the panic");
            panic!("simulated test failure");
        }));
        assert!(outcome.is_err(), "the simulated panic must propagate");
        assert!(
            !socket.exists(),
            "Drop must remove the socket while unwinding a failed test"
        );
    }

    /// user keeps foreign sockets | Given a guarded socket next to an unrelated
    /// socket | When the guard is dropped | Then only the guarded file is
    /// removed, so a test can never delete another process's socket (#1007).
    #[test]
    fn user_keeps_foreign_sockets_untouched() {
        let owned = short_temp_path("owned");
        let foreign = short_temp_path("foreign");
        std::fs::write(&owned, b"").unwrap();
        std::fs::write(&foreign, b"").unwrap();
        {
            let _guard = TempUnixSocketGuard::file(&owned);
        }
        assert!(!owned.exists(), "the guarded socket is removed");
        assert!(foreign.exists(), "an unrelated socket must survive");
        let _ = std::fs::remove_file(&foreign);
    }

    /// user cannot add an unguarded /tmp socket fixture | Given every test
    /// function in the crate that binds a socket under /tmp | When the fixture
    /// audit runs | Then each one owns a `TempUnixSocketGuard` (#1007).
    #[test]
    fn user_cannot_add_unguarded_tmp_socket_fixtures() {
        let unguarded = audit_tmp_socket_fixtures();
        assert!(
            unguarded.is_empty(),
            "these test fixtures bind a /tmp socket without a TempUnixSocketGuard: {unguarded:?}"
        );
    }

    fn audit_tmp_socket_fixtures() -> Vec<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rust_files(&root, &mut files);
        let mut unguarded = Vec::new();
        for file in files {
            // This audit itself names `/tmp/…sock` while describing the rule.
            if file.file_name().and_then(|name| name.to_str()) == Some("test_env.rs") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            for (name, body) in test_functions(&source) {
                if !binds_tmp_socket(&body) {
                    continue;
                }
                if !body.contains("TempUnixSocketGuard") {
                    unguarded.push(format!("{}::{name}", file.display()));
                }
            }
        }
        unguarded
    }

    fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rust_files(&path, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// Split a source file into `fn` bodies so a guard in one test cannot be
    /// credited to another test that shares the file.
    fn test_functions(source: &str) -> Vec<(String, String)> {
        let mut functions = Vec::new();
        let mut current: Option<(String, String)> = None;
        for line in source.lines() {
            let trimmed = line.trim_start();
            let starts_fn = line.starts_with("    fn ") || line.starts_with("    pub fn ");
            if starts_fn {
                if let Some(done) = current.take() {
                    functions.push(done);
                }
                let name = trimmed
                    .split("fn ")
                    .nth(1)
                    .and_then(|rest| rest.split('(').next())
                    .unwrap_or("unknown")
                    .to_owned();
                current = Some((name, String::new()));
            } else if line == "    }"
                && let Some(done) = current.take()
            {
                functions.push(done);
            }
            if let Some((_, body)) = current.as_mut() {
                body.push_str(line);
                body.push('\n');
            }
        }
        functions.extend(current);
        functions
    }

    /// Fixtures build their socket path with `format!("/tmp/…sock")` or
    /// `PathBuf::from(format!("/tmp/…"))`. Config JSON strings and pure
    /// resolution assertions that only *name* a socket are not bindings (and
    /// must not be guarded: a guard on a foreign path would delete a socket
    /// another test owns), so they stay out of scope. The audit needs both the
    /// `/tmp/…sock` literal and a `format!`-built path in the same function.
    fn binds_tmp_socket(body: &str) -> bool {
        body.contains("format!(")
            && body.lines().any(|line| {
                line.contains("\"/tmp/") && line.contains(".sock") && !line.contains("br#\"")
            })
    }
}
