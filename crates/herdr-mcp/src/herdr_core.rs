//! Read-only detection of the Herdr app / herdr-core runtime (`hc`).
//!
//! herdr-mcp drives only the Herdr TUI (`herdr` CLI + `herdr server`). The
//! Herdr app and its herdr-core runtime are a separate runtime that shares no
//! agents with the TUI, so a machine that has them installed gets an explicit,
//! non-fatal doctor warning (issue #765). This module only ever executes `hc
//! --version` with a short timeout; it never starts or attaches to a
//! herdr-core server and never mutates anything.

use crate::child_process;
use crate::locale::Locale;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// Stable machine-readable warning code surfaced in `doctor --json`.
pub(crate) const HERDR_CORE_WARNING_CODE: &str = "herdr_core_unsupported";

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_OUTPUT: usize = 4 * 1024;
const MAX_VERSION_CHARS: usize = 80;
const APP_CONFIG_DIR_NAME: &str = "herdr-app";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HerdrCoreProbe {
    pub present: bool,
    pub version: Option<String>,
    pub source: Option<&'static str>,
    pub binary: Option<PathBuf>,
}

/// Injectable process environment so tests need no real `hc` on the machine.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProbeEnv {
    pub path: Option<OsString>,
    pub home: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
}

impl ProbeEnv {
    fn from_process() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            home: std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        }
    }
}

pub(crate) fn probe() -> HerdrCoreProbe {
    probe_with_env(&ProbeEnv::from_process(), &run_version_probe)
}

pub(crate) fn probe_with_env(
    env: &ProbeEnv,
    run_version: &dyn Fn(&Path) -> Option<String>,
) -> HerdrCoreProbe {
    for (candidate, source) in binary_candidates(env) {
        if !is_executable_file(&candidate) {
            continue;
        }
        // Only `--version` is ever executed; a failed or slow probe still
        // reports presence without a version.
        let version = run_version(&candidate).and_then(sanitize_version);
        return HerdrCoreProbe {
            present: true,
            version,
            source: Some(source),
            binary: Some(candidate),
        };
    }
    if let Some(dir) = app_config_dir(env)
        && dir.is_dir()
    {
        return HerdrCoreProbe {
            present: true,
            version: None,
            source: Some("app-config"),
            binary: None,
        };
    }
    HerdrCoreProbe::default()
}

/// PATH entries first, then the stable install roots (mirroring the Herdr TUI
/// discovery set), then the app config marker directory.
fn binary_candidates(env: &ProbeEnv) -> Vec<(PathBuf, &'static str)> {
    let names: &[&str] = if cfg!(windows) {
        &["hc.exe", "hc.cmd", "hc", "herdr-core.exe", "herdr-core"]
    } else {
        &["hc", "herdr-core"]
    };
    let mut out = Vec::new();
    if let Some(path) = &env.path {
        for dir in std::env::split_paths(path) {
            for name in names {
                out.push((dir.join(name), "path"));
            }
        }
    }
    if let Some(home) = &env.home {
        for name in names {
            out.push((home.join(".local/bin").join(name), "install-location"));
        }
    }
    for root in [Path::new("/opt/homebrew/bin"), Path::new("/usr/local/bin")] {
        for name in names {
            out.push((root.join(name), "install-location"));
        }
    }
    out
}

fn app_config_dir(env: &ProbeEnv) -> Option<PathBuf> {
    if let Some(home) = &env.home {
        return Some(home.join(".config").join(APP_CONFIG_DIR_NAME));
    }
    env.local_app_data
        .as_ref()
        .map(|dir| dir.join(APP_CONFIG_DIR_NAME))
}

fn run_version_probe(binary: &Path) -> Option<String> {
    let mut command = std::process::Command::new(binary);
    command.arg("--version").stdin(Stdio::null());
    let output =
        child_process::run_bounded_output(&mut command, PROBE_TIMEOUT, MAX_OUTPUT).ok()??;
    if !output.status.success() || output.truncated {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.trim().is_empty() {
        Some(String::from_utf8_lossy(&output.stderr).into_owned())
    } else {
        Some(text)
    }
}

/// First line only, control characters stripped, bounded length. Nothing from
/// the probe output beyond the version string is ever surfaced.
fn sanitize_version(raw: String) -> Option<String> {
    let line = raw.lines().next().unwrap_or("").trim();
    let cleaned: String = line
        .chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_VERSION_CHARS)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_owned())
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub(crate) fn warning_line(probe: &HerdrCoreProbe, language: Locale) -> String {
    let detected = language.text(
        "herdr-core / Herdr app detected",
        "检测到 herdr-core / Herdr app",
        "herdr-core / Herdr app を検出",
    );
    let supports = language.text(
        "herdr-mcp v1.1 supports only the Herdr TUI (https://herdr.dev/tui/docs/); agents started in the app are not visible to herdr-mcp",
        "herdr-mcp v1.1 仅支持 Herdr TUI（https://herdr.dev/tui/docs/）；在 app 中启动的 Agent 对 herdr-mcp 不可见",
        "herdr-mcp v1.1 は Herdr TUI のみをサポート（https://herdr.dev/tui/docs/）。app で起動した Agent は herdr-mcp から見えません",
    );
    match &probe.version {
        Some(version) => {
            format!("WARN {HERDR_CORE_WARNING_CODE}: {detected} ({version}); {supports}")
        }
        None => format!("WARN {HERDR_CORE_WARNING_CODE}: {detected}; {supports}"),
    }
}

pub(crate) fn probe_json(probe: &HerdrCoreProbe) -> Value {
    json!({
        "present": probe.present,
        "version": probe.version,
        "source": probe.source,
        "binary": probe
            .binary
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
    })
}

pub(crate) fn warnings_json(probe: &HerdrCoreProbe) -> Value {
    if probe.present {
        json!([HERDR_CORE_WARNING_CODE])
    } else {
        json!([])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-herdr-core-{name}-{}-{nonce}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_executable(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn fake_runner(output: Option<&str>) -> impl Fn(&Path) -> Option<String> + '_ {
        move |_binary| output.map(str::to_owned)
    }

    #[test]
    fn tui_only_environment_has_no_warning() {
        let root = temp_root("tui-only");
        let env = ProbeEnv {
            path: Some(OsString::new()),
            home: Some(root.clone()),
            local_app_data: None,
        };
        let probe = probe_with_env(&env, &fake_runner(None));
        assert!(!probe.present);
        assert_eq!(probe.version, None);
        assert_eq!(probe.source, None);
        assert_eq!(warnings_json(&probe), json!([]));
        assert_eq!(probe_json(&probe)["present"], json!(false));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn hc_on_path_is_detected_and_versioned() {
        let root = temp_root("path-hc");
        let bin = root.join("bin");
        write_executable(&bin.join("hc"));
        let env = ProbeEnv {
            path: Some(bin.into_os_string()),
            home: Some(root.join("home")),
            local_app_data: None,
        };
        let probe = probe_with_env(&env, &fake_runner(Some("hc 0.1.14\n")));
        assert!(probe.present);
        assert_eq!(probe.version.as_deref(), Some("hc 0.1.14"));
        assert_eq!(probe.source, Some("path"));
        assert_eq!(warnings_json(&probe), json!(["herdr_core_unsupported"]));
        let warning = warning_line(&probe, Locale::En);
        assert!(warning.contains("herdr_core_unsupported"));
        assert!(warning.contains("hc 0.1.14"));
        assert!(warning.contains("https://herdr.dev/tui/docs/"));
        assert!(warning.contains("not visible to herdr-mcp"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stable_install_location_detected_without_path() {
        let root = temp_root("install-location");
        write_executable(&root.join(".local/bin/herdr-core"));
        let env = ProbeEnv {
            path: Some(OsString::new()),
            home: Some(root.clone()),
            local_app_data: None,
        };
        let probe = probe_with_env(&env, &fake_runner(Some("herdr-core 0.1.14")));
        assert!(probe.present);
        assert_eq!(probe.source, Some("install-location"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn app_config_dir_detected_without_binary() {
        let root = temp_root("app-config");
        fs::create_dir_all(root.join(".config/herdr-app")).unwrap();
        let env = ProbeEnv {
            path: Some(OsString::new()),
            home: Some(root.clone()),
            local_app_data: None,
        };
        let probe = probe_with_env(&env, &fake_runner(None));
        assert!(probe.present);
        assert_eq!(probe.source, Some("app-config"));
        assert_eq!(probe.version, None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn version_probe_failure_still_warns_without_version() {
        let root = temp_root("version-failure");
        let bin = root.join("bin");
        write_executable(&bin.join("hc"));
        let env = ProbeEnv {
            path: Some(bin.into_os_string()),
            home: Some(root.clone()),
            local_app_data: None,
        };
        let probe = probe_with_env(&env, &fake_runner(None));
        assert!(probe.present);
        assert_eq!(probe.version, None);
        let warning = warning_line(&probe, Locale::En);
        assert!(!warning.contains("()"));
        assert!(warning.contains(HERDR_CORE_WARNING_CODE));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn version_output_is_sanitized() {
        // take() runs before trim: 80 raw chars are kept, then trimmed.
        let long = format!("hc 0.1.14 extra{}", "x".repeat(200));
        let expected: String = long.chars().take(80).collect::<String>().trim().to_owned();
        assert_eq!(sanitize_version(long), Some(expected));
        assert_eq!(
            sanitize_version("hc 0.1.14 \u{7}extra\u{1f}\nsecond line".to_owned()),
            Some("hc 0.1.14 extra".to_owned())
        );
        assert_eq!(sanitize_version("   \n  ".to_owned()), None);
        assert_eq!(sanitize_version("\n".to_owned()), None);
    }

    #[test]
    fn non_executable_path_entry_is_ignored() {
        let root = temp_root("non-exec");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("hc"), b"not executable").unwrap();
        let env = ProbeEnv {
            path: Some(bin.clone().into_os_string()),
            home: Some(root.clone()),
            local_app_data: None,
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(bin.join("hc"), fs::Permissions::from_mode(0o644)).unwrap();
        }
        let probe = probe_with_env(&env, &fake_runner(Some("hc 0.1.14")));
        #[cfg(unix)]
        assert!(!probe.present);
        // On non-unix platforms a plain file counts as executable.
        #[cfg(not(unix))]
        assert!(probe.present);
        let _ = fs::remove_dir_all(&root);
    }
}
