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

    /// user cannot add an unguarded /tmp socket fixture | Given every function
    /// in the crate that binds a Unix socket under `/tmp` or `temp_dir()` |
    /// When the fixture audit runs | Then each one owns a `TempUnixSocketGuard`
    /// (#1007).
    #[test]
    fn user_cannot_add_unguarded_tmp_socket_fixtures() {
        let unguarded = audit_tmp_socket_fixtures();
        assert!(
            unguarded.is_empty(),
            "these fixtures bind a /tmp socket without a TempUnixSocketGuard: {unguarded:?}"
        );
    }

    /// user cannot regress the audit itself | Given synthetic fixtures covering
    /// the shapes the audit must catch | When the audit runs on each one | Then
    /// direct binds, `temp_dir()`-built paths, helper-based binds, nested-module
    /// indentation and the path-only opt-out all behave as documented (#1007).
    #[test]
    fn user_keeps_the_socket_fixture_audit_honest() {
        // Literal /tmp path, direct bind, no guard -> flagged.
        let flagged = audit_source(
            "literal.rs",
            "mod tests {\n    #[test]\n    fn fixture() {\n        let s = PathBuf::from(format!(\"/tmp/herdr-x-{}.sock\", 1));\n        UnixListener::bind(&s).unwrap();\n    }\n}\n",
        );
        assert_eq!(flagged.len(), 1, "{flagged:?}");

        // temp_dir()-built path, direct bind, guarded -> clean.
        let clean = audit_source(
            "tempdir.rs",
            "mod tests {\n    #[test]\n    fn fixture() {\n        let s = std::env::temp_dir().join(format!(\"herdr-y-{}.sock\", 1));\n        let _g = crate::test_env::TempUnixSocketGuard::file(&s);\n        UnixListener::bind(&s).unwrap();\n    }\n}\n",
        );
        assert!(clean.is_empty(), "{clean:?}");

        // temp_dir()-built path, direct bind, unguarded -> flagged.
        let flagged_tempdir = audit_source(
            "tempdir-unguarded.rs",
            "mod tests {\n    #[test]\n    fn fixture() {\n        let s = std::env::temp_dir().join(format!(\"herdr-z-{}.sock\", 1));\n        UnixListener::bind(&s).unwrap();\n    }\n}\n",
        );
        assert_eq!(flagged_tempdir.len(), 1, "{flagged_tempdir:?}");

        // Bind through a same-file helper, unguarded caller -> flagged.
        let flagged_helper = audit_source(
            "helper.rs",
            "mod tests {\n    fn spawn_fixture(path: &std::path::Path) {\n        std::os::unix::net::UnixListener::bind(path).unwrap();\n    }\n\n    #[test]\n    fn fixture() {\n        let s = PathBuf::from(format!(\"/tmp/herdr-h-{}.sock\", 1));\n        spawn_fixture(&s);\n    }\n}\n",
        );
        assert_eq!(flagged_helper.len(), 1, "{flagged_helper:?}");

        // The same helper-based fixture with a guard -> clean.
        let clean_helper = audit_source(
            "helper-guarded.rs",
            "mod tests {\n    fn spawn_fixture(path: &std::path::Path) {\n        std::os::unix::net::UnixListener::bind(path).unwrap();\n    }\n\n    #[test]\n    fn fixture() {\n        let s = PathBuf::from(format!(\"/tmp/herdr-hg-{}.sock\", 1));\n        let _g = crate::test_env::TempUnixSocketGuard::file(&s);\n        spawn_fixture(&s);\n    }\n}\n",
        );
        assert!(clean_helper.is_empty(), "{clean_helper:?}");

        // A same-file path helper + direct bind: the caller's body carries
        // neither `temp_dir()` nor `.sock`, so the helper itself must be
        // recognised (unguarded -> flagged, guarded -> clean).
        let helper_path_source = |guard: &str| {
            format!(
                "mod tests {{\n    fn peer_socket_path(tag: &str) -> PathBuf {{\n        std::env::temp_dir().join(format!(\"herdr-peer-{{tag}}-{{}}.sock\", 1))\n    }}\n\n    #[test]\n    fn fixture() {{\n        let s = peer_socket_path(\"x\");\n        {guard}UnixListener::bind(&s).unwrap();\n    }}\n}}\n"
            )
        };
        let flagged_path_helper = audit_source("path-helper.rs", &helper_path_source(""));
        assert_eq!(flagged_path_helper.len(), 1, "{flagged_path_helper:?}");
        let clean_path_helper = audit_source(
            "path-helper-guarded.rs",
            &helper_path_source(
                "let _g = crate::test_env::TempUnixSocketGuard::file(&s);\n        ",
            ),
        );
        assert!(clean_path_helper.is_empty(), "{clean_path_helper:?}");

        // The opt-out must not silence a function that really binds.
        let marker_with_bind = audit_source(
            "marker-and-bind.rs",
            "mod tests {\n    #[test]\n    fn fixture() {\n        // temp-socket-path-only: documented, but this body still binds.\n        let s = std::env::temp_dir().join(format!(\"herdr-mb-{}.sock\", 1));\n        UnixListener::bind(&s).unwrap();\n    }\n}\n",
        );
        assert_eq!(marker_with_bind.len(), 1, "{marker_with_bind:?}");

        // A path that is only used in an assertion/message may opt out.
        let opted_out = audit_source(
            "path-only.rs",
            "mod tests {\n    #[test]\n    fn fixture() {\n        // temp-socket-path-only: the path only appears in the assertion.\n        let s = PathBuf::from(format!(\"/tmp/herdr-p-{}.sock\", 1));\n        assert!(s.display().to_string().contains(\".sock\"));\n    }\n}\n",
        );
        assert!(opted_out.is_empty(), "{opted_out:?}");
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

    fn audit_tmp_socket_fixtures() -> Vec<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rust_files(&root, &mut files);
        let mut unguarded = Vec::new();
        for file in files {
            // The audit module itself names these shapes while describing them.
            if file.file_name().and_then(|name| name.to_str()) == Some("test_env.rs") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            for name in audit_source(&file.display().to_string(), &source) {
                unguarded.push(name);
            }
        }
        unguarded
    }

    /// Pure audit over one source file: a function that binds a Unix listener
    /// (directly or through a helper defined in the same file) at a socket path
    /// under `/tmp` or `temp_dir()` must own a `TempUnixSocketGuard`. Functions
    /// that only *name* such a path opt out with `// temp-socket-path-only`,
    /// because guarding a path this test does not create would be exactly the
    /// delete-someone-else's-socket bug the guard must not encourage.
    fn audit_source(label: &str, source: &str) -> Vec<String> {
        let functions = functions(source);
        let body_of = |wanted: &str| {
            functions
                .iter()
                .find(|(name, _)| name == wanted)
                .map(|(_, body)| body.as_str())
        };
        // `body` starts at the signature line, so a function containing its own
        // name (`fn spawn_fixture`) must not count as calling itself.
        let calls = |body: &str, helper: &str| {
            body.lines()
                .skip(1)
                .any(|line| line.contains(&format!("{helper}(")))
        };
        let binders: Vec<&str> = functions
            .iter()
            .filter(|(_, body)| binds_unix_listener(body))
            .map(|(name, _)| name.as_str())
            .collect();
        let path_helpers: Vec<&str> = functions
            .iter()
            .filter(|(_, body)| constructs_tmp_socket_path(body))
            .map(|(name, _)| name.as_str())
            .collect();
        // A helper that already owns the cleanup (`TempUnixSocketGuard` in its
        // own body, usually returned to the caller) makes its callers safe.
        let guarded_helper =
            |name: &str| body_of(name).is_some_and(|body| body.contains("TempUnixSocketGuard"));
        let mut unguarded = Vec::new();
        for (name, body) in &functions {
            let binds_here =
                binds_unix_listener(body) || binders.iter().any(|binder| calls(body, binder));
            // The opt-out only covers a function that really does not create
            // anything: a marker next to a live bind must not silence it.
            if !binds_here && body.contains("temp-socket-path-only") {
                continue;
            }
            let constructs_here = constructs_tmp_socket_path(body)
                || path_helpers.iter().any(|helper| calls(body, helper));
            if !binds_here || !constructs_here {
                continue;
            }
            if body.contains("TempUnixSocketGuard") {
                continue;
            }
            // A caller whose socket is created and cleaned by a guarded helper
            // (the helper returns the guard to the caller) needs no guard of its
            // own.
            let binding_helper_is_guarded = binders
                .iter()
                .filter(|binder| calls(body, binder))
                .all(|binder| guarded_helper(binder));
            if !binds_unix_listener(body) && binding_helper_is_guarded {
                continue;
            }
            unguarded.push(format!("{label}::{name}"));
        }
        unguarded
    }

    fn binds_unix_listener(body: &str) -> bool {
        body.contains("UnixListener::bind")
    }

    /// A socket path this fixture creates: a `/tmp/…sock` construction (not a
    /// config JSON string, which only names one) or a `temp_dir()`-derived
    /// `…sock` path. Multi-line `format!` calls are covered by the second arm.
    fn constructs_tmp_socket_path(body: &str) -> bool {
        let literal = body.lines().any(|line| {
            line.contains(".sock")
                && !line.contains("br#\"")
                && !line.contains("Some(\"")
                && (line.contains("/tmp/") && line.contains("format!(")
                    || line.trim_start().starts_with("\"/tmp/")
                    || (line.contains("/tmp/")
                        && (line.contains("PathBuf::from") || line.contains(".join("))))
        });
        literal
            || (body.contains("temp_dir()")
                && body.contains(".sock")
                && (body.contains("format!(") || body.contains(".join(")))
    }

    /// Split a source file into function bodies by indentation, which is
    /// exactly what rustfmt guarantees for this crate. Brace-depth scanning was
    /// fragile here: braces inside string literals (`"{"`) and char literals
    /// drift the depth and silently mis-attribute a body, which would make the
    /// audit claim coverage it does not have.
    fn functions(source: &str) -> Vec<(String, String)> {
        const PREFIXES: [&str; 5] = [
            "fn ",
            "pub fn ",
            "pub(crate) fn ",
            "pub(super) fn ",
            "async fn ",
        ];
        let lines: Vec<&str> = source.lines().collect();
        let mut functions = Vec::new();
        let mut index = 0usize;
        while index < lines.len() {
            let line = lines[index];
            let trimmed = line.trim_start();
            let Some(rest) = PREFIXES
                .iter()
                .find_map(|prefix| trimmed.strip_prefix(prefix))
            else {
                index += 1;
                continue;
            };
            let name = rest
                .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .next()
                .unwrap_or_default()
                .to_owned();
            let indent = line.len() - trimmed.len();
            let mut end = index + 1;
            while end < lines.len() {
                let candidate = lines[end];
                let candidate_trimmed = candidate.trim_start();
                let candidate_indent = candidate.len() - candidate_trimmed.len();
                if candidate_trimmed.starts_with('}') && candidate_indent <= indent {
                    break;
                }
                end += 1;
            }
            let last = end.min(lines.len().saturating_sub(1));
            functions.push((name, lines[index..=last].join("\n")));
            index = last + 1;
        }
        functions
    }
}
