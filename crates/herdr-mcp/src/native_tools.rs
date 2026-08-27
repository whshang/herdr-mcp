use crate::agent_visibility::AgentVisibility;
use crate::exec_sessions::ExecRegistry;
use crate::herdr::HerdrClient;
use crate::inspect;
use crate::runtime_meta;
use crate::schema::{self, MethodSchema, ValidationIssue};
use crate::state_cache::{DigestSnapshot, EventCache};
use serde_json::{Value, json};
use std::collections::HashSet;

const SCHEMA_SOURCE: &str = "herdr api schema --json (live, 60s cache)";

pub fn inspect(
    client: &HerdrClient,
    cache: Option<&EventCache>,
    exec: Option<&ExecRegistry>,
) -> Value {
    let cached_snapshot = cache.and_then(EventCache::fresh_snapshot);
    let mut view = inspect::inspect_core(client, cached_snapshot);
    runtime_meta::augment_inspect(&mut view, cache, exec);
    view
}

pub fn since(cache: &EventCache, cursor: u64, workspace: Option<&str>) -> Value {
    let digest = cache.digest_since(cursor);
    let visibility = AgentVisibility::from_env();
    since_result(cache.boot_id(), cursor, digest, workspace, &visibility)
}

pub fn methods(query: &str) -> Value {
    match schema::list_methods(query) {
        Ok(methods) => json!({
            "ok": true,
            "count": methods.len(),
            "methods": methods.iter().map(method_json).collect::<Vec<_>>(),
            "source": SCHEMA_SOURCE,
        }),
        Err(error) => json!({
            "ok": false,
            "reason": "schema_unavailable",
            "message": error,
        }),
    }
}

pub fn call(client: &HerdrClient, method: &str, params: Value) -> Value {
    if !params.is_object() {
        return json!({
            "ok": false,
            "code": "invalid_params",
            "method": method,
            "errors": ["params must be a JSON object"],
        });
    }

    let validation = match schema::validate_method_params(method, &params) {
        Ok(validation) => validation,
        Err(error) => {
            return json!({
                "ok": false,
                "reason": "schema_unavailable",
                "method": method,
                "message": error,
            });
        }
    };

    if !validation.ok {
        return json!({
            "ok": false,
            "code": "invalid_params",
            "method": method,
            "errors": validation.errors.iter().map(issue_json).collect::<Vec<_>>(),
            "warnings": validation.warnings.iter().map(issue_json).collect::<Vec<_>>(),
        });
    }

    match client.call(method, params) {
        Ok(result) => {
            let warnings = validation
                .warnings
                .iter()
                .map(issue_json)
                .collect::<Vec<_>>();
            if warnings.is_empty() {
                json!({"ok": true, "result": result})
            } else {
                json!({"ok": true, "result": result, "warnings": warnings})
            }
        }
        Err(error) => json!({
            "ok": false,
            "code": error.code,
            "message": error.message,
            "method": method,
        }),
    }
}

fn method_json(method: &MethodSchema) -> Value {
    json!({
        "method": method.method,
        "params": {
            "properties": method.properties,
            "required": method.required,
            "empty": method.empty,
        },
    })
}

fn issue_json(issue: &ValidationIssue) -> Value {
    json!({
        "name": issue.name,
        "message": issue.message,
    })
}

fn since_result(
    boot_id: &str,
    requested_cursor: u64,
    digest: DigestSnapshot,
    workspace: Option<&str>,
    visibility: &AgentVisibility,
) -> Value {
    let mut events = digest.events;
    let mut agents = digest.agents;
    let mut workspaces = digest.workspaces;

    if let Some(workspace) = workspace {
        let mut ids = workspaces
            .iter()
            .filter(|item| {
                item.get("workspace_id").and_then(Value::as_str) == Some(workspace)
                    || item.get("label").and_then(Value::as_str) == Some(workspace)
            })
            .filter_map(|item| item.get("workspace_id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        if ids.is_empty() {
            ids.insert(workspace.to_owned());
        }
        events.retain(|event| {
            event
                .get("workspace_id")
                .and_then(Value::as_str)
                .is_some_and(|workspace_id| ids.contains(workspace_id))
        });
        agents.retain(|agent| {
            agent
                .get("workspace")
                .and_then(Value::as_str)
                .is_some_and(|workspace_id| ids.contains(workspace_id))
        });
        workspaces.retain(|item| {
            item.get("workspace_id")
                .and_then(Value::as_str)
                .is_some_and(|workspace_id| ids.contains(workspace_id))
        });
    }

    let agents_before_hide = agents.len();
    let (agents, hidden) = visibility.filter_agents(agents);
    debug_assert_eq!(agents_before_hide.saturating_sub(agents.len()), hidden);
    let cursor_reset = requested_cursor > digest.cursor;
    let mut output = serde_json::Map::new();
    output.insert("ok".to_owned(), json!(true));
    output.insert("boot_id".to_owned(), json!(boot_id));
    output.insert("cursor".to_owned(), json!(digest.cursor));
    output.insert("cursor_reset".to_owned(), json!(cursor_reset));
    output.insert("event_count".to_owned(), json!(events.len()));
    output.insert("events".to_owned(), Value::Array(events));
    output.insert("agents".to_owned(), Value::Array(agents));
    output.insert(
        "workspaces".to_owned(),
        Value::Array(
            workspaces
                .iter()
                .map(|workspace| {
                    json!({
                        "workspace_id": workspace.get("workspace_id").cloned().unwrap_or(Value::Null),
                        "label": workspace.get("label").cloned().unwrap_or(Value::Null),
                        "cwd": workspace.get("cwd").cloned().unwrap_or(Value::Null),
                        "panes": workspace.get("pane_count").cloned().unwrap_or(Value::Null),
                        "tabs": workspace.get("tab_count").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect(),
        ),
    );
    visibility.append_meta(&mut output, hidden);
    if cursor_reset {
        output.insert(
            "warnings".to_owned(),
            json!(["cursor_reset_boot_or_rollover"]),
        );
    }
    output.insert(
        "hint".to_owned(),
        json!("save boot_id+cursor; if boot_id changes or cursor_reset=true, start from cursor 0"),
    );
    Value::Object(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_object_params_before_schema_or_socket() {
        let client = HerdrClient::new("/path/that/does/not/exist");
        let result = call(&client, "ping", json!([1, 2, 3]));
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "invalid_params");
    }

    #[test]
    fn since_filters_workspace_visibility_and_resets_future_cursor() {
        let digest = DigestSnapshot {
            cursor: 7,
            events: vec![
                json!({"cursor": 6, "workspace_id": "w1"}),
                json!({"cursor": 7, "workspace_id": "w2"}),
            ],
            agents: vec![
                json!({"name": "pi", "workspace": "w1"}),
                json!({"name": "claude", "workspace": "w1"}),
                json!({"name": "pi", "workspace": "w2"}),
            ],
            workspaces: vec![
                json!({"workspace_id": "w1", "label": "one", "pane_count": 1, "tab_count": 1}),
                json!({"workspace_id": "w2", "label": "two", "pane_count": 2, "tab_count": 1}),
            ],
        };
        let visibility = AgentVisibility::Allow(["pi".to_owned()].into_iter().collect());
        let result = since_result("boot", 99, digest, Some("one"), &visibility);

        assert_eq!(result["cursor"], 7);
        assert_eq!(result["cursor_reset"], true);
        assert_eq!(result["event_count"], 1);
        assert_eq!(result["events"][0]["workspace_id"], "w1");
        assert_eq!(result["agents"].as_array().unwrap().len(), 1);
        assert_eq!(result["agents"][0]["name"], "pi");
        assert_eq!(result["workspaces"].as_array().unwrap().len(), 1);
        assert_eq!(result["workspaces"][0]["workspace_id"], "w1");
        assert_eq!(result["agents_hidden"], 1);
        assert_eq!(result["warnings"], json!(["cursor_reset_boot_or_rollover"]));
    }
}
