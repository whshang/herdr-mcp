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

#[cfg(test)]
mod tests {
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
