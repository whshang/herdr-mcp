use crate::child_process;
use semver::Version;
use serde_json::{Value, json};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_OUTPUT: usize = 32 * 1024;

#[derive(Debug, Clone, Default)]
pub(crate) struct HerdrCliProbe {
    pub(crate) cli_version: Option<String>,
    pub(crate) machine_forwarding: Option<bool>,
    pub(crate) saved_machine_count: Option<usize>,
    pub(crate) saved_machines: Option<Vec<Value>>,
}

pub(crate) fn probe_cli() -> HerdrCliProbe {
    let Some(binary) = discover_binary() else {
        return HerdrCliProbe::default();
    };

    let cli_version = run_text(&binary, &["--version"])
        .and_then(|text| first_nonempty_line(&text).map(str::to_owned));
    let help = run_text(&binary, &["--help"]);
    let machine_forwarding = help.as_deref().map(machine_forwarding_from_help);

    let (saved_machine_count, saved_machines) = if machine_forwarding == Some(true) {
        machine_inventory(&binary)
    } else {
        (None, None)
    };

    HerdrCliProbe {
        cli_version,
        machine_forwarding,
        saved_machine_count,
        saved_machines,
    }
}

pub(crate) fn project_runtime(
    probe: &HerdrCliProbe,
    server_version: Option<&str>,
    pane_count: usize,
) -> Value {
    let cli_semver = probe.cli_version.as_deref().and_then(extract_semver);
    let server_semver = server_version.and_then(extract_semver);
    let state = match (&cli_semver, &server_semver) {
        (Some(cli), Some(server)) if cli == server => "current",
        (Some(cli), Some(server)) if cli > server => "server_restart_pending",
        (Some(_), Some(_)) => "version_mismatch",
        _ => "unknown",
    };
    let handoff_blocked_reason = match (&cli_semver, &server_semver) {
        (Some(cli), Some(server))
            if *cli >= Version::new(0, 9, 1)
                && *server < Version::new(0, 9, 1)
                && pane_count > 64 =>
        {
            Some("legacy_sender_pane_limit")
        }
        _ => None,
    };

    json!({
        "cli_version": probe.cli_version,
        "server_version": server_version,
        "version_state": state,
        "machine_forwarding": probe.machine_forwarding,
        "saved_machine_count": probe.saved_machine_count,
        "saved_machines": probe.saved_machines,
        "handoff_blocked_reason": handoff_blocked_reason,
        "routing_boundary": "edge_device_and_saved_machine_are_separate",
        "cross_transport_retry": "not_delivered_or_live_proof_only",
    })
}

fn discover_binary() -> Option<PathBuf> {
    let home = env::var_os("HOME").map(PathBuf::from);
    [
        env::var_os("HERDR_BIN").map(PathBuf::from),
        home.as_ref().map(|home| home.join(".local/bin/herdr")),
        Some(PathBuf::from("/opt/homebrew/bin/herdr")),
        Some(PathBuf::from("/usr/local/bin/herdr")),
    ]
    .into_iter()
    .flatten()
    .find(|path| path.is_file())
}

fn run_text(binary: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new(binary);
    command.args(args);
    let output =
        child_process::run_bounded_output(&mut command, PROBE_TIMEOUT, MAX_OUTPUT).ok()??;
    if !output.status.success() || output.truncated {
        return None;
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stderr).into_owned();
    }
    Some(text)
}

fn machine_inventory(binary: &Path) -> (Option<usize>, Option<Vec<Value>>) {
    let Some(text) = run_text(binary, &["machine", "list", "--json"]) else {
        return (None, None);
    };
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(&text) else {
        return (None, None);
    };
    let projected = project_machine_entries(&entries);
    (Some(projected.len()), Some(projected))
}

fn project_machine_entries(entries: &[Value]) -> Vec<Value> {
    entries
        .iter()
        .map(|entry| {
            json!({
                "id": entry.get("id").cloned().unwrap_or(Value::Null),
                "label": entry.get("label").cloned().unwrap_or(Value::Null),
                "session": entry.get("session").cloned().unwrap_or(Value::Null),
                "enabled": entry.get("enabled").cloned().unwrap_or(Value::Null),
            })
        })
        .collect()
}

fn first_nonempty_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

fn extract_semver(text: &str) -> Option<Version> {
    text.split_whitespace()
        .find_map(|token| Version::parse(token.trim_start_matches('v')).ok())
}

fn machine_forwarding_from_help(text: &str) -> bool {
    text.contains("--machine <label-or-id>")
        && text.contains("Run an API command on a saved SSH machine")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_new_server_old_is_restart_pending() {
        let probe = HerdrCliProbe {
            cli_version: Some("herdr 0.9.1".to_owned()),
            machine_forwarding: Some(true),
            saved_machine_count: Some(0),
            saved_machines: Some(vec![]),
        };
        let value = project_runtime(&probe, Some("0.9.0"), 67);
        assert_eq!(value["version_state"], "server_restart_pending");
        assert_eq!(value["handoff_blocked_reason"], "legacy_sender_pane_limit");
    }

    #[test]
    fn unavailable_cli_keeps_capability_unknown() {
        let probe = HerdrCliProbe::default();
        let value = project_runtime(&probe, Some("0.9.0"), 1);
        assert!(value["machine_forwarding"].is_null());
        assert_eq!(value["version_state"], "unknown");
        assert_eq!(
            value["cross_transport_retry"],
            "not_delivered_or_live_proof_only"
        );
    }

    #[test]
    fn older_help_reports_machine_forwarding_false() {
        assert!(!machine_forwarding_from_help(
            "Usage: herdr [options]\n  --remote <target> Attach through SSH"
        ));
    }

    #[test]
    fn current_help_reports_machine_forwarding_true() {
        assert!(machine_forwarding_from_help(
            "Usage: herdr --machine <label-or-id> <command>\n  --machine <label-or-id>  Run an API command on a saved SSH machine"
        ));
    }

    #[test]
    fn saved_machine_projection_redacts_transport_and_credentials() {
        let projected = project_machine_entries(&[json!({
            "id": "machine-1",
            "label": "r5c",
            "target": "user@private-host",
            "session": "default",
            "enabled": true,
            "credential": "must-not-leak"
        })]);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0]["id"], "machine-1");
        assert_eq!(projected[0]["label"], "r5c");
        assert_eq!(projected[0]["session"], "default");
        assert!(projected[0].get("target").is_none());
        assert!(projected[0].get("credential").is_none());
    }

    #[test]
    fn older_server_stays_pending_without_claiming_current() {
        let probe = HerdrCliProbe {
            cli_version: Some("herdr 0.9.1".to_owned()),
            machine_forwarding: Some(true),
            saved_machine_count: None,
            saved_machines: None,
        };
        let value = project_runtime(&probe, Some("0.7.0"), 70);
        assert_eq!(value["version_state"], "server_restart_pending");
        assert_eq!(value["handoff_blocked_reason"], "legacy_sender_pane_limit");
    }
}
