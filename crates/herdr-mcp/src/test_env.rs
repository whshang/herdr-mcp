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
