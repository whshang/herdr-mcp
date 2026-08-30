//! User-facing macOS permission lifecycle for the unsigned stable TCC broker.
//!
//! Developer ID signing is optional hardening, not a v0.4.2 gate. The stable
//! identity is the installed broker at `<config_dir>/tcc-broker/herdr-mcp-broker`.
//! `setup` may open Privacy & Security settings and must never claim to grant
//! permission.

use crate::tcc_broker;
use serde_json::{Value, json};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(any(target_os = "macos", test))]
const HINT_SETUP: &str = "run `herdr-mcp permissions setup`";
const HINT_DENIED: &str = "enable Files and Folders or Full Disk Access for herdr-mcp-broker, then `herdr-mcp permissions verify`";
#[cfg(any(target_os = "macos", test))]
const HINT_TIMEOUT: &str =
    "probe timed out (2s); finish the macOS prompt, then `herdr-mcp permissions verify`";
#[cfg(any(target_os = "macos", test))]
const HINT_UNKNOWN: &str = "re-run `herdr-mcp permissions verify`";
#[cfg(any(target_os = "macos", test))]
const HINT_GRANTED: &str = "protected path readable";
#[cfg(any(not(target_os = "macos"), test))]
const HINT_NOT_APPLICABLE: &str = "macOS only";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermissionState {
    #[cfg(any(target_os = "macos", test))]
    Granted,
    #[cfg(any(target_os = "macos", test))]
    Denied,
    #[cfg(any(target_os = "macos", test))]
    NeedsSetup,
    #[cfg(any(target_os = "macos", test))]
    Unknown,
    #[cfg(any(target_os = "macos", test))]
    Timeout,
    #[cfg(any(not(target_os = "macos"), test))]
    NotApplicable,
}

impl PermissionState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            #[cfg(any(target_os = "macos", test))]
            Self::Granted => "granted",
            #[cfg(any(target_os = "macos", test))]
            Self::Denied => "denied",
            #[cfg(any(target_os = "macos", test))]
            Self::NeedsSetup => "needs_setup",
            #[cfg(any(target_os = "macos", test))]
            Self::Unknown => "unknown",
            #[cfg(any(target_os = "macos", test))]
            Self::Timeout => "timeout",
            #[cfg(any(not(target_os = "macos"), test))]
            Self::NotApplicable => "not_applicable",
        }
    }

    #[cfg(any(target_os = "macos", test))]
    fn hint(self) -> &'static str {
        match self {
            Self::Granted => HINT_GRANTED,
            Self::Denied => HINT_DENIED,
            Self::NeedsSetup => HINT_SETUP,
            Self::Unknown => HINT_UNKNOWN,
            Self::Timeout => HINT_TIMEOUT,
            #[cfg(test)]
            Self::NotApplicable => HINT_NOT_APPLICABLE,
        }
    }

    fn doctor_pass(self) -> bool {
        #[cfg(any(target_os = "macos", test))]
        {
            !matches!(self, Self::Denied | Self::Timeout)
        }
        #[cfg(all(not(target_os = "macos"), not(test)))]
        {
            true
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PermissionReport {
    pub(crate) state: PermissionState,
    pub(crate) broker_installed: bool,
    pub(crate) broker_path: PathBuf,
    pub(crate) broker_update_available: bool,
    pub(crate) probe: &'static str,
    pub(crate) hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerSync {
    pub path: PathBuf,
    pub installed: bool,
    pub broker_update_available: bool,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionsCommand {
    Status,
    Setup { upgrade_broker: bool },
    Verify,
}

pub fn run_cli(command: PermissionsCommand) -> Result<ExitCode, String> {
    match command {
        PermissionsCommand::Status => {
            print_status(&collect_status()?);
            Ok(ExitCode::SUCCESS)
        }
        PermissionsCommand::Setup { upgrade_broker } => run_setup(upgrade_broker),
        PermissionsCommand::Verify => run_verify(),
    }
}

pub(crate) fn collect_status() -> Result<PermissionReport, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let config_dir = crate::paths::RuntimePaths::discover()?.config_dir;
        let path = tcc_broker::broker_path(&config_dir);
        Ok(PermissionReport {
            state: PermissionState::NotApplicable,
            broker_installed: tcc_broker::status(&path).is_some(),
            broker_path: path,
            broker_update_available: false,
            probe: "skipped",
            hint: HINT_NOT_APPLICABLE.to_owned(),
        })
    }

    #[cfg(target_os = "macos")]
    {
        let config_dir = crate::paths::RuntimePaths::discover()?.config_dir;
        let path = tcc_broker::broker_path(&config_dir);
        let installed = tcc_broker::status(&path).is_some();
        let update_available = broker_update_available(&config_dir);
        if !installed {
            return Ok(PermissionReport {
                state: PermissionState::NeedsSetup,
                broker_installed: false,
                broker_path: path,
                broker_update_available: false,
                probe: "skipped",
                hint: HINT_SETUP.to_owned(),
            });
        }
        let (state, probe) = classify_probe(probe_protected_path(&path));
        let hint = state.hint().to_owned();
        Ok(PermissionReport {
            state,
            broker_installed: true,
            broker_path: path,
            broker_update_available: update_available,
            probe,
            hint,
        })
    }
}

pub(crate) fn doctor_layer_from(report: &Result<PermissionReport, String>) -> String {
    match report {
        Ok(report) => format!(
            "LAYER macos-permissions status={} broker={} update_available={} probe={} hint={}",
            report.state.as_str(),
            if report.broker_installed {
                "installed"
            } else {
                "missing"
            },
            report.broker_update_available,
            report.probe,
            report.hint
        ),
        Err(error) => format!("LAYER macos-permissions status=unknown broker=unknown hint={error}"),
    }
}

pub(crate) fn report_doctor_pass(report: &PermissionReport) -> bool {
    #[cfg(target_os = "macos")]
    {
        report.state.doctor_pass()
    }
    #[cfg(not(target_os = "macos"))]
    {
        report.state.doctor_pass()
    }
}

fn print_status(report: &PermissionReport) {
    println!("status: {}", report.state.as_str());
    println!("broker: {}", report.broker_path.display());
    println!("broker_installed: {}", report.broker_installed);
    println!(
        "broker_update_available: {}",
        report.broker_update_available
    );
    if let Some(config_dir) = report.broker_path.parent().and_then(Path::parent) {
        let upgrade = tcc_broker::upgrade_status(config_dir);
        println!(
            "broker_installed_revision: {}",
            upgrade
                .installed_revision
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_owned())
        );
        println!("broker_candidate_revision: {}", upgrade.candidate_revision);
        println!(
            "broker_identity_compatible: {}",
            upgrade
                .identity_compatible
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned())
        );
        println!(
            "broker_update_requires_reauthorization: {}",
            upgrade.update_requires_reauthorization
        );
    }
    println!("probe: {}", report.probe);
    println!("hint: {}", report.hint);
}

fn run_setup(upgrade_broker: bool) -> Result<ExitCode, String> {
    let config_dir = crate::paths::RuntimePaths::discover()?.config_dir;
    let upgrade = tcc_broker::upgrade_status(&config_dir);
    if upgrade_broker && upgrade.update_available {
        tcc_broker::install(&config_dir, true)?;
    }
    let sync = preserve_or_install_broker(&config_dir)?;
    println!("broker: {}", sync.path.display());
    println!("broker_installed: {}", sync.installed);
    println!("broker_update_available: {}", sync.broker_update_available);
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os("HERDR_MCP_PERMISSIONS_DRY_RUN").is_some() {
            println!("opened: skipped");
        } else {
            match open_privacy_settings() {
                Ok(()) => println!("opened: Privacy & Security"),
                Err(error) => {
                    println!("opened: false");
                    println!("settings_open_failed: {error}");
                }
            }
        }
        println!(
            "hint: this does not grant permission; grant access, then `herdr-mcp permissions verify`"
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        println!("status: not_applicable");
        println!("hint: {HINT_NOT_APPLICABLE}");
    }
    Ok(ExitCode::SUCCESS)
}

fn run_verify() -> Result<ExitCode, String> {
    #[cfg(not(target_os = "macos"))]
    {
        println!("status: not_applicable");
        println!("hint: {HINT_NOT_APPLICABLE}");
        Ok(ExitCode::SUCCESS)
    }

    #[cfg(target_os = "macos")]
    {
        let report = collect_status()?;
        print_status(&report);
        // TCC verification must not make the rotating runtime (or a git child
        // spawned by it) the responsible client. The broker probe validates
        // read_dir + W_OK on ~/Documents without mutation. `herdr_git` is
        // separately routed through the broker during real MCP use.
        println!("git_common_dir: skipped (broker-owned MCP path)");
        println!("git_status: skipped (verify with herdr_git after setup)");
        println!(
            "write_gate: {}",
            if report.state == PermissionState::Granted {
                "granted (broker probe)"
            } else {
                "skipped"
            }
        );
        let ok = report.state == PermissionState::Granted;
        Ok(if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(2)
        })
    }
}

/// Install the broker when missing. An existing broker is a long-lived TCC
/// identity: ordinary runtime byte changes never replace it, and update
/// availability is driven only by the broker compatibility revision.
pub fn preserve_or_install_broker(config_dir: &Path) -> Result<BrokerSync, String> {
    let path = tcc_broker::broker_path(config_dir);
    if tcc_broker::status(&path).is_none() {
        tcc_broker::install(config_dir, false)?;
        let info = tcc_broker::status(&path);
        return Ok(BrokerSync {
            path,
            installed: true,
            broker_update_available: false,
            sha256: info.map(|info| info.sha256),
        });
    }
    let info = tcc_broker::status(&path);
    Ok(BrokerSync {
        path,
        installed: true,
        broker_update_available: broker_update_available(config_dir),
        sha256: info.map(|info| info.sha256),
    })
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn annotate_service_result(result: &mut Value, config_dir: &Path) {
    match preserve_or_install_broker(config_dir) {
        Ok(sync) => {
            if let Some(object) = result.as_object_mut() {
                object.insert(
                    "broker_path".to_owned(),
                    json!(sync.path.to_string_lossy().into_owned()),
                );
                object.insert("broker_installed".to_owned(), json!(sync.installed));
                object.insert(
                    "broker_update_available".to_owned(),
                    json!(sync.broker_update_available),
                );
                if let Some(sha256) = sync.sha256 {
                    object.insert("broker_sha256".to_owned(), json!(sha256));
                }
            }
        }
        Err(error) => {
            if let Some(object) = result.as_object_mut() {
                object.insert(
                    "broker_path".to_owned(),
                    json!(
                        tcc_broker::broker_path(config_dir)
                            .to_string_lossy()
                            .into_owned()
                    ),
                );
                object.insert("broker_installed".to_owned(), json!(false));
                object.insert("broker_update_available".to_owned(), json!(false));
                object.insert("broker_error".to_owned(), json!(error));
            }
        }
    }
}

pub fn map_fs_git_result(mut value: Value) -> Value {
    if value.get("ok").and_then(Value::as_bool) != Some(false) {
        return value;
    }
    if value.get("code").and_then(Value::as_str) == Some("macos_tcc_access_blocked")
        || value.get("reason").and_then(Value::as_str) == Some("macos_tcc_access_blocked")
    {
        return value;
    }
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| value.get("stderr").and_then(Value::as_str))
        .unwrap_or("");
    let code = value.get("code").and_then(Value::as_str).unwrap_or("");
    if (is_protected_timeout(code, message) || is_permission_denied_text(message))
        && let Some(object) = value.as_object_mut()
    {
        object.insert("ok".to_owned(), json!(false));
        object.insert("code".to_owned(), json!("macos_tcc_access_blocked"));
        object.insert("reason".to_owned(), json!("macos_tcc_access_blocked"));
        object.insert("hint".to_owned(), json!(HINT_DENIED));
    }
    value
}

pub fn io_error_to_fs_value(fallback_reason: &str, path: &Path, error: std::io::Error) -> Value {
    if is_permission_denied_io(&error) {
        tcc_blocked_value(path, error.to_string())
    } else {
        json!({
            "ok": false,
            "reason": fallback_reason,
            "path": path.to_string_lossy(),
            "message": error.to_string(),
        })
    }
}

pub fn io_message_to_fs_value(fallback_reason: &str, path: &Path, message: String) -> Value {
    if is_permission_denied_text(&message) {
        tcc_blocked_value(path, message)
    } else {
        json!({
            "ok": false,
            "reason": fallback_reason,
            "path": path.to_string_lossy(),
            "message": message,
        })
    }
}

pub fn git_failure_to_value(root: &Path, action: &str, message: String) -> Value {
    if is_protected_timeout("", &message) || is_permission_denied_text(&message) {
        let mut value = tcc_blocked_value(root, message);
        if let Some(object) = value.as_object_mut() {
            object.insert("root".to_owned(), json!(root.to_string_lossy()));
            object.insert("action".to_owned(), json!(action));
        }
        value
    } else {
        json!({
            "ok": false,
            "root": root.to_string_lossy(),
            "action": action,
            "message": message,
        })
    }
}

pub fn is_permission_denied_io(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::PermissionDenied
        || error.raw_os_error() == Some(libc::EPERM)
        || error.raw_os_error() == Some(libc::EACCES)
}

fn is_permission_denied_text(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("operation not permitted")
        || lower.contains("permission denied")
        || lower.contains("eperm")
}

fn is_protected_timeout(code: &str, message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    (code == "broker_failed" && lower.contains("timed out"))
        || lower.contains("broker request timed out")
        || lower.contains("protected-path probe timed out")
}

fn tcc_blocked_value(path: &Path, message: String) -> Value {
    json!({
        "ok": false,
        "code": "macos_tcc_access_blocked",
        "reason": "macos_tcc_access_blocked",
        "path": path.to_string_lossy(),
        "message": message,
        "hint": HINT_DENIED,
    })
}

fn broker_update_available(config_dir: &Path) -> bool {
    tcc_broker::upgrade_status(config_dir).update_available
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeOutcome {
    Granted,
    Denied,
    NotPresent,
    Timeout,
    Failed,
}

#[cfg(target_os = "macos")]
fn probe_protected_path(broker: &Path) -> ProbeOutcome {
    use crate::child_process;
    use std::process::{Command, Stdio};

    let executable = if broker.is_file() {
        broker.to_path_buf()
    } else {
        return ProbeOutcome::Failed;
    };
    let mut command = Command::new(executable);
    command
        .arg("__documents-probe")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    child_process::configure_process_group(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return ProbeOutcome::Failed,
    };
    let _registration = child_process::register_owned_child("macos-permissions-probe", &child);
    match child_process::wait_bounded(&mut child, PROBE_TIMEOUT) {
        Ok(Some(status)) if status.success() => ProbeOutcome::Granted,
        Ok(Some(status)) if status.code() == Some(77) => ProbeOutcome::Denied,
        Ok(Some(status)) if status.code() == Some(66) => ProbeOutcome::NotPresent,
        Ok(Some(_)) => ProbeOutcome::Failed,
        Ok(None) => ProbeOutcome::Timeout,
        Err(_) => ProbeOutcome::Failed,
    }
}

#[cfg(target_os = "macos")]
fn classify_probe(outcome: ProbeOutcome) -> (PermissionState, &'static str) {
    match outcome {
        ProbeOutcome::Granted => (PermissionState::Granted, "ok"),
        ProbeOutcome::Denied => (PermissionState::Denied, "denied"),
        ProbeOutcome::NotPresent => (PermissionState::Granted, "documents_absent"),
        ProbeOutcome::Timeout => (PermissionState::Timeout, "timeout"),
        ProbeOutcome::Failed => (PermissionState::Unknown, "failed"),
    }
}

#[cfg(target_os = "macos")]
fn open_privacy_settings() -> Result<(), String> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders")
        .status()
        .map_err(|error| format!("cannot open Privacy & Security: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("open exited {}", status.code().unwrap_or(-1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-macos-permissions-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn states_have_actionable_hints() {
        for state in [
            PermissionState::Granted,
            PermissionState::Denied,
            PermissionState::NeedsSetup,
            PermissionState::Unknown,
            PermissionState::Timeout,
        ] {
            assert!(!state.hint().is_empty(), "{}", state.as_str());
            assert!(!state.hint().to_ascii_lowercase().contains("granted you"));
        }
        assert!(PermissionState::Denied.hint().contains("Full Disk Access"));
        assert!(
            PermissionState::NeedsSetup
                .hint()
                .contains("permissions setup")
        );
        assert!(PermissionState::Timeout.hint().contains("2s"));
        assert!(
            !PermissionState::NeedsSetup
                .hint()
                .contains("has been granted")
        );
        assert!(PermissionState::Granted.doctor_pass());
        assert!(!PermissionState::Denied.doctor_pass());
        assert!(!PermissionState::Timeout.doctor_pass());
        assert!(PermissionState::NeedsSetup.doctor_pass());
        assert_eq!(PermissionState::NotApplicable.as_str(), "not_applicable");
        assert!(PermissionState::NotApplicable.doctor_pass());
    }

    #[test]
    fn doctor_permission_layer_tracks_broker_access_only() {
        let report = PermissionReport {
            state: PermissionState::Granted,
            broker_installed: true,
            broker_path: PathBuf::from("/tmp/herdr-mcp-broker"),
            broker_update_available: true,
            probe: "ok",
            hint: "protected path readable".to_owned(),
        };
        assert!(report_doctor_pass(&report));
        let layer = doctor_layer_from(&Ok(report));
        assert!(layer.contains("status=granted"));
        assert!(layer.contains("update_available=true"));
        assert!(!layer.contains("exec_host"));
    }

    #[test]
    fn maps_eperm_and_timeout_but_not_not_found() {
        let path = Path::new("/tmp/example");
        let denied = std::io::Error::from_raw_os_error(libc::EPERM);
        let value = io_error_to_fs_value("read_failed", path, denied);
        assert_eq!(value["code"], "macos_tcc_access_blocked");
        assert_eq!(value["reason"], "macos_tcc_access_blocked");

        let missing = std::io::Error::from(ErrorKind::NotFound);
        let value = io_error_to_fs_value("read_failed", path, missing);
        assert_eq!(value["reason"], "read_failed");
        assert_ne!(value["code"], "macos_tcc_access_blocked");

        let timed_out = json!({
            "ok": false,
            "code": "broker_failed",
            "message": "broker request timed out"
        });
        let mapped = map_fs_git_result(timed_out);
        assert_eq!(mapped["code"], "macos_tcc_access_blocked");

        let unrelated = json!({
            "ok": false,
            "reason": "outside_managed_roots",
            "message": "not in a project"
        });
        let mapped = map_fs_git_result(unrelated);
        assert_eq!(mapped["reason"], "outside_managed_roots");
        assert_ne!(
            mapped.get("code").and_then(Value::as_str),
            Some("macos_tcc_access_blocked")
        );
    }

    #[test]
    fn git_timeout_without_permission_text_is_not_tcc() {
        let value = git_failure_to_value(
            Path::new("/tmp/repo"),
            "status",
            "git command timed out after 15000ms".to_owned(),
        );
        assert_eq!(value["message"], "git command timed out after 15000ms");
        assert_ne!(value["code"], "macos_tcc_access_blocked");

        let value = git_failure_to_value(
            Path::new("/tmp/repo"),
            "status",
            "fatal: Operation not permitted".to_owned(),
        );
        assert_eq!(value["code"], "macos_tcc_access_blocked");
    }

    #[test]
    fn chmod_zero_file_is_permission_denied_not_not_found() {
        let dir = temp_dir("chmod");
        let file = dir.join("secret.txt");
        fs::write(&file, "hello").unwrap();
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&file, permissions).unwrap();
        let error = fs::read(&file).unwrap_err();
        assert!(is_permission_denied_io(&error) || error.kind() == ErrorKind::PermissionDenied);
        let value = io_error_to_fs_value("read_failed", &file, error);
        assert_eq!(value["code"], "macos_tcc_access_blocked");
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&file, permissions).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn preserve_or_install_ignores_runtime_byte_changes_at_same_broker_revision() {
        let dir = temp_dir("preserve");
        let config = dir.join("config");
        let target = tcc_broker::broker_path(&config);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"existing-stable-broker").unwrap();
        let sync = preserve_or_install_broker(&config).unwrap();
        assert!(!sync.broker_update_available);
        assert_eq!(
            tcc_broker::installed_compat_revision(&config),
            Some(tcc_broker::BROKER_COMPAT_REVISION)
        );
        assert_eq!(fs::read(&target).unwrap(), b"existing-stable-broker");
        let _ = fs::remove_dir_all(&dir);
    }
}
