use crate::agent_visibility::AgentVisibility;
use crate::herdr::HerdrClient;
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
    project_snapshot(
        &snapshot_result.value,
        &pong,
        snapshot_result.source,
        &visibility,
    )
}

fn project_snapshot(
    snapshot: &Value,
    pong: &Value,
    source: SnapshotSource,
    visibility: &AgentVisibility,
) -> Value {
    let topology = projects::derive(snapshot);
    let agents_raw = array(snapshot, "agents");
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
    let agents = agents_raw.iter().map(project_agent).collect::<Vec<_>>();
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

fn project_agent(agent: &Value) -> Value {
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

    json!({
        "name": agent.get("agent").cloned().unwrap_or(Value::Null),
        "kind": agent.get("kind").cloned().or_else(|| agent.get("agent_kind").cloned()).unwrap_or(Value::Null),
        "pane": agent.get("pane_id").cloned().unwrap_or(Value::Null),
        "status": status(agent),
        "workspace": agent.get("workspace_id").cloned().unwrap_or(Value::Null),
        "cwd": cwd,
        "terminal_title": agent.get("terminal_title").cloned().unwrap_or(Value::Null),
        "state_change_seq": agent.get("state_change_seq").cloned().unwrap_or(Value::Null),
        "session_ref": session_ref,
    })
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
        assert_eq!(output["workstation_info"]["default_cwd"], "/tmp/demo");
        assert_eq!(output["workstation_info"]["managed_git_roots"], json!([]));
        assert!(output.get("warnings").is_none());
    }

    #[test]
    fn list_fallback_is_visible_as_warning() {
        let visibility = AgentVisibility::All;
        let output = project_snapshot(
            &fixture(),
            &json!({"version": "0.8.2", "protocol": 20}),
            SnapshotSource::Lists,
            &visibility,
        );
        assert_eq!(
            output["warnings"],
            json!(["snapshot_failed_used_list_apis"])
        );
    }
}
