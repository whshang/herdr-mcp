use crate::herdr::HerdrClient;
use serde_json::{Map, Value, json};
use std::time::Duration;

const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SnapshotSource {
    Snapshot,
    Lists,
}

impl SnapshotSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Lists => "lists",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SnapshotResult {
    pub value: Value,
    pub source: SnapshotSource,
}

#[derive(Debug, Default)]
struct LiveCollections {
    workspaces: Option<Vec<Value>>,
    panes: Option<Vec<Value>>,
    agents: Option<Vec<Value>>,
}

pub fn fetch(client: &HerdrClient) -> Result<SnapshotResult, String> {
    match client.call_with_timeout("session.snapshot", json!({}), SNAPSHOT_TIMEOUT) {
        Ok(result) => {
            let raw = result.get("snapshot").cloned().unwrap_or(result);
            let value = reconcile_with_lists(client, raw);
            Ok(SnapshotResult {
                value,
                source: SnapshotSource::Snapshot,
            })
        }
        Err(snapshot_error) => match assemble_from_lists(client) {
            Some(value) => Ok(SnapshotResult {
                value,
                source: SnapshotSource::Lists,
            }),
            None => Err(format!(
                "session.snapshot and list APIs unavailable: {snapshot_error}"
            )),
        },
    }
}

fn reconcile_with_lists(client: &HerdrClient, snapshot: Value) -> Value {
    let mut output = snapshot.as_object().cloned().unwrap_or_default();
    let live = fetch_live_collections(client);
    replace_array(&mut output, "workspaces", live.workspaces);
    replace_array(&mut output, "panes", live.panes);
    replace_array(&mut output, "agents", live.agents);
    Value::Object(output)
}

fn assemble_from_lists(client: &HerdrClient) -> Option<Value> {
    let workspace_result = client
        .call_with_timeout("workspace.list", json!({}), SNAPSHOT_TIMEOUT)
        .ok()?;
    let workspaces = array_field(&workspace_result, "workspaces").unwrap_or_default();

    let panes = client
        .call_with_timeout("pane.list", json!({}), SNAPSHOT_TIMEOUT)
        .ok()
        .and_then(|value| array_field(&value, "panes"))
        .unwrap_or_default();
    let agents = client
        .call_with_timeout("agent.list", json!({}), SNAPSHOT_TIMEOUT)
        .ok()
        .and_then(|value| array_field(&value, "agents"))
        .unwrap_or_default();

    if workspaces.is_empty() && panes.is_empty() && agents.is_empty() {
        return None;
    }

    Some(json!({
        "type": "assembled_from_lists",
        "workspaces": workspaces,
        "panes": panes,
        "agents": agents,
    }))
}

fn fetch_live_collections(client: &HerdrClient) -> LiveCollections {
    std::thread::scope(|scope| {
        let workspaces = scope.spawn(|| {
            client
                .call_with_timeout("workspace.list", json!({}), SNAPSHOT_TIMEOUT)
                .ok()
                .and_then(|value| array_field(&value, "workspaces"))
        });
        let panes = scope.spawn(|| {
            client
                .call_with_timeout("pane.list", json!({}), SNAPSHOT_TIMEOUT)
                .ok()
                .and_then(|value| array_field(&value, "panes"))
        });
        let agents = scope.spawn(|| {
            client
                .call_with_timeout("agent.list", json!({}), SNAPSHOT_TIMEOUT)
                .ok()
                .and_then(|value| array_field(&value, "agents"))
        });

        LiveCollections {
            workspaces: workspaces.join().unwrap_or(None),
            panes: panes.join().unwrap_or(None),
            agents: agents.join().unwrap_or(None),
        }
    })
}

fn replace_array(output: &mut Map<String, Value>, key: &str, value: Option<Vec<Value>>) {
    if let Some(value) = value {
        output.insert(key.to_owned(), Value::Array(value));
    }
}

fn array_field(value: &Value, key: &str) -> Option<Vec<Value>> {
    value.get(key)?.as_array().cloned()
}

pub fn collection_count(snapshot: &Value, key: &str) -> usize {
    snapshot
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_authoritative_live_collections() {
        let mut output = json!({
            "workspaces": [{"workspace_id": "stale"}],
            "panes": [{"pane_id": "old"}],
            "other": true
        })
        .as_object()
        .cloned()
        .unwrap();

        replace_array(
            &mut output,
            "workspaces",
            Some(vec![json!({"workspace_id": "w1"})]),
        );
        replace_array(&mut output, "panes", Some(Vec::new()));
        replace_array(&mut output, "agents", None);

        assert_eq!(output["workspaces"], json!([{"workspace_id": "w1"}]));
        assert_eq!(output["panes"], json!([]));
        assert!(output.get("agents").is_none());
        assert_eq!(output["other"], true);
    }

    #[test]
    fn counts_snapshot_collections() {
        let snapshot = json!({
            "workspaces": [{}, {}],
            "panes": [{}],
        });
        assert_eq!(collection_count(&snapshot, "workspaces"), 2);
        assert_eq!(collection_count(&snapshot, "panes"), 1);
        assert_eq!(collection_count(&snapshot, "agents"), 0);
    }
}
