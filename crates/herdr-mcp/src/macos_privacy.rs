use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
pub(crate) const STABLE_CODE_IDENTIFIER: &str = "dev.herdr.mcp";
#[cfg(target_os = "macos")]
const DOCUMENTS_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(target_os = "macos")]
const DOCUMENTS_PERMISSION_DENIED_EXIT: u8 = 77;
#[cfg(target_os = "macos")]
const DOCUMENTS_NOT_PRESENT_EXIT: u8 = 66;

// Variants are intentionally platform-specific: macOS constructs the probe states,
// while other targets construct only NotApplicable.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DocumentsPermission {
    Available,
    Unavailable,
    TimedOut,
    NotPresent,
    ProbeFailed(String),
    NotApplicable,
}

impl DocumentsPermission {
    /// Machine-readable token shared by `doctor`, `update status` and the
    /// persisted preflight so every surface names the same state.
    pub(crate) fn state_token(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable => "unavailable",
            Self::TimedOut => "pending",
            Self::NotPresent => "not_present",
            Self::ProbeFailed(_) => "probe_failed",
            Self::NotApplicable => "not_applicable",
        }
    }

    /// Whether a macOS Documents prompt is outstanding.
    ///
    /// `TimedOut` means the bounded probe returned before macOS answered, which
    /// is exactly the state where an operator still has to allow the prompt.
    /// Every other state is either answered (`Available`), answered negatively
    /// (`Unavailable`), not applicable, or a harness error.
    pub(crate) fn tcc_prompt_pending(&self) -> bool {
        matches!(self, Self::TimedOut)
    }

    /// The preflight object persisted after `update apply` activates a Runtime
    /// and exposed by `doctor` / `update status`.
    pub(crate) fn preflight_view(&self) -> Value {
        let detail = match self {
            Self::ProbeFailed(detail) => Some(detail.clone()),
            _ => None,
        };
        json!({
            "probe": self.state_token(),
            "prompt_pending": self.tcc_prompt_pending(),
            "detail": detail,
        })
    }
}

impl DocumentsPermission {
    pub(crate) fn doctor_pass(&self) -> bool {
        matches!(
            self,
            Self::Available | Self::NotPresent | Self::NotApplicable
        )
    }

    pub(crate) fn doctor_line(&self) -> String {
        match self {
            Self::Available => "PASS macOS Documents permission".to_owned(),
            Self::Unavailable => {
                "FAIL macOS Documents permission — macOS Documents permission unavailable"
                    .to_owned()
            }
            Self::TimedOut => "FAIL macOS Documents permission — probe timed out".to_owned(),
            Self::NotPresent => {
                "INFO macOS Documents permission — Documents directory not present".to_owned()
            }
            Self::ProbeFailed(detail) => {
                format!("FAIL macOS Documents permission — probe failed: {detail}")
            }
            Self::NotApplicable => "INFO macOS Documents permission — not applicable".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodeIdentity {
    pub(crate) mode: &'static str,
    pub(crate) identifier: Option<String>,
    pub(crate) team: Option<String>,
    pub(crate) expected_identifier: bool,
}

impl CodeIdentity {
    pub(crate) fn doctor_line(&self) -> String {
        format!(
            "INFO macOS-code-identity mode={} identifier={} team={} expected_identifier={}",
            self.mode,
            self.identifier.as_deref().unwrap_or("unknown"),
            self.team.as_deref().unwrap_or("none"),
            self.expected_identifier
        )
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn run_documents_probe_child() -> ExitCode {
    use std::fs;
    use std::io::ErrorKind;

    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return ExitCode::from(1);
    };
    let documents = home.join("Documents");
    match fs::read_dir(&documents) {
        Ok(_handle) => {
            let Ok(cstr) = std::ffi::CString::new(documents.to_string_lossy().as_bytes()) else {
                return ExitCode::from(1);
            };
            // Validate the mutation-free write gate under the same stable
            // broker identity. The rotating runtime must never probe W_OK on
            // ~/Documents directly.
            if unsafe { libc::access(cstr.as_ptr(), libc::W_OK) } == 0 {
                ExitCode::SUCCESS
            } else {
                let error = std::io::Error::last_os_error();
                if error.kind() == ErrorKind::PermissionDenied {
                    ExitCode::from(DOCUMENTS_PERMISSION_DENIED_EXIT)
                } else if error.kind() == ErrorKind::NotFound {
                    ExitCode::from(DOCUMENTS_NOT_PRESENT_EXIT)
                } else {
                    ExitCode::from(1)
                }
            }
        }
        Err(error) if error.kind() == ErrorKind::PermissionDenied => {
            ExitCode::from(DOCUMENTS_PERMISSION_DENIED_EXIT)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            ExitCode::from(DOCUMENTS_NOT_PRESENT_EXIT)
        }
        Err(_) => ExitCode::from(1),
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn run_documents_probe_child() -> ExitCode {
    ExitCode::SUCCESS
}

#[cfg(target_os = "macos")]
pub(crate) fn probe_documents_permission(config_dir: &Path) -> DocumentsPermission {
    use crate::child_process;
    use std::process::{Command, Stdio};

    let broker = crate::tcc_broker::broker_path(config_dir);
    let broker_is_owned_regular_file = crate::tcc_broker::status(&broker).is_some();
    let executable = match documents_probe_executable(&broker, broker_is_owned_regular_file) {
        Ok(path) => path,
        Err(detail) => return DocumentsPermission::ProbeFailed(detail),
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
        Err(error) => return DocumentsPermission::ProbeFailed(error.to_string()),
    };
    let _registration = child_process::register_owned_child("documents-probe", &child);
    match child_process::wait_bounded(&mut child, DOCUMENTS_PROBE_TIMEOUT) {
        Ok(Some(status)) if status.success() => DocumentsPermission::Available,
        Ok(Some(status)) if status.code() == Some(i32::from(DOCUMENTS_PERMISSION_DENIED_EXIT)) => {
            DocumentsPermission::Unavailable
        }
        Ok(Some(status)) if status.code() == Some(i32::from(DOCUMENTS_NOT_PRESENT_EXIT)) => {
            DocumentsPermission::NotPresent
        }
        Ok(Some(status)) => {
            DocumentsPermission::ProbeFailed(format!("exit status {}", status.code().unwrap_or(-1)))
        }
        Ok(None) => DocumentsPermission::TimedOut,
        Err(error) => DocumentsPermission::ProbeFailed(error.to_string()),
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn probe_documents_permission(_config_dir: &Path) -> DocumentsPermission {
    DocumentsPermission::NotApplicable
}

/// Select the executable that is allowed to become the macOS TCC responsible
/// client for the Documents probe. Doctor is also invoked directly from user
/// shells, which do not necessarily inherit the service LaunchAgent's
/// `HERDR_MCP_TCC_BROKER=1`. Therefore the protected-path probe always uses the
/// installed broker and never falls back to a rotating runtime generation.
#[cfg(any(target_os = "macos", test))]
fn documents_probe_executable(broker: &Path, broker_exists: bool) -> Result<PathBuf, String> {
    if broker_exists {
        return Ok(broker.to_path_buf());
    }
    Err(format!(
        "TCC broker routing is enabled but broker is missing at {}",
        broker.display()
    ))
}

#[cfg(target_os = "macos")]
pub(crate) fn probe_code_identity() -> CodeIdentity {
    use crate::child_process;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::thread;

    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => {
            return CodeIdentity {
                mode: "unverifiable",
                identifier: None,
                team: None,
                expected_identifier: false,
            };
        }
    };
    let mut command = Command::new("/usr/bin/codesign");
    command
        .args(["-dvvv", "--requirements", "-"])
        .arg(executable)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    child_process::configure_process_group(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            return CodeIdentity {
                mode: "unverifiable",
                identifier: None,
                team: None,
                expected_identifier: false,
            };
        }
    };
    let _registration = child_process::register_owned_child("codesign", &child);
    let Some(mut stderr) = child.stderr.take() else {
        child_process::terminate_and_reap(&mut child);
        return CodeIdentity {
            mode: "unverifiable",
            identifier: None,
            team: None,
            expected_identifier: false,
        };
    };
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stderr.by_ref().take(64 * 1024).read_to_end(&mut output);
        output
    });
    let status = child_process::wait_bounded(&mut child, Duration::from_secs(2));
    let output = reader.join().unwrap_or_default();
    if !matches!(status, Ok(Some(status)) if status.success()) {
        return CodeIdentity {
            mode: "unsigned_or_unverifiable",
            identifier: None,
            team: None,
            expected_identifier: false,
        };
    }
    let text = String::from_utf8_lossy(&output);
    let identifier = value_after_prefix(&text, "Identifier=");
    let team = value_after_prefix(&text, "TeamIdentifier=")
        .filter(|value| value != "not set" && !value.is_empty());
    let mode = if text.lines().any(|line| line.trim() == "Signature=adhoc") {
        "adhoc"
    } else if team.is_some() {
        "developer-id"
    } else {
        "signed-no-team"
    };
    CodeIdentity {
        expected_identifier: identifier.as_deref() == Some(STABLE_CODE_IDENTIFIER),
        identifier,
        team,
        mode,
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn probe_code_identity() -> CodeIdentity {
    CodeIdentity {
        mode: "not-applicable",
        identifier: None,
        team: None,
        expected_identifier: false,
    }
}

#[cfg(any(target_os = "macos", test))]
fn value_after_prefix(text: &str, prefix: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(prefix))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Exit codes of the `__documents-alias-probe` child. The child is the stable
/// broker identity, so it may inspect `~/Documents` when the rotating runtime
/// may not; the runtime only reads an exit code, never the protected path.
// The exit-code vocabulary is platform-independent: the mapping is compiled and
// tested on every target, and only the child that produces these codes is
// macOS-only.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const ALIAS_READY_EXIT: u8 = 0;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const ALIAS_MISSING_EXIT: u8 = 66;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const ALIAS_OCCUPIED_LINK_EXIT: u8 = 67;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const ALIAS_OCCUPIED_ENTRY_EXIT: u8 = 68;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const ALIAS_DENIED_EXIT: u8 = 77;
/// The child's own failure to create the alias (mkdir / symlink). Distinct from
/// exit 1 so a create failure on a current broker is never mistaken for the
/// `unknown command` an outdated broker returns.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const ALIAS_CREATE_FAILED_EXIT: u8 = 70;
/// A generic I/O or verification failure inside a current broker child.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const ALIAS_IO_ERROR_EXIT: u8 = 71;

/// Outcome of one bounded, broker-routed alias inspection.
///
/// Only macOS constructs the answered variants; other targets return `Denied`
/// or `BrokerOutdated`, so the enum carries a per-platform dead-code allowance.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum BrokeredAlias {
    /// The entry is a symlink to the expected target.
    Ready,
    /// The entry is a symlink to something else.
    OccupiedLink,
    /// The entry exists but is not a symlink (file or directory).
    OccupiedEntry,
    /// Nothing exists at the path.
    Missing,
    /// The broker did not answer within the budget: a prompt is still up.
    Pending,
    /// The installed broker predates the alias subcommand (it exits 1 for
    /// "unknown command"). Reinstall the broker; this is not a refusal.
    BrokerOutdated,
    /// The broker refused, is missing, or failed.
    Denied(String),
}

/// Map the alias child's exit code to a decision.
///
/// Extracted so the mapping is testable on every platform. A pre-#844 broker
/// rejects the argv and exits 1, which must surface as `BrokerOutdated`
/// (reinstall) rather than as a permission refusal that would leave the default
/// Documents alias permanently uncreatable.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn map_alias_probe_exit(code: Option<i32>) -> BrokeredAlias {
    match code.map(|code| code as u8) {
        Some(ALIAS_READY_EXIT) => BrokeredAlias::Ready,
        Some(ALIAS_OCCUPIED_LINK_EXIT) => BrokeredAlias::OccupiedLink,
        Some(ALIAS_OCCUPIED_ENTRY_EXIT) => BrokeredAlias::OccupiedEntry,
        Some(ALIAS_MISSING_EXIT) => BrokeredAlias::Missing,
        Some(ALIAS_DENIED_EXIT) => {
            BrokeredAlias::Denied("the stable broker cannot read this protected path".to_owned())
        }
        Some(ALIAS_CREATE_FAILED_EXIT) => {
            BrokeredAlias::Denied("the broker could not create the Documents alias".to_owned())
        }
        Some(ALIAS_IO_ERROR_EXIT) => BrokeredAlias::Denied(
            "the broker hit an I/O error while checking the Documents alias".to_owned(),
        ),
        // Only exit 1 / no code means the installed broker did not understand the
        // argv at all: `herdr-mcp <unknown-subcommand>` prints usage and returns
        // `ExitCode::FAILURE` (1) from the CLI error path, which is exactly what a
        // pre-#844 broker does with `__documents-alias-probe`/`-ensure`.
        Some(1) | None => BrokeredAlias::BrokerOutdated,
        other => BrokeredAlias::Denied(format!("alias probe exit {other:?}")),
    }
}

/// Inspect one Chrome-facing alias through the stable broker identity.
///
/// This is the whole point of the alias check: the rotating generation must not
/// run `symlink_metadata`/`read_link` on `~/Documents`, but the answer still has
/// to be real (so a missing alias can be created and an existing one reused).
/// One bounded child does the stat, and only a timeout or a refusal turns into
/// `tcc_pending` upstream.
#[cfg(target_os = "macos")]
pub(crate) fn probe_alias_via_stable_broker(
    config_dir: &Path,
    path: &Path,
    expected: &Path,
) -> BrokeredAlias {
    use crate::child_process;
    use std::process::{Command, Stdio};

    let broker = crate::tcc_broker::broker_path(config_dir);
    let broker_is_owned_regular_file = crate::tcc_broker::status(&broker).is_some();
    let executable = match documents_probe_executable(&broker, broker_is_owned_regular_file) {
        Ok(path) => path,
        Err(detail) => return BrokeredAlias::Denied(detail),
    };
    let mut command = Command::new(executable);
    command
        .arg("__documents-alias-probe")
        .arg(path)
        .arg(expected)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    child_process::configure_process_group(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return BrokeredAlias::Denied(error.to_string()),
    };
    let _registration = child_process::register_owned_child("documents-alias-probe", &child);
    match child_process::wait_bounded(&mut child, DOCUMENTS_PROBE_TIMEOUT) {
        Ok(Some(status)) => map_alias_probe_exit(status.code()),
        Ok(None) => BrokeredAlias::Pending,
        Err(error) => BrokeredAlias::Denied(error.to_string()),
    }
}

/// Create or verify the Chrome-facing alias through the stable broker.
///
/// The parent-directory create, the symlink and the readlink verification all
/// run inside the broker child, so the rotating generation never mutates or
/// stats `~/Documents`. Bounded like the probe: a timeout is `Pending`, and an
/// unknown-command exit means the installed broker is outdated.
#[cfg(target_os = "macos")]
pub(crate) fn ensure_alias_via_stable_broker(
    config_dir: &Path,
    path: &Path,
    expected: &Path,
) -> BrokeredAlias {
    use crate::child_process;
    use std::process::{Command, Stdio};

    let broker = crate::tcc_broker::broker_path(config_dir);
    let broker_is_owned_regular_file = crate::tcc_broker::status(&broker).is_some();
    let executable = match documents_probe_executable(&broker, broker_is_owned_regular_file) {
        Ok(path) => path,
        Err(detail) => return BrokeredAlias::Denied(detail),
    };
    let mut command = Command::new(executable);
    command
        .arg("__documents-alias-ensure")
        .arg(path)
        .arg(expected)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    child_process::configure_process_group(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return BrokeredAlias::Denied(error.to_string()),
    };
    let _registration = child_process::register_owned_child("documents-alias-ensure", &child);
    match child_process::wait_bounded(&mut child, DOCUMENTS_PROBE_TIMEOUT) {
        Ok(Some(status)) => map_alias_probe_exit(status.code()),
        Ok(None) => BrokeredAlias::Pending,
        Err(error) => BrokeredAlias::Denied(error.to_string()),
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn ensure_alias_via_stable_broker(
    _config_dir: &Path,
    _path: &Path,
    _expected: &Path,
) -> BrokeredAlias {
    BrokeredAlias::Denied("macOS Documents aliases are not applicable on this platform".to_owned())
}

/// Child side of [`ensure_alias_via_stable_broker`], under the broker identity.
#[cfg(target_os = "macos")]
pub(crate) fn run_documents_alias_ensure_child(path: &Path, expected: &Path) -> ExitCode {
    // Probe first so an occupied path is never overwritten.
    match run_documents_alias_probe_child(path, expected) {
        ready if ready == ExitCode::from(ALIAS_READY_EXIT) => return ExitCode::SUCCESS,
        occupied
            if occupied == ExitCode::from(ALIAS_OCCUPIED_LINK_EXIT)
                || occupied == ExitCode::from(ALIAS_OCCUPIED_ENTRY_EXIT) =>
        {
            return occupied;
        }
        denied if denied == ExitCode::from(ALIAS_DENIED_EXIT) => return denied,
        _ => {}
    }
    let Some(parent) = path.parent() else {
        return ExitCode::from(ALIAS_CREATE_FAILED_EXIT);
    };
    if std::fs::create_dir_all(parent).is_err() {
        return ExitCode::from(ALIAS_CREATE_FAILED_EXIT);
    }
    if std::os::unix::fs::symlink(expected, path).is_err() {
        return ExitCode::from(ALIAS_CREATE_FAILED_EXIT);
    }
    match std::fs::read_link(path) {
        Ok(target) if target == expected => ExitCode::from(ALIAS_READY_EXIT),
        Ok(_) => ExitCode::from(ALIAS_OCCUPIED_LINK_EXIT),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            ExitCode::from(ALIAS_DENIED_EXIT)
        }
        Err(_) => ExitCode::from(ALIAS_IO_ERROR_EXIT),
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn run_documents_alias_ensure_child(_path: &Path, _expected: &Path) -> ExitCode {
    ExitCode::from(ALIAS_IO_ERROR_EXIT)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn probe_alias_via_stable_broker(
    _config_dir: &Path,
    _path: &Path,
    _expected: &Path,
) -> BrokeredAlias {
    BrokeredAlias::Denied("macOS Documents aliases are not applicable on this platform".to_owned())
}

/// Child side of [`probe_alias_via_stable_broker`]. Runs only under the stable
/// broker identity, which is the process that holds the Documents grant.
#[cfg(target_os = "macos")]
pub(crate) fn run_documents_alias_probe_child(path: &Path, expected: &Path) -> ExitCode {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => match std::fs::read_link(path) {
            Ok(target) if target == expected => ExitCode::from(ALIAS_READY_EXIT),
            Ok(_) => ExitCode::from(ALIAS_OCCUPIED_LINK_EXIT),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                ExitCode::from(ALIAS_DENIED_EXIT)
            }
            Err(_) => ExitCode::from(ALIAS_IO_ERROR_EXIT),
        },
        Ok(_) => ExitCode::from(ALIAS_OCCUPIED_ENTRY_EXIT),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ExitCode::from(ALIAS_MISSING_EXIT)
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            ExitCode::from(ALIAS_DENIED_EXIT)
        }
        Err(_) => ExitCode::from(ALIAS_IO_ERROR_EXIT),
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn run_documents_alias_probe_child(_path: &Path, _expected: &Path) -> ExitCode {
    ExitCode::from(ALIAS_MISSING_EXIT)
}

/// Where the post-activation Documents preflight is recorded.
///
/// It lives next to the other update artifacts so `update status` can report the
/// prompt state without probing again: a status query must never become the
/// syscall that blocks on a TCC dialog.
pub(crate) fn tcc_preflight_path(config_dir: &Path) -> PathBuf {
    config_dir.join("update").join("tcc-preflight.json")
}

/// Record one preflight result. Best effort: a failed write never fails an
/// update that already committed, it only leaves the previous record in place.
pub(crate) fn persist_tcc_preflight(
    config_dir: &Path,
    permission: &DocumentsPermission,
) -> Result<(), String> {
    let path = tcc_preflight_path(config_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let mut view = permission.preflight_view();
    view["recorded_at_ms"] = json!(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or_default()
    );
    crate::mutation::atomic_write(&path, view.to_string().as_bytes())
}

/// Persisted preflight view, or an explicit `unknown` shape when no update has
/// recorded one yet. Never probes.
pub(crate) fn persisted_tcc_preflight(config_dir: &Path) -> Value {
    std::fs::read_to_string(tcc_preflight_path(config_dir))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or_else(|| {
            json!({
                "probe": "unknown",
                "prompt_pending": Value::Null,
                "detail": Value::Null,
                "recorded_at_ms": Value::Null,
            })
        })
}

/// Exactly one post-activation Documents preflight.
///
/// The probe is the same bounded, broker-only probe `doctor` uses, so at most one
/// macOS dialog can appear per upgrade and it is created by the stable broker
/// identity rather than the rotating runtime generation. The result is persisted
/// so later status queries read it instead of probing.
pub(crate) fn preflight_after_activation(config_dir: &Path) -> Value {
    let permission = probe_documents_permission(config_dir);
    let _ = persist_tcc_preflight(config_dir, &permission);
    permission.preflight_view()
}

#[cfg(test)]
mod tests {
    /// user can tell an outdated broker from a refusal | Given each alias child
    /// exit code, including the `unknown command` exit 1 a pre-#844 broker
    /// returns | When the runtime maps it | Then exit 1 is `broker_outdated`
    /// (reinstall), not a permission denial that would leave the default
    /// Documents alias uncreatable forever.
    #[test]
    fn alias_probe_exit_codes_map_to_decisions() {
        use super::{BrokeredAlias, map_alias_probe_exit};
        assert_eq!(map_alias_probe_exit(Some(0)), BrokeredAlias::Ready);
        assert_eq!(map_alias_probe_exit(Some(66)), BrokeredAlias::Missing);
        assert_eq!(map_alias_probe_exit(Some(67)), BrokeredAlias::OccupiedLink);
        assert_eq!(map_alias_probe_exit(Some(68)), BrokeredAlias::OccupiedEntry);
        assert!(matches!(
            map_alias_probe_exit(Some(77)),
            BrokeredAlias::Denied(_)
        ));
        // 70/71 are the alias child's own failures on a current broker: they must
        // read as a plain failure with detail, never as "reinstall the broker"
        // (that path mints a new TCC identity and re-prompts).
        assert!(matches!(
            map_alias_probe_exit(Some(70)),
            BrokeredAlias::Denied(detail) if detail.contains("could not create")
        ));
        assert!(matches!(
            map_alias_probe_exit(Some(71)),
            BrokeredAlias::Denied(detail) if detail.contains("I/O error")
        ));
        // Only exit 1 (the `unknown command` path of a pre-#844 broker) or no
        // exit code at all means the broker predates the subcommand.
        assert_eq!(map_alias_probe_exit(Some(1)), BrokeredAlias::BrokerOutdated);
        assert_eq!(map_alias_probe_exit(None), BrokeredAlias::BrokerOutdated);
        assert!(matches!(
            map_alias_probe_exit(Some(9)),
            BrokeredAlias::Denied(_)
        ));
    }

    /// user sees one prompt state everywhere | Given each probe outcome | When
    /// doctor, update status or the persisted preflight name it | Then only a
    /// bounded-out probe is `pending` and every other state is answered.
    #[test]
    fn tcc_prompt_pending_is_only_the_bounded_out_probe() {
        use super::DocumentsPermission;
        assert_eq!(DocumentsPermission::TimedOut.state_token(), "pending");
        assert!(DocumentsPermission::TimedOut.tcc_prompt_pending());
        for answered in [
            DocumentsPermission::Available,
            DocumentsPermission::Unavailable,
            DocumentsPermission::NotPresent,
            DocumentsPermission::NotApplicable,
        ] {
            assert!(!answered.tcc_prompt_pending(), "{answered:?}");
            assert_ne!(answered.state_token(), "pending", "{answered:?}");
        }
        let failed = DocumentsPermission::ProbeFailed("boom".to_owned());
        assert!(!failed.tcc_prompt_pending());
        assert_eq!(failed.state_token(), "probe_failed");
        assert_eq!(failed.preflight_view()["detail"], "boom");
    }

    /// user keeps the recorded preflight across processes | Given one recorded
    /// probe result | When update status reads it later | Then it reports the same
    /// state without probing again.
    #[test]
    fn tcc_preflight_record_round_trips_without_probing() {
        use super::DocumentsPermission;
        use super::{persist_tcc_preflight, persisted_tcc_preflight, tcc_preflight_path};
        let root = std::env::temp_dir().join(format!(
            "herdr-tcc-preflight-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        // Before any update records one, the view is explicitly unknown and never
        // claims a prompt state.
        let empty = persisted_tcc_preflight(&root);
        assert_eq!(empty["probe"], "unknown");
        assert_eq!(empty["prompt_pending"], serde_json::Value::Null);

        persist_tcc_preflight(&root, &DocumentsPermission::TimedOut).unwrap();
        let pending = persisted_tcc_preflight(&root);
        assert_eq!(pending["probe"], "pending");
        assert_eq!(pending["prompt_pending"], true);
        assert!(pending["recorded_at_ms"].as_u64().is_some());
        assert!(tcc_preflight_path(&root).is_file());

        persist_tcc_preflight(&root, &DocumentsPermission::Available).unwrap();
        let answered = persisted_tcc_preflight(&root);
        assert_eq!(answered["probe"], "available");
        assert_eq!(answered["prompt_pending"], false);
        let _ = std::fs::remove_dir_all(&root);
    }

    use super::*;

    #[test]
    fn parses_codesign_fields_without_certificate_material() {
        let text = "Identifier=dev.herdr.mcp\nTeamIdentifier=ABCDE12345\nSignature=adhoc\n";
        assert_eq!(
            value_after_prefix(text, "Identifier="),
            Some("dev.herdr.mcp".to_owned())
        );
        assert_eq!(
            value_after_prefix(text, "TeamIdentifier="),
            Some("ABCDE12345".to_owned())
        );
    }

    #[test]
    fn documents_doctor_lines_are_explicit() {
        assert_eq!(
            DocumentsPermission::Unavailable.doctor_line(),
            "FAIL macOS Documents permission — macOS Documents permission unavailable"
        );
        assert_eq!(
            DocumentsPermission::TimedOut.doctor_line(),
            "FAIL macOS Documents permission — probe timed out"
        );
    }

    #[test]
    fn documents_probe_never_falls_back_to_rotating_runtime() {
        let broker = Path::new("/tmp/config/tcc-broker/herdr-mcp-broker");
        assert_eq!(documents_probe_executable(broker, true).unwrap(), broker);
        assert!(documents_probe_executable(broker, false).is_err());
    }
}
