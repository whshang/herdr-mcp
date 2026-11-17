//! Read-only local diagnostic snapshot (1.2 S7).
//!
//! One hop for an operator who needs the local facts P9 listed as missing:
//! whitelisted processes (name, PID, start time), whitelisted local ports
//! (listening or not) and whitelisted configuration files (exists + mtime).
//!
//! Security contract:
//! - metadata only: file contents, environment values, tokens and credentials
//!   are never read, returned or inferred;
//! - allow-list only: a process, port or file that is not named here never
//!   appears, even when it exists;
//! - `state.db` is deliberately absent: this snapshot is metadata about the
//!   installation, not a reader of the state store;
//! - a metadata read that fails returns an explicit per-entry error instead of
//!   silently reporting the entry as missing.

use serde_json::{Value, json};
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

pub const SCHEMA_VERSION: u64 = 1;

/// Process image names that may appear. Matched against the executable
/// basename exactly; anything else is dropped before serialization.
pub const WHITELISTED_PROCESSES: &[&str] = &["herdr", "herdr-mcp", "hc"];

/// Loopback TCP ports that may be probed, with a fixed public label.
pub const WHITELISTED_PORTS: &[(u16, &str)] = &[(8772, "runtime-http")];

/// Configuration paths relative to the instance config directory. Only
/// existence, kind, mtime and (for the runtime pointer) the symlink target are
/// reported - never content.
pub const WHITELISTED_CONFIG_FILES: &[&str] = &[
    "config.json",
    "runtime-control.json",
    "runtime-status.json",
    "link-transport-telemetry.json",
    "runtime/current",
];

const PORT_PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// One process row, already reduced to non-secret metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRow {
    pub name: String,
    pub pid: u32,
    pub started_ms: Option<i64>,
}

/// Local facts source. Production reads `ps` and probes loopback ports; tests
/// inject a fake so allow-list filtering and content non-disclosure are proven
/// without depending on the host's live process table.
pub trait LocalProbe {
    fn processes(&self) -> Result<Vec<ProcessRow>, String>;
    fn port_listening(&self, port: u16) -> bool;
}

pub struct SystemProbe;

impl LocalProbe for SystemProbe {
    fn processes(&self) -> Result<Vec<ProcessRow>, String> {
        let output = std::process::Command::new("ps")
            .args(["-A", "-o", "pid=,etime=,comm="])
            .output()
            .map_err(|error| format!("cannot enumerate processes: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "cannot enumerate processes: ps exited with {}",
                output.status
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(parse_ps_output(&text, now_ms()))
    }

    fn port_listening(&self, port: u16) -> bool {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        TcpStream::connect_timeout(&address, PORT_PROBE_TIMEOUT).is_ok()
    }
}

/// Parse `ps -A -o pid=,etime=,comm=`. Unparsable rows and non-whitelisted
/// images are dropped, so a malformed or noisy process table can never widen
/// what the snapshot exposes.
pub fn parse_ps_output(text: &str, now_ms: i64) -> Vec<ProcessRow> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(etime), Some(comm)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        let name = comm.rsplit('/').next().unwrap_or(comm);
        if !WHITELISTED_PROCESSES.contains(&name) {
            continue;
        }
        let started_ms = parse_etime_seconds(etime).map(|seconds| now_ms - (seconds * 1_000));
        rows.push(ProcessRow {
            name: name.to_owned(),
            pid,
            started_ms,
        });
    }
    rows
}

/// `ps` elapsed time: `[[dd-]hh:]mm:ss`.
fn parse_etime_seconds(value: &str) -> Option<i64> {
    let (days, rest) = match value.split_once('-') {
        Some((days, rest)) => (days.parse::<i64>().ok()?, rest),
        None => (0, value),
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (
            0,
            minutes.parse::<i64>().ok()?,
            seconds.parse::<i64>().ok()?,
        ),
        [hours, minutes, seconds] => (
            hours.parse::<i64>().ok()?,
            minutes.parse::<i64>().ok()?,
            seconds.parse::<i64>().ok()?,
        ),
        _ => return None,
    };
    Some(days * 86_400 + hours * 3_600 + minutes * 60 + seconds)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// Build the snapshot for one instance config directory.
pub fn snapshot(config_dir: &Path) -> Value {
    snapshot_with(config_dir, &SystemProbe)
}

pub fn snapshot_with(config_dir: &Path, probe: &dyn LocalProbe) -> Value {
    let (processes, process_error) = match probe.processes() {
        Ok(rows) => {
            let mut allowed = rows
                .into_iter()
                .filter(|row| WHITELISTED_PROCESSES.contains(&row.name.as_str()))
                .collect::<Vec<_>>();
            allowed.sort_by(|left, right| {
                left.name
                    .cmp(&right.name)
                    .then_with(|| left.pid.cmp(&right.pid))
            });
            (
                allowed
                    .into_iter()
                    .map(|row| {
                        json!({
                            "name": row.name,
                            "pid": row.pid,
                            "started_ms": row.started_ms,
                        })
                    })
                    .collect::<Vec<_>>(),
                Value::Null,
            )
        }
        Err(error) => (Vec::new(), json!(error)),
    };

    let ports = WHITELISTED_PORTS
        .iter()
        .map(|(port, name)| {
            json!({
                "name": name,
                "port": port,
                "listening": probe.port_listening(*port),
            })
        })
        .collect::<Vec<_>>();

    let config_files = WHITELISTED_CONFIG_FILES
        .iter()
        .map(|relative| config_file_entry(config_dir, relative))
        .collect::<Vec<_>>();

    json!({
        "ok": true,
        "schema_version": SCHEMA_VERSION,
        "local": true,
        "generated_at_ms": now_ms(),
        "config_dir": config_dir.to_string_lossy(),
        "processes": processes,
        "process_error": process_error,
        "ports": ports,
        "config_files": config_files,
        "metadata_only": true,
        "excluded": [
            "file contents",
            "environment values",
            "credentials and secrets",
            "state.db",
            "non-allowlisted processes, ports and files",
        ],
    })
}

/// Existence/kind/mtime for one allow-listed configuration path. Never opens
/// the file; a symlink's target string is metadata, not content.
fn config_file_entry(config_dir: &Path, relative: &str) -> Value {
    let path = config_dir.join(relative);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            let kind = if metadata.file_type().is_symlink() {
                "symlink"
            } else if metadata.is_dir() {
                "dir"
            } else if metadata.is_file() {
                "file"
            } else {
                "other"
            };
            let mut entry = json!({
                "name": relative,
                "path": path.to_string_lossy(),
                "exists": true,
                "kind": kind,
                "modified_ms": metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|value| value.as_millis().min(i64::MAX as u128) as i64),
            });
            if kind == "symlink"
                && let Ok(target) = std::fs::read_link(&path)
            {
                entry["target"] = json!(target.to_string_lossy());
            }
            entry
        }
        Err(error) if error.kind() == ErrorKind::NotFound => json!({
            "name": relative,
            "path": path.to_string_lossy(),
            "exists": false,
        }),
        Err(error) => json!({
            "name": relative,
            "path": path.to_string_lossy(),
            "exists": Value::Null,
            "error": io_error_code(&error),
            "detail": error.to_string(),
        }),
    }
}

fn io_error_code(error: &std::io::Error) -> &'static str {
    match error.kind() {
        ErrorKind::NotFound => "not_found",
        ErrorKind::PermissionDenied => "permission_denied",
        _ => "io_error",
    }
}

/// `doctor --local` entry point: print the JSON snapshot and report success.
pub fn print_local_snapshot(config_dir: &Path) -> bool {
    match serde_json::to_string_pretty(&snapshot(config_dir)) {
        Ok(text) => {
            println!("{text}");
            true
        }
        Err(error) => {
            eprintln!("herdr-mcp doctor --local: cannot serialize snapshot: {error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct FakeProbe {
        processes: Result<Vec<ProcessRow>, String>,
    }

    impl LocalProbe for FakeProbe {
        fn processes(&self) -> Result<Vec<ProcessRow>, String> {
            self.processes.clone()
        }

        fn port_listening(&self, _port: u16) -> bool {
            false
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-local-diag-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn non_allowlisted_processes_and_extra_rows_never_appear() {
        let dir = temp_dir("allowlist");
        let probe = FakeProbe {
            processes: Ok(vec![
                ProcessRow {
                    name: "herdr-mcp".to_owned(),
                    pid: 42,
                    started_ms: Some(1_000),
                },
                ProcessRow {
                    name: "python3".to_owned(),
                    pid: 43,
                    started_ms: Some(2_000),
                },
                ProcessRow {
                    name: "secret-credential-broker".to_owned(),
                    pid: 44,
                    started_ms: None,
                },
            ]),
        };
        let snapshot = snapshot_with(&dir, &probe);
        let processes = snapshot["processes"].as_array().unwrap();
        assert_eq!(processes.len(), 1, "{snapshot}");
        assert_eq!(processes[0]["name"], json!("herdr-mcp"));
        assert_eq!(processes[0]["pid"], json!(42));
        assert_eq!(processes[0]["started_ms"], json!(1_000));
        let rendered = snapshot.to_string();
        assert!(!rendered.contains("python3"), "{rendered}");
        assert!(!rendered.contains("secret-credential-broker"), "{rendered}");

        // Ports are a fixed allow-list, so no other port can ever be reported.
        let ports = snapshot["ports"].as_array().unwrap();
        assert_eq!(ports.len(), WHITELISTED_PORTS.len(), "{snapshot}");
        for (entry, (port, name)) in ports.iter().zip(WHITELISTED_PORTS.iter()) {
            assert_eq!(entry["name"], json!(name));
            assert_eq!(entry["port"], json!(port));
            assert!(entry["listening"].is_boolean(), "{snapshot}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_contents_and_secret_values_are_never_returned() {
        let dir = temp_dir("contents");
        std::fs::write(
            dir.join("config.json"),
            r#"{"token":"SECRET-VALUE-MUST-NOT-LEAK","api_key":"another-secret"}"#,
        )
        .unwrap();
        let probe = FakeProbe {
            processes: Ok(Vec::new()),
        };
        let snapshot = snapshot_with(&dir, &probe);
        let rendered = snapshot.to_string();
        assert!(
            !rendered.contains("SECRET-VALUE-MUST-NOT-LEAK"),
            "{rendered}"
        );
        assert!(!rendered.contains("another-secret"), "{rendered}");
        assert!(!rendered.contains("api_key"), "{rendered}");

        let config = snapshot["config_files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == json!("config.json"))
            .unwrap();
        assert_eq!(config["exists"], json!(true), "{config}");
        assert_eq!(config["kind"], json!("file"), "{config}");
        assert!(config["modified_ms"].is_i64(), "{config}");
        // The entry exposes metadata keys only.
        let keys = config
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for key in &keys {
            assert!(
                matches!(
                    key.as_str(),
                    "name"
                        | "path"
                        | "exists"
                        | "kind"
                        | "modified_ms"
                        | "target"
                        | "error"
                        | "detail"
                ),
                "unexpected metadata key {key}"
            );
        }
        assert!(!keys.iter().any(|key| key == "content" || key == "text"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn permission_failure_is_reported_explicitly() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("permission");
        let locked = dir.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("config.json"), "{}").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let probe = FakeProbe {
            processes: Ok(Vec::new()),
        };
        let snapshot = snapshot_with(&locked, &probe);
        let entry = snapshot["config_files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == json!("config.json"))
            .unwrap();
        // Restore permissions before asserting so a failure cannot leave an
        // unreadable directory behind.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(entry["error"], json!("permission_denied"), "{entry}");
        assert!(entry["exists"].is_null(), "{entry}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn process_rows_are_parsed_filtered_and_time_normalized() {
        let text = "\
  101 02:03:04 /Users/example/.config/herdr-mcp/runtime/current/herdr-mcp
  202 1-00:00:05 herdr
  303 00:10 python3
  404 malformed
waggle 505 00:01 hc
  606 00:09:59 /usr/local/bin/hc
";
        let rows = parse_ps_output(text, 100_000);
        assert_eq!(
            rows,
            vec![
                ProcessRow {
                    name: "herdr-mcp".to_owned(),
                    pid: 101,
                    started_ms: Some(100_000 - (2 * 3_600 + 3 * 60 + 4) * 1_000),
                },
                ProcessRow {
                    name: "herdr".to_owned(),
                    pid: 202,
                    started_ms: Some(100_000 - (86_400 + 5) * 1_000),
                },
                ProcessRow {
                    name: "hc".to_owned(),
                    pid: 606,
                    started_ms: Some(100_000 - (9 * 60 + 59) * 1_000),
                },
            ]
        );
    }

    #[test]
    fn process_enumeration_failure_is_explicit_not_empty() {
        let dir = temp_dir("process-error");
        let probe = FakeProbe {
            processes: Err("cannot enumerate processes: ps unavailable".to_owned()),
        };
        let snapshot = snapshot_with(&dir, &probe);
        assert_eq!(snapshot["processes"], json!([]));
        assert_eq!(
            snapshot["process_error"],
            json!("cannot enumerate processes: ps unavailable")
        );
        assert_eq!(snapshot["metadata_only"], json!(true));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn state_store_and_secret_paths_are_not_in_the_allowlist() {
        assert!(!WHITELISTED_CONFIG_FILES.contains(&"state.db"));
        assert!(!WHITELISTED_CONFIG_FILES.contains(&"config.json.bak"));
        // Every allow-listed entry stays inside the instance config directory.
        for relative in WHITELISTED_CONFIG_FILES {
            assert!(
                !relative.starts_with('/') && !relative.contains(".."),
                "{relative}"
            );
        }
    }

    #[test]
    fn loopback_port_probe_detects_live_and_closed_listeners() {
        // Positive: a bound loopback listener is reported as listening.
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(SystemProbe.port_listening(port));
        drop(listener);

        // Negative: once released, the probe reports closed. Another test in
        // this workspace can be handed the just-freed ephemeral port, so retry
        // with a fresh port; five independent attempts cannot all be reused.
        for _ in 0..5 {
            let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            if !SystemProbe.port_listening(port) {
                return;
            }
        }
        panic!("no released ephemeral loopback port was observable as closed");
    }
}
