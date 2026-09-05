use crate::agent_visibility::AgentVisibility;
use crate::exec_sessions::ExecRegistry;
use crate::herdr::HerdrClient;
use crate::inspect;
use crate::runtime_meta;
use crate::schema::{self, MethodSchema, ValidationIssue};
use crate::skill::SkillService;
use crate::state_cache::{DigestSnapshot, EventCache};
use crate::state_store::GenerationTransitionRecord;
use serde_json::{Value, json};
use std::collections::HashSet;

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

pub fn since(
    cache: &EventCache,
    cursor: u64,
    workspace: Option<&str>,
    transition: Result<Option<GenerationTransitionRecord>, String>,
) -> Value {
    let digest = cache.digest_since(cursor);
    let visibility = AgentVisibility::from_env();
    since_result(
        cache.boot_id(),
        cursor,
        digest,
        workspace,
        &visibility,
        transition,
    )
}

pub fn methods(query: &str) -> Value {
    let mut local = crate::progressive_skills::local_method_schemas(query);
    match schema::list_methods(query) {
        Ok(methods) => {
            let mut combined = methods.iter().map(method_json).collect::<Vec<_>>();
            combined.append(&mut local);
            json!({
                "ok": true,
                "count": combined.len(),
                "methods": combined,
                "source": "herdr api schema --json (live, 60s cache)",
                "local_method_source": "herdr_mcp_local_registry",
            })
        }
        Err(error) if !local.is_empty() => json!({
            "ok": true,
            "count": local.len(),
            "methods": local,
            "source": "herdr_mcp_local_registry",
            "warnings": [{"code": "herdr_schema_unavailable", "message": error}],
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

pub fn call_with_local(
    client: &HerdrClient,
    skill: &SkillService,
    snapshot: &Value,
    method: &str,
    params: Value,
) -> Value {
    if method.starts_with("herdr_mcp.") {
        return skill.local_call(method, &params, snapshot).unwrap_or_else(|| {
            json!({
                "ok": false,
                "code": "unknown_local_method",
                "method": method,
                "message": "unknown herdr-mcp local method; request was not forwarded to the Herdr socket",
            })
        });
    }
    call(client, method, params)
}

fn method_json(method: &MethodSchema) -> Value {
    json!({
        "method": method.method,
        "source": "herdr_socket",
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
    transition: Result<Option<GenerationTransitionRecord>, String>,
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
        let current_generation = std::env::var("HERDR_MCP_RUNTIME_GENERATION").ok();
        let started_at_ms = runtime_meta::runtime_started_at_ms();
        match transition {
            Ok(Some(record))
                if record.new_generation.as_deref() == current_generation.as_deref()
                    && record.timestamp_ms.abs_diff(started_at_ms) <= 120_000 =>
            {
                output.insert(
                    "cursor_reset_reason".to_owned(),
                    json!("runtime_generation_replaced"),
                );
                output.insert(
                    "generation_transition".to_owned(),
                    serde_json::to_value(record).unwrap_or(Value::Null),
                );
                output.insert(
                    "warnings".to_owned(),
                    json!(["cursor_reset_runtime_replaced"]),
                );
            }
            Ok(_) => {
                output.insert("cursor_reset_reason".to_owned(), json!("cursor_rollover"));
                output.insert("warnings".to_owned(), json!(["cursor_reset_rollover"]));
            }
            Err(error) => {
                output.insert(
                    "cursor_reset_reason".to_owned(),
                    json!("attribution_unavailable"),
                );
                output.insert(
                    "warnings".to_owned(),
                    json!([format!("cursor_reset_attribution_unavailable: {error}")]),
                );
            }
        }
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
    fn unknown_local_method_never_reaches_herdr_socket() {
        let client = HerdrClient::new("/path/that/does/not/exist");
        let skill = SkillService::new();
        let result = call_with_local(
            &client,
            &skill,
            &json!({}),
            "herdr_mcp.skill.unknown",
            json!({}),
        );
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "unknown_local_method");
        assert!(
            result["message"]
                .as_str()
                .unwrap()
                .contains("not forwarded")
        );
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
                json!({"name": "pi-one", "kind": "pi", "workspace": "w1"}),
                json!({"name": "claude-one", "kind": "claude", "workspace": "w1"}),
                json!({"name": "pi-two", "kind": "pi", "workspace": "w2"}),
            ],
            workspaces: vec![
                json!({"workspace_id": "w1", "label": "one", "pane_count": 1, "tab_count": 1}),
                json!({"workspace_id": "w2", "label": "two", "pane_count": 2, "tab_count": 1}),
            ],
        };
        let visibility = AgentVisibility::Allow(["pi".to_owned()].into_iter().collect());
        let result = since_result("boot", 99, digest, Some("one"), &visibility, Ok(None));

        assert_eq!(result["cursor"], 7);
        assert_eq!(result["cursor_reset"], true);
        assert_eq!(result["event_count"], 1);
        assert_eq!(result["events"][0]["workspace_id"], "w1");
        assert_eq!(result["agents"].as_array().unwrap().len(), 1);
        assert_eq!(result["agents"][0]["name"], "pi-one");
        assert_eq!(result["agents"][0]["kind"], "pi");
        assert_eq!(result["workspaces"].as_array().unwrap().len(), 1);
        assert_eq!(result["workspaces"][0]["workspace_id"], "w1");
        assert_eq!(result["agents_hidden"], 1);
        assert_eq!(result["cursor_reset_reason"], "cursor_rollover");
        assert_eq!(result["warnings"], json!(["cursor_reset_rollover"]));
    }

    #[test]
    fn since_attributes_cursor_reset_to_matching_runtime_generation_transition() {
        let _guard = crate::test_env::lock();
        let previous_generation = std::env::var_os("HERDR_MCP_RUNTIME_GENERATION");
        unsafe { std::env::set_var("HERDR_MCP_RUNTIME_GENERATION", "rust-new") };
        let started_at_ms = runtime_meta::runtime_started_at_ms();
        let digest = DigestSnapshot {
            cursor: 2,
            events: Vec::new(),
            agents: Vec::new(),
            workspaces: Vec::new(),
        };
        let transition = GenerationTransitionRecord {
            timestamp_ms: started_at_ms,
            previous_generation: Some("rust-old".to_owned()),
            new_generation: Some("rust-new".to_owned()),
            previous_source_commit: Some("old-commit".to_owned()),
            new_source_commit: Some("new-commit".to_owned()),
            trigger: "dev_sync".to_owned(),
        };
        let result = since_result(
            "boot-new",
            99,
            digest,
            None,
            &AgentVisibility::All,
            Ok(Some(transition)),
        );
        assert_eq!(result["cursor_reset"], true);
        assert_eq!(result["cursor_reset_reason"], "runtime_generation_replaced");
        assert_eq!(
            result["generation_transition"]["previous_generation"],
            "rust-old"
        );
        assert_eq!(
            result["generation_transition"]["new_generation"],
            "rust-new"
        );
        assert_eq!(result["generation_transition"]["trigger"], "dev_sync");
        assert_eq!(result["warnings"], json!(["cursor_reset_runtime_replaced"]));
        unsafe {
            match previous_generation {
                Some(value) => std::env::set_var("HERDR_MCP_RUNTIME_GENERATION", value),
                None => std::env::remove_var("HERDR_MCP_RUNTIME_GENERATION"),
            }
        }
    }
}
