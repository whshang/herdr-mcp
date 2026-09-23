use crate::child_process;
use crate::paths::RuntimePaths;
use reqwest::blocking::Client;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(not(target_os = "macos"))]
use std::process::Stdio;
#[cfg(not(target_os = "macos"))]
use std::thread;
use std::time::Duration;
#[cfg(not(target_os = "macos"))]
use std::time::Instant;

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(not(target_os = "macos"))]
const SERVER_START_BUDGET: Duration = Duration::from_secs(15);
const MAX_INSTALLER_BYTES: usize = 1024 * 1024;
const UNIX_INSTALLER_URL: &str = "https://herdr.dev/install.sh";
const WINDOWS_INSTALLER_URL: &str = "https://herdr.dev/install.ps1";

pub(crate) fn prepare_for_service_install() -> Result<PathBuf, String> {
    let (binary, installed_now) = match crate::workstation::find_herdr_executable() {
        Some(binary) => (binary, false),
        None => {
            install_official_herdr()?;
            let binary = crate::workstation::find_herdr_executable().ok_or_else(|| {
                "Herdr installer completed but herdr-mcp still cannot locate the Herdr executable in its stable user/install paths"
                    .to_owned()
            })?;
            (binary, true)
        }
    };

    verify_cli(&binary)?;
    if !installed_now {
        update_existing_herdr(&binary)?;
    }

    let resolved = crate::workstation::find_herdr_executable().unwrap_or(binary);
    verify_cli(&resolved)?;
    Ok(resolved)
}

pub(crate) fn ensure_server_ready_after_install() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        crate::herdr_supervisor::ensure_ready_for_service()
    }

    #[cfg(not(target_os = "macos"))]
    {
        let binary = crate::workstation::find_herdr_executable().ok_or_else(|| {
            "Herdr executable disappeared after dependency preparation; rerun herdr-mcp install to repair the dependency"
                .to_owned()
        })?;
        ensure_server_running_direct(&binary)
    }
}

fn verify_cli(binary: &Path) -> Result<(), String> {
    run_success(
        binary,
        &["--version"],
        Duration::from_secs(5),
        "Herdr version probe",
    )?;
    run_success(
        binary,
        &["api", "schema"],
        Duration::from_secs(8),
        "Herdr API schema probe",
    )?;
    Ok(())
}

fn update_existing_herdr(binary: &Path) -> Result<(), String> {
    let running = server_reports_running(binary).unwrap_or(false);
    let args = if running {
        vec!["update", "--handoff"]
    } else {
        vec!["update"]
    };
    match run_success(binary, &args, COMMAND_TIMEOUT, "Herdr update") {
        Ok(()) => Ok(()),
        Err(update_error) => {
            // Existing compatible Herdr remains usable when the update service is
            // temporarily unavailable. Installation must not turn a transient
            // update-network failure into loss of a working local workspace.
            verify_cli(binary).map_err(|probe_error| {
                format!(
                    "Herdr automatic update failed ({update_error}) and the installed Herdr is not usable ({probe_error})"
                )
            })?;
            eprintln!(
                "warning: Herdr automatic update did not complete; continuing with the verified installed CLI: {update_error}"
            );
            Ok(())
        }
    }
}

fn server_reports_running(binary: &Path) -> Result<bool, String> {
    let paths = RuntimePaths::discover()?;
    let socket = paths
        .herdr_socket
        .as_deref()
        .ok_or_else(|| "Herdr dependency requires a local API endpoint".to_owned())?;
    let mut command = Command::new(binary);
    command
        .args(["status", "server", "--json"])
        .env("HERDR_SOCKET_PATH", socket);
    let Some(output) =
        child_process::run_bounded_output(&mut command, Duration::from_secs(4), 32 * 1024)
            .map_err(|error| format!("cannot query Herdr server status: {error}"))?
    else {
        return Ok(false);
    };
    if !output.status.success() || output.truncated {
        return Ok(false);
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| "Herdr server status returned invalid JSON".to_owned())?;
    Ok(value.get("running").and_then(serde_json::Value::as_bool) == Some(true))
}

#[cfg(not(target_os = "macos"))]
fn ensure_server_running_direct(binary: &Path) -> Result<(), String> {
    let paths = RuntimePaths::discover()?;
    let socket = paths
        .herdr_socket
        .as_deref()
        .ok_or_else(|| "Herdr dependency requires a local API endpoint".to_owned())?;

    if herdr_reachable(socket) {
        return Ok(());
    }

    if server_reports_running(binary)? {
        return wait_for_server(socket, None);
    }

    let mut command = Command::new(binary);
    command
        .arg("server")
        .env("HERDR_SOCKET_PATH", socket)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .env_remove("HERDR_EDGE_TOKEN")
        .env_remove("HERDR_LINK_TOKEN")
        .env_remove("HERDR_MCP_CLIENT_SECRET")
        .env_remove("HERDR_MCP_TOKEN")
        .env_remove("LINK_SHARED_SECRET")
        .env_remove("STATIC_MCP_BEARER_SECRET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    child_process::configure_process_group(&mut command);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    let child = command.spawn().map_err(|error| {
        format!(
            "cannot start installed Herdr server {}: {error}",
            binary.display()
        )
    })?;
    wait_for_server(socket, Some(child))
}

#[cfg(not(target_os = "macos"))]
fn wait_for_server(socket: &Path, mut child: Option<std::process::Child>) -> Result<(), String> {
    let deadline = Instant::now() + SERVER_START_BUDGET;
    loop {
        if herdr_reachable(socket) {
            return Ok(());
        }
        if let Some(child) = child.as_mut()
            && let Some(status) = child
                .try_wait()
                .map_err(|error| format!("cannot inspect started Herdr server: {error}"))?
        {
            return Err(format!(
                "started Herdr server exited before its API became reachable: {status}"
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Herdr server did not make {} reachable within {} seconds",
                socket.display(),
                SERVER_START_BUDGET.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(not(target_os = "macos"))]
fn herdr_reachable(socket: &Path) -> bool {
    crate::herdr::HerdrClient::new(socket)
        .call_with_timeout("ping", serde_json::json!({}), Duration::from_millis(500))
        .is_ok()
}

fn run_success(binary: &Path, args: &[&str], timeout: Duration, label: &str) -> Result<(), String> {
    let mut command = Command::new(binary);
    command.args(args);
    let Some(output) = child_process::run_bounded_output(&mut command, timeout, 128 * 1024)
        .map_err(|error| format!("{label} failed to start: {error}"))?
    else {
        return Err(format!(
            "{label} timed out after {} seconds",
            timeout.as_secs()
        ));
    };
    if output.status.success() && !output.truncated {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(if detail.is_empty() {
        format!("{label} failed with status {}", output.status)
    } else {
        format!("{label} failed: {detail}")
    })
}

fn install_official_herdr() -> Result<(), String> {
    let (url, suffix) = installer_spec();
    let bytes = Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|error| format!("cannot create Herdr installer HTTP client: {error}"))?
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| format!("cannot download official Herdr installer: {error}"))?
        .bytes()
        .map_err(|error| format!("cannot read official Herdr installer: {error}"))?;
    if bytes.is_empty() || bytes.len() > MAX_INSTALLER_BYTES {
        return Err(format!(
            "official Herdr installer has invalid size {} bytes",
            bytes.len()
        ));
    }

    let path = env::temp_dir().join(format!("herdr-install-{}.{suffix}", std::process::id()));
    #[cfg(unix)]
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o700)
        .open(&path)
        .map_err(|error| format!("cannot create temporary Herdr installer: {error}"))?;
    #[cfg(not(unix))]
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("cannot create temporary Herdr installer: {error}"))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot persist temporary Herdr installer: {error}"))?;
    drop(file);

    let result = run_installer(&path);
    let _ = fs::remove_file(&path);
    result
}

fn installer_spec() -> (&'static str, &'static str) {
    if cfg!(windows) {
        (WINDOWS_INSTALLER_URL, "ps1")
    } else {
        (UNIX_INSTALLER_URL, "sh")
    }
}

fn run_installer(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            path.to_str()
                .ok_or_else(|| "temporary Herdr installer path is not UTF-8".to_owned())?,
        ]);
        command
    };

    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.arg(path);
        command
    };

    let Some(output) = child_process::run_bounded_output(&mut command, COMMAND_TIMEOUT, 256 * 1024)
        .map_err(|error| format!("official Herdr installer failed to start: {error}"))?
    else {
        return Err(format!(
            "official Herdr installer timed out after {} seconds",
            COMMAND_TIMEOUT.as_secs()
        ));
    };
    if output.status.success() && !output.truncated {
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if detail.is_empty() {
            format!(
                "official Herdr installer failed with status {}",
                output.status
            )
        } else {
            format!("official Herdr installer failed: {detail}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn installer_spec_uses_only_official_herdr_origin() {
        let (url, suffix) = installer_spec();
        assert!(url.starts_with("https://herdr.dev/"));
        if cfg!(windows) {
            assert_eq!(suffix, "ps1");
        } else {
            assert_eq!(suffix, "sh");
        }
    }

    #[cfg(unix)]
    #[test]
    fn existing_herdr_is_verified_and_updated_before_service_install() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "herdr-dependency-test-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let binary = root.join("herdr");
        let marker = root.join("updated");
        let script = format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n  \"--version \") echo 'herdr 0.9.1'; exit 0 ;;\n  \"api schema\") echo '{{}}'; exit 0 ;;\n  \"status server\") echo '{{\"running\":false}}'; exit 0 ;;\n  \"update \") : > '{}'; exit 0 ;;\nesac\nexit 2\n",
            marker.display()
        );
        fs::write(&binary, script).unwrap();
        let mut permissions = fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&binary, permissions).unwrap();

        update_existing_herdr(&binary).unwrap();
        assert!(marker.is_file());
        fs::remove_dir_all(root).unwrap();
    }
}
