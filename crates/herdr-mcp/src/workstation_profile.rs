use crate::capability_inventory::{AgentCapabilityRecord, CapabilityInventoryStore, Evidence};
use crate::paths::RuntimePaths;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

const PROFILE_SCHEMA_VERSION: u32 = 1;
const PROFILE_MAX_BYTES: u64 = 256 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkstationProfile {
    schema_version: u32,
    secrets_policy: String,
    #[serde(default)]
    projects: Vec<ProjectRequirement>,
    #[serde(default)]
    agents: Vec<AgentRequirement>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRequirement {
    id: String,
    remote: String,
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentRequirement {
    agent: String,
    #[serde(default)]
    available_for_start: Option<bool>,
    #[serde(default)]
    can_run_headless: Option<bool>,
    #[serde(default)]
    supports_code_edit: Option<bool>,
    #[serde(default)]
    supports_shell: Option<bool>,
    #[serde(default)]
    supports_vision: Option<bool>,
}

pub fn check(file: &str) -> Result<ExitCode, String> {
    let path = Path::new(file);
    let profile = load_profile(path)?;
    validate_profile(&profile)?;

    let paths = RuntimePaths::discover()?;
    let inventory_available = CapabilityInventoryStore::has_scan_cache(&paths.config_dir);
    let inventory = if inventory_available {
        Some(CapabilityInventoryStore::open(&paths.config_dir)?)
    } else {
        None
    };
    let mut drift = Vec::new();

    for project in &profile.projects {
        check_project(project, &mut drift);
    }
    for agent in &profile.agents {
        let record = match inventory.as_ref() {
            Some(store) => store.get(&agent.agent)?,
            None => None,
        };
        check_agent(agent, record.as_ref(), &mut drift);
    }

    let in_sync = drift.is_empty();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "schema_version": PROFILE_SCHEMA_VERSION,
            "profile": path.display().to_string(),
            "secrets_policy": profile.secrets_policy,
            "inventory_available": inventory_available,
            "in_sync": in_sync,
            "drift": drift,
            "next_action": if inventory_available { Value::Null } else { json!("herdr-mcp scan --probe") },
        }))
        .map_err(|error| format!("cannot encode workstation profile report: {error}"))?
    );
    Ok(if in_sync {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn load_profile(path: &Path) -> Result<WorkstationProfile, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "cannot inspect workstation profile {}: {error}",
            path.display()
        )
    })?;
    if !metadata.file_type().is_file() || metadata.len() > PROFILE_MAX_BYTES {
        return Err("workstation profile must be a bounded regular file".to_owned());
    }
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "cannot read workstation profile {}: {error}",
            path.display()
        )
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid workstation profile JSON: {error}"))
}

fn validate_profile(profile: &WorkstationProfile) -> Result<(), String> {
    if profile.schema_version != PROFILE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported workstation profile schema {}; expected {PROFILE_SCHEMA_VERSION}",
            profile.schema_version
        ));
    }
    if profile.secrets_policy != "reauthorize" {
        return Err("workstation profile secrets_policy must be 'reauthorize'".to_owned());
    }
    for project in &profile.projects {
        if project.id.trim().is_empty()
            || project.remote.trim().is_empty()
            || project.path.trim().is_empty()
        {
            return Err("workstation profile project id/remote/path must be non-empty".to_owned());
        }
        reject_remote_credentials(&project.remote)?;
    }
    for agent in &profile.agents {
        if agent.agent.trim().is_empty() {
            return Err("workstation profile agent name must be non-empty".to_owned());
        }
    }
    Ok(())
}

fn reject_remote_credentials(remote: &str) -> Result<(), String> {
    if let Ok(url) = url::Url::parse(remote) {
        let username = url.username();
        if url.password().is_some()
            || (!username.is_empty() && username != "git")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "workstation profile remote must not contain credentials, query, or fragment"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

fn check_project(project: &ProjectRequirement, drift: &mut Vec<Value>) {
    let path = Path::new(&project.path);
    if !path.is_dir() {
        push_drift(
            drift,
            "project",
            &project.id,
            "path",
            json!(project.path),
            Value::Null,
        );
        return;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["remote", "get-url", "origin"])
        .stdin(Stdio::null())
        .output();
    let actual = output
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned());
    if actual.as_deref() != Some(project.remote.as_str()) {
        push_drift(
            drift,
            "project",
            &project.id,
            "remote",
            json!(project.remote),
            actual.map(Value::String).unwrap_or(Value::Null),
        );
    }
}

fn check_agent(
    desired: &AgentRequirement,
    actual: Option<&AgentCapabilityRecord>,
    drift: &mut Vec<Value>,
) {
    let Some(actual) = actual else {
        push_drift(
            drift,
            "agent",
            &desired.agent,
            "inventory",
            json!("present"),
            Value::Null,
        );
        return;
    };
    check_bool(
        drift,
        &desired.agent,
        "available_for_start",
        desired.available_for_start,
        actual.available_for_start.as_ref(),
    );
    check_bool(
        drift,
        &desired.agent,
        "can_run_headless",
        desired.can_run_headless,
        actual.can_run_headless.as_ref(),
    );
    check_bool(
        drift,
        &desired.agent,
        "supports_code_edit",
        desired.supports_code_edit,
        actual.supports_code_edit.as_ref(),
    );
    check_bool(
        drift,
        &desired.agent,
        "supports_shell",
        desired.supports_shell,
        actual.supports_shell.as_ref(),
    );
    check_bool(
        drift,
        &desired.agent,
        "supports_vision",
        desired.supports_vision,
        actual.supports_vision.as_ref(),
    );
}

fn check_bool(
    drift: &mut Vec<Value>,
    agent: &str,
    field: &str,
    desired: Option<bool>,
    actual: Option<&Evidence<bool>>,
) {
    let Some(desired) = desired else {
        return;
    };
    if actual.map(|value| value.value) != Some(desired) {
        push_drift(
            drift,
            "agent",
            agent,
            field,
            json!(desired),
            actual
                .map(|value| json!(value.value))
                .unwrap_or(Value::Null),
        );
    }
}

fn push_drift(
    drift: &mut Vec<Value>,
    kind: &str,
    id: &str,
    field: &str,
    desired: Value,
    actual: Value,
) {
    drift.push(json!({
        "kind": kind,
        "id": id,
        "field": field,
        "desired": desired,
        "actual": actual,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_rejects_embedded_remote_credentials() {
        let profile = WorkstationProfile {
            schema_version: PROFILE_SCHEMA_VERSION,
            secrets_policy: "reauthorize".to_owned(),
            projects: vec![ProjectRequirement {
                id: "repo".to_owned(),
                remote: "https://token@example.com/repo.git".to_owned(),
                path: "/tmp/repo".to_owned(),
            }],
            agents: vec![],
        };
        assert!(validate_profile(&profile).is_err());
    }
}
