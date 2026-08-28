use crate::agent_visibility::AgentVisibility;
use crate::capability_inventory::{AgentCapabilityRecord, CapabilityInventoryStore, ProbeLevel};
use crate::capability_resolver::{WorkerCapability, project_capabilities_with_inventory};
use crate::herdr::HerdrClient;
use crate::paths::RuntimePaths;
use crate::projects::{self, ProjectInfo, ProjectTopology};
use crate::snapshot::{self, SnapshotSource};
use crate::workstation;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

pub fn inspect_core(client: &HerdrClient, cached_snapshot: Option<Value>) -> Value {
    let pong = match client.ping() {
        Ok(value) => value,
        Err(error) => {
            return json!({
                "ok": false,
                "code": error.code,
                "message": error.message,
                "context": "herdr_inspect",
                "failure_phase": "ping",
            });
        }
    };

    let snapshot_result = if let Some(value) = cached_snapshot {
        snapshot::SnapshotResult {
            value,
            source: SnapshotSource::Cache,
        }
    } else {
        match snapshot::fetch(client) {
            Ok(result) => result,
            Err(error) => {
                return json!({
                    "ok": false,
                    "code": "snapshot_unavailable",
                    "message": error,
                    "context": "herdr_inspect",
                    "failure_phase": "snapshot",
                });
            }
        }
    };

    let visibility = AgentVisibility::from_env();
    let inventory = RuntimePaths::discover()
        .ok()
        .and_then(|paths| CapabilityInventoryStore::load_existing(&paths.config_dir).ok())
        .unwrap_or_default();
    project_snapshot(
        &snapshot_result.value,
        &pong,
        snapshot_result.source,
        &visibility,
        &inventory,
    )
}

fn project_snapshot(
    snapshot: &Value,
    pong: &Value,
    source: SnapshotSource,
    visibility: &AgentVisibility,
    inventory: &[AgentCapabilityRecord],
) -> Value {
    let topology = projects::derive(snapshot);
    let agents_raw = array(snapshot, "agents");
    let capability_snapshot = project_capabilities_with_inventory(snapshot, visibility, inventory);
    let inventory_by_agent = inventory
        .iter()
        .map(|record| (record.agent.as_str(), record))
        .collect::<HashMap<_, _>>();
    let capability_by_pane = capability_snapshot
        .workers
        .iter()
        .filter_map(|worker| {
            let pane_id = worker.pane_id.as_ref()?;
            let record = worker
                .kind
                .as_deref()
                .and_then(|kind| inventory_by_agent.get(kind).copied());
            Some((pane_id.clone(), project_capability(worker, record)))
        })
        .collect::<HashMap<_, _>>();
    let agent_by_pane = agents_raw
        .iter()
        .filter_map(|agent| {
            let pane_id = agent.get("pane_id")?.as_str()?;
            Some((pane_id.to_owned(), agent.clone()))
        })
        .collect::<HashMap<_, _>>();

    let workspaces = array(snapshot, "workspaces")
        .iter()
        .map(|workspace| project_workspace(workspace, &topology))
        .collect::<Vec<_>>();
    let tabs = array(snapshot, "tabs")
        .iter()
        .map(project_tab)
        .collect::<Vec<_>>();
    let panes = array(snapshot, "panes")
        .iter()
        .map(|pane| project_pane(pane, &agent_by_pane))
        .collect::<Vec<_>>();
    let agents = agents_raw
        .iter()
        .map(|agent| project_agent(agent, &capability_by_pane))
        .collect::<Vec<_>>();
    let panes = visibility.redact_panes(panes);
    let (agents, hidden_agents) = visibility.filter_agents(agents);

    let mut output = Map::new();
    copy(
        snapshot,
        &mut output,
        "focused_workspace_id",
        "focused_workspace",
    );
    copy(snapshot, &mut output, "focused_pane_id", "focused_pane");
    output.insert("workspaces".to_owned(), Value::Array(workspaces));
    output.insert("tabs".to_owned(), Value::Array(tabs));
    output.insert("panes".to_owned(), Value::Array(panes));
    output.insert("agents".to_owned(), Value::Array(agents));
    output.insert(
        "shared_projects".to_owned(),
        Value::Array(project_shared_projects(&topology)),
    );
    output.insert("ok".to_owned(), Value::Bool(true));
    output.insert(
        "herdr_version".to_owned(),
        pong.get("version").cloned().unwrap_or(Value::Null),
    );
    output.insert(
        "protocol".to_owned(),
        pong.get("protocol").cloned().unwrap_or(Value::Null),
    );
    visibility.append_meta(&mut output, hidden_agents);
    let visible_scanned_workers = capability_snapshot
        .workers
        .iter()
        .filter(|worker| {
            worker
                .kind
                .as_deref()
                .is_some_and(|kind| inventory_by_agent.contains_key(kind))
        })
        .count();
    let available_total = inventory
        .iter()
        .filter(|record| {
            record
                .available_for_start
                .as_ref()
                .is_some_and(|evidence| evidence.value)
        })
        .count();
    let available_agents = inventory
        .iter()
        .filter(|record| {
            record
                .available_for_start
                .as_ref()
                .is_some_and(|evidence| evidence.value)
                && visibility.is_visible(None, Some(record.agent.as_str()))
        })
        .map(|record| {
            json!({
                "kind": record.agent,
                "binary_version": record.binary_version.as_ref().map(|value| value.value.clone()),
                "can_run_headless": record.can_run_headless.as_ref().map(|value| value.value),
                "supports_code_edit": record.supports_code_edit.as_ref().map(|value| value.value),
                "supports_shell": record.supports_shell.as_ref().map(|value| value.value),
            })
        })
        .collect::<Vec<_>>();
    let hidden_available_agents = available_total.saturating_sub(available_agents.len());
    output.insert(
        "capability_inventory".to_owned(),
        json!({
            "source": if inventory.is_empty() { "not_scanned" } else { "scan_cache" },
            "record_count": inventory.len(),
            "visible_worker_records": visible_scanned_workers,
            "available_agents": available_agents,
            "hidden_available_agents": hidden_available_agents,
            "unknown_semantics": "absent capability fields are unverified, never inferred",
            "refresh": "herdr-mcp scan --probe",
        }),
    );
    if source == SnapshotSource::Lists {
        output.insert(
            "warnings".to_owned(),
            json!(["snapshot_failed_used_list_apis"]),
        );
    }
    let mut view = Value::Object(output);
    let workstation_info = workstation::info(&view, &topology);
    if let Some(object) = view.as_object_mut() {
        object.insert("workstation_info".to_owned(), workstation_info);
    }
    view
}

fn project_workspace(workspace: &Value, topology: &ProjectTopology) -> Value {
    let worktree = workspace.get("worktree").and_then(Value::as_object);
    let workspace_id = workspace
        .get("workspace_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let cwd = workspace
        .get("cwd")
        .cloned()
        .or_else(|| {
            worktree
                .and_then(|value| value.get("checkout_path"))
                .cloned()
        })
        .or_else(|| worktree.and_then(|value| value.get("path")).cloned())
        .unwrap_or(Value::Null);
    let project_list = topology
        .projects
        .values()
        .filter(|project| {
            project.pane_ids.iter().any(|pane| {
                topology
                    .pane_to_workspace
                    .get(pane)
                    .is_some_and(|value| value == workspace_id)
            })
        })
        .map(|project| project_json(project, topology, Some(workspace_id)))
        .collect::<Vec<_>>();
    let heterogeneous = project_list.len() > 1;

    json!({
        "id": workspace.get("workspace_id").cloned().unwrap_or(Value::Null),
        "label": workspace.get("label").cloned().unwrap_or(Value::Null),
        "cwd": cwd,
        "tabs": workspace.get("tab_count").cloned().unwrap_or(Value::Null),
        "panes": workspace.get("pane_count").cloned().unwrap_or(Value::Null),
        "focused": workspace.get("focused").cloned().unwrap_or(Value::Bool(false)),
        "projects": project_list,
        "heterogeneous": heterogeneous,
    })
}

fn project_json(
    project: &ProjectInfo,
    topology: &ProjectTopology,
    current_workspace: Option<&str>,
) -> Value {
    let mut also_open_in = projects::workspaces_for_root(topology, &project.root);
    if let Some(current_workspace) = current_workspace {
        also_open_in.retain(|workspace| workspace != current_workspace);
    }
    json!({
        "root": project.root.to_string_lossy(),
        "pane_ids": project.pane_ids,
        "dirty": project.dirty,
        "changed_files": project.changed_files,
        "vcs": project.vcs,
        "managed": project.managed,
        "also_open_in": also_open_in,
    })
}

fn project_shared_projects(topology: &ProjectTopology) -> Vec<Value> {
    topology
        .projects
        .values()
        .filter_map(|project| {
            let workspace_ids = projects::workspaces_for_root(topology, &project.root);
            if workspace_ids.len() < 2 {
                return None;
            }
            Some(json!({
                "root": project.root.to_string_lossy(),
                "workspace_ids": workspace_ids,
                "pane_ids": project.pane_ids,
                "dirty": project.dirty,
                "managed": project.managed,
                "vcs": project.vcs,
            }))
        })
        .collect()
}

fn project_tab(tab: &Value) -> Value {
    json!({
        "id": tab.get("tab_id").cloned().unwrap_or(Value::Null),
        "workspace": tab.get("workspace_id").cloned().unwrap_or(Value::Null),
        "label": tab.get("label").cloned().unwrap_or(Value::Null),
    })
}

fn project_pane(pane: &Value, agent_by_pane: &HashMap<String, Value>) -> Value {
    let pane_id = pane.get("pane_id").and_then(Value::as_str).unwrap_or("");
    let agent = agent_by_pane
        .get(pane_id)
        .map(project_pane_agent)
        .unwrap_or(Value::Null);
    let cwd = pane
        .get("cwd")
        .cloned()
        .or_else(|| pane.get("foreground_cwd").cloned())
        .unwrap_or(Value::Null);

    json!({
        "id": pane.get("pane_id").cloned().unwrap_or(Value::Null),
        "workspace": pane.get("workspace_id").cloned().unwrap_or(Value::Null),
        "cwd": cwd,
        "agent": agent,
    })
}

fn project_pane_agent(agent: &Value) -> Value {
    json!({
        "name": agent.get("agent").cloned().unwrap_or(Value::Null),
        "status": status(agent),
        "terminal_title": agent.get("terminal_title").cloned().unwrap_or(Value::Null),
        "state_change_seq": agent.get("state_change_seq").cloned().unwrap_or(Value::Null),
    })
}

fn project_agent(agent: &Value, capability_by_pane: &HashMap<String, Value>) -> Value {
    let session_ref = agent
        .get("agent_session")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or(Value::Null);
    let cwd = agent
        .get("cwd")
        .cloned()
        .or_else(|| agent.get("foreground_cwd").cloned())
        .unwrap_or(Value::Null);
    let capability = agent
        .get("pane_id")
        .and_then(Value::as_str)
        .and_then(|pane_id| capability_by_pane.get(pane_id))
        .cloned()
        .unwrap_or(Value::Null);

    json!({
        "name": agent.get("agent").cloned().unwrap_or(Value::Null),
        "kind": agent.get("kind").cloned().or_else(|| agent.get("agent_kind").cloned()).unwrap_or(Value::Null),
        "pane": agent.get("pane_id").cloned().unwrap_or(Value::Null),
        "status": status(agent),
        "workspace": agent.get("workspace_id").cloned().unwrap_or(Value::Null),
        "cwd": cwd,
        "terminal_title": agent.get("terminal_title").cloned().unwrap_or(Value::Null),
        "state_change_seq": agent.get("state_change_seq").cloned().unwrap_or(Value::Null),
        "interactive_ready": agent.get("interactive_ready").cloned().unwrap_or(Value::Null),
        "session_ref": session_ref,
        "capabilities": capability,
    })
}

fn project_capability(worker: &WorkerCapability, record: Option<&AgentCapabilityRecord>) -> Value {
    let mut verified = Map::new();
    macro_rules! put {
        ($name:literal, $value:expr) => {
            if let Some(value) = $value {
                verified.insert($name.to_owned(), json!(value));
            }
        };
    }
    put!("provider", worker.provider.as_deref());
    put!("model", worker.model.as_deref());
    put!("profile", worker.profile.as_deref());
    put!("supports_code_edit", worker.supports_code_edit);
    put!("supports_shell", worker.supports_shell);
    put!("supports_vision", worker.supports_vision);
    put!("reasoning_tier", worker.reasoning_tier);
    put!("latency_tier", worker.latency_tier);
    put!("cost_tier", worker.cost_tier);
    put!("context_tier", worker.context_tier);
    put!("interactive_only", worker.interactive_only);
    put!("can_run_headless", worker.can_run_headless);

    let mut value = Map::new();
    value.insert(
        "source".to_owned(),
        json!(if record.is_some() {
            "capability_inventory"
        } else {
            "live_only"
        }),
    );
    value.insert("verified".to_owned(), Value::Object(verified));
    if let Some(record) = record {
        value.insert(
            "manifest_version".to_owned(),
            record
                .manifest_version
                .as_deref()
                .map_or(Value::Null, |value| json!(value)),
        );
        value.insert(
            "agent_version".to_owned(),
            record
                .binary_version
                .as_ref()
                .map(|evidence| json!(evidence.value))
                .unwrap_or(Value::Null),
        );
        value.insert(
            "probe_level".to_owned(),
            json!(match record.probe_level {
                ProbeLevel::Version => "version",
                ProbeLevel::Deep => "deep",
            }),
        );
        value.insert("observed_at_ms".to_owned(), json!(record.observed_at_ms));
    }
    Value::Object(value)
}

fn status(value: &Value) -> Value {
    value
        .get("agent_status")
        .cloned()
        .or_else(|| value.get("status").cloned())
        .unwrap_or(Value::Null)
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn copy(input: &Value, output: &mut Map<String, Value>, from: &str, to: &str) {
    output.insert(
        to.to_owned(),
        input.get(from).cloned().unwrap_or(Value::Null),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        json!({
            "focused_workspace_id": "w1",
            "focused_pane_id": "w1:p1",
            "workspaces": [{
                "workspace_id": "w1",
                "label": "demo",
                "focused": true,
                "pane_count": 1,
                "tab_count": 1,
                "worktree": {"checkout_path": "/tmp/demo"}
            }],
            "tabs": [{
                "tab_id": "w1:t1",
                "workspace_id": "w1",
                "label": "1"
            }],
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": "/tmp/demo"
            }],
            "agents": [{
                "agent": "pi",
                "agent_status": "working",
                "workspace_id": "w1",
                "pane_id": "w1:p1",
                "cwd": "/tmp/demo",
                "state_change_seq": 42,
                "agent_session": {"source": "herdr:pi", "kind": "path", "value": "session.jsonl"}
            }]
        })
    }

    #[test]
    fn projects_current_snapshot_shape() {
        let visibility = AgentVisibility::Allow(["pi".to_owned()].into_iter().collect());
        let output = project_snapshot(
            &fixture(),
            &json!({"version": "0.8.2", "protocol": 20}),
            SnapshotSource::Snapshot,
            &visibility,
            &[],
        );

        assert_eq!(output["ok"], true);
        assert_eq!(output["focused_workspace"], "w1");
        assert_eq!(output["focused_pane"], "w1:p1");
        assert_eq!(output["workspaces"][0]["id"], "w1");
        assert_eq!(output["workspaces"][0]["cwd"], "/tmp/demo");
        assert_eq!(output["workspaces"][0]["projects"][0]["root"], "/tmp/demo");
        assert_eq!(output["workspaces"][0]["projects"][0]["managed"], false);
        assert_eq!(output["tabs"][0]["workspace"], "w1");
        assert_eq!(output["panes"][0]["agent"]["name"], "pi");
        assert_eq!(output["agents"][0]["status"], "working");
        assert_eq!(output["agents"][0]["session_ref"]["source"], "herdr:pi");
        assert_eq!(output["herdr_version"], "0.8.2");
        assert_eq!(output["protocol"], 20);
        assert_eq!(output["agent_visibility"], "allowlist");
        assert_eq!(output["agents_hidden"], 0);
        assert_eq!(output["capability_inventory"]["source"], "not_scanned");
        assert_eq!(output["agents"][0]["capabilities"]["source"], "live_only");
        assert_eq!(output["workstation_info"]["default_cwd"], "/tmp/demo");
        assert_eq!(output["workstation_info"]["managed_git_roots"], json!([]));
        assert!(output.get("warnings").is_none());
    }

    fn scanned_record(agent: &str) -> AgentCapabilityRecord {
        use crate::capability_inventory::{Evidence, INVENTORY_SCHEMA_VERSION};
        AgentCapabilityRecord {
            schema_version: INVENTORY_SCHEMA_VERSION,
            agent: agent.to_owned(),
            manifest_version: Some("manifest-1".to_owned()),
            manifest_source: Some("bundled".to_owned()),
            manifest_source_kind: Some("bundled".to_owned()),
            binary_path: Some(format!("/bin/{agent}")),
            herdr_startable: Some(Evidence {
                value: true,
                source: "herdr_cli:agent_start_help".to_owned(),
                authority: "herdr_declared".to_owned(),
                observed_at_ms: 7,
                detail: None,
            }),
            executable_available: Some(Evidence {
                value: true,
                source: "path_lookup".to_owned(),
                authority: "observed".to_owned(),
                observed_at_ms: 7,
                detail: Some(format!("/bin/{agent}")),
            }),
            available_for_start: Some(Evidence {
                value: true,
                source: "herdr_start_kind+path_lookup".to_owned(),
                authority: "derived".to_owned(),
                observed_at_ms: 7,
                detail: None,
            }),
            binary_version: Some(Evidence {
                value: format!("{agent} 1.2.3"),
                source: "cli_version_probe".to_owned(),
                authority: "reported".to_owned(),
                observed_at_ms: 7,
                detail: None,
            }),
            provider: None,
            model: None,
            profile: None,
            supports_code_edit: None,
            supports_shell: None,
            supports_vision: None,
            reasoning_tier: None,
            latency_tier: None,
            cost_tier: None,
            context_tier: None,
            interactive_only: None,
            can_run_headless: Some(Evidence {
                value: true,
                source: "cli_help_probe".to_owned(),
                authority: "reported".to_owned(),
                observed_at_ms: 7,
                detail: Some("help advertises 'headless'".to_owned()),
            }),
            probe_level: ProbeLevel::Deep,
            probe_adapter_version: 1,
            fingerprint: format!("sha256:{agent}"),
            observed_at_ms: 7,
        }
    }

    #[test]
    fn inspect_projects_only_visible_scanned_capabilities() {
        let mut snapshot = fixture();
        snapshot["agents"].as_array_mut().unwrap().push(json!({
            "agent": "claude",
            "agent_status": "idle",
            "workspace_id": "w1",
            "pane_id": "w1:p2",
            "cwd": "/tmp/demo",
            "state_change_seq": 43
        }));
        let visibility = AgentVisibility::Allow(["pi".to_owned()].into_iter().collect());
        let output = project_snapshot(
            &snapshot,
            &json!({"version": "0.8.2", "protocol": 20}),
            SnapshotSource::Snapshot,
            &visibility,
            &[scanned_record("pi"), scanned_record("claude")],
        );
        assert_eq!(output["agents"].as_array().unwrap().len(), 1);
        assert_eq!(output["agents_hidden"], 1);
        assert_eq!(output["capability_inventory"]["visible_worker_records"], 1);
        assert_eq!(output["capability_inventory"]["record_count"], 2);
        assert_eq!(
            output["capability_inventory"]["available_agents"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            output["capability_inventory"]["available_agents"][0]["kind"],
            "pi"
        );
        assert_eq!(output["capability_inventory"]["hidden_available_agents"], 1);
        assert_eq!(
            output["agents"][0]["capabilities"]["source"],
            "capability_inventory"
        );
        assert_eq!(
            output["agents"][0]["capabilities"]["verified"]["can_run_headless"],
            true
        );
        assert_eq!(
            output["agents"][0]["capabilities"]["agent_version"],
            "pi 1.2.3"
        );
        assert!(output.to_string().find("claude 1.2.3").is_none());
    }

    #[test]
    fn list_fallback_is_visible_as_warning() {
        let visibility = AgentVisibility::All;
        let output = project_snapshot(
            &fixture(),
            &json!({"version": "0.8.2", "protocol": 20}),
            SnapshotSource::Lists,
            &visibility,
            &[],
        );
        assert_eq!(
            output["warnings"],
            json!(["snapshot_failed_used_list_apis"])
        );
    }
}
