use crate::agent_visibility::AgentVisibility;
use crate::exec_sessions::ExecRegistry;
use crate::herdr::{HerdrClient, HerdrError};
use crate::inspect;
use crate::runtime_meta;
use crate::schema::{self, MethodSchema, ValidationIssue};
use crate::skill::SkillService;
use crate::state_cache::{DigestSnapshot, EventCache};
use crate::state_store::GenerationTransitionRecord;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::Duration;

const AGENT_STATE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

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
    for method in &mut local {
        if let Some(object) = method.as_object_mut() {
            object.insert("route".to_owned(), json!("workstation_local"));
        }
    }
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
    if let Some(error) = target_kind_preflight(snapshot, method, &params) {
        return error;
    }
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

    if method == "pane.close" {
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
        if let Some(pane_id) = params.get("pane_id").and_then(Value::as_str)
            && let Some(blocked) = pane_close_guard(client, snapshot, pane_id)
        {
            return blocked;
        }
    }

    let mut result = call(client, method, params.clone());
    if method == "worktree.remove"
        && result.get("ok").and_then(Value::as_bool) == Some(false)
        && result.get("code").and_then(Value::as_str) == Some("not_linked_worktree")
    {
        result = reconcile_historical_linked_worktree_remove(client, snapshot, &params, result);
    }
    annotate_control_semantics(method, &params, &mut result);
    result
}

fn target_kind_preflight(snapshot: &Value, method: &str, params: &Value) -> Option<Value> {
    if method != "agent.read" {
        return None;
    }
    let target = params.get("target").and_then(Value::as_str)?;
    let pane = snapshot
        .get("panes")
        .and_then(Value::as_array)?
        .iter()
        .find(|pane| pane.get("pane_id").and_then(Value::as_str) == Some(target))?;
    let has_agent = pane
        .get("agent")
        .and_then(Value::as_str)
        .is_some_and(|agent| !agent.trim().is_empty());
    if has_agent {
        return None;
    }
    Some(json!({
        "ok": false,
        "code": "target_kind_mismatch",
        "method": method,
        "target": target,
        "target_kind": "utility_pane",
        "correct_method": "pane.read",
        "message": format!(
            "agent.read target {target} is a pane without an Agent; use pane.read with pane_id={target}"
        ),
    }))
}

fn reconcile_historical_linked_worktree_remove(
    client: &HerdrClient,
    snapshot: &Value,
    remove_params: &Value,
    original_error: Value,
) -> Value {
    reconcile_historical_linked_worktree_remove_with(
        snapshot,
        remove_params,
        original_error,
        |method, params| client.call(method, params),
    )
}

fn reconcile_historical_linked_worktree_remove_with<Call>(
    snapshot: &Value,
    remove_params: &Value,
    original_error: Value,
    mut call_native: Call,
) -> Value
where
    Call: FnMut(&str, Value) -> Result<Value, HerdrError>,
{
    let Some(workspace_id) = remove_params.get("workspace_id").and_then(Value::as_str) else {
        return original_error;
    };
    let topology = crate::projects::derive_routing(snapshot);
    let projects = crate::projects::projects_for_workspace(&topology, workspace_id);
    let [project] = projects.as_slice() else {
        return original_error;
    };
    if !project.managed || project.vcs != Some("git") {
        return original_error;
    }

    let list = match call_native("worktree.list", json!({"workspace_id": workspace_id})) {
        Ok(list) => list,
        Err(_) => return original_error,
    };
    let target_path = project.root.to_string_lossy();
    let matching = list
        .get("worktrees")
        .and_then(Value::as_array)
        .and_then(|worktrees| {
            worktrees.iter().find(|worktree| {
                worktree.get("path").and_then(Value::as_str) == Some(target_path.as_ref())
                    && worktree.get("is_linked_worktree").and_then(Value::as_bool) == Some(true)
                    && worktree.get("open_workspace_id").and_then(Value::as_str)
                        == Some(workspace_id)
            })
        });
    if matching.is_none() {
        return original_error;
    }

    let source = list.get("source").and_then(Value::as_object);
    let mut open_params = json!({
        "path": target_path.as_ref(),
        "focus": false,
    });
    if let Some(source_workspace_id) = source
        .and_then(|source| source.get("source_workspace_id"))
        .and_then(Value::as_str)
    {
        open_params["workspace_id"] = json!(source_workspace_id);
    } else if let Some(source_checkout_path) = source
        .and_then(|source| source.get("source_checkout_path"))
        .and_then(Value::as_str)
    {
        open_params["cwd"] = json!(source_checkout_path);
    } else {
        return original_error;
    }

    let opened = match call_native("worktree.open", open_params) {
        Ok(opened) => opened,
        Err(_) => return original_error,
    };
    let opened_workspace = opened
        .get("workspace")
        .and_then(|workspace| workspace.get("workspace_id"))
        .and_then(Value::as_str);
    let opened_worktree = opened.get("worktree").and_then(Value::as_object);
    let reconciled = opened_workspace == Some(workspace_id)
        && opened_worktree
            .and_then(|worktree| worktree.get("path"))
            .and_then(Value::as_str)
            == Some(target_path.as_ref())
        && opened_worktree
            .and_then(|worktree| worktree.get("is_linked_worktree"))
            .and_then(Value::as_bool)
            == Some(true);
    if !reconciled {
        return original_error;
    }

    match call_native("worktree.remove", remove_params.clone()) {
        Ok(result) => json!({
            "ok": true,
            "result": result,
            "compatibility_reconciled": {
                "kind": "historical_linked_worktree_membership",
                "workspace_id": workspace_id,
                "path": target_path,
            }
        }),
        Err(error) => json!({
            "ok": false,
            "code": error.code,
            "message": error.message,
            "method": "worktree.remove",
        }),
    }
}

fn pane_close_guard(client: &HerdrClient, snapshot: &Value, pane_id: &str) -> Option<Value> {
    let live = client.call_with_timeout(
        "agent.get",
        json!({"target": pane_id}),
        AGENT_STATE_PROBE_TIMEOUT,
    );
    match live {
        Ok(value) => {
            let agent = value.get("agent").unwrap_or(&value);
            let status = agent
                .get("agent_status")
                .and_then(Value::as_str)
                .or_else(|| agent.get("status").and_then(Value::as_str));
            if status.is_some_and(agent_status_settled) {
                return None;
            }
            return Some(pane_close_blocked(pane_id, agent, "fresh_agent_get"));
        }
        Err(error)
            if matches!(
                error.code.as_str(),
                "agent_not_found" | "unknown_agent" | "unknown_pane"
            ) => {}
        Err(error) => {
            if let Some(agent) = pane_agent_from_snapshot(snapshot, pane_id) {
                return Some(pane_close_blocked(pane_id, agent, "snapshot_fallback"));
            }
            return Some(json!({
                "ok": false,
                "code": "pane_close_agent_state_unverified",
                "method": "pane.close",
                "pane_id": pane_id,
                "message": "cannot freshly verify that the pane has no running Agent; pane.close was not sent",
                "probe_error": {"code": error.code, "message": error.message},
                "hint": "verify with agent.get / herdr_since; interrupt with agent.send_keys ESC then ctrl+c only if still working; retry pane.close only after a settled state is observed",
                "pane_close_is_not_cancellation_proof": true,
            }));
        }
    }

    pane_agent_from_snapshot(snapshot, pane_id).and_then(|agent| {
        let status = agent
            .get("agent_status")
            .and_then(Value::as_str)
            .or_else(|| agent.get("status").and_then(Value::as_str));
        (!status.is_some_and(agent_status_settled))
            .then(|| pane_close_blocked(pane_id, agent, "snapshot_fallback"))
    })
}

fn pane_agent_from_snapshot<'a>(snapshot: &'a Value, pane_id: &str) -> Option<&'a Value> {
    snapshot
        .get("agents")
        .and_then(Value::as_array)?
        .iter()
        .find(|agent| agent.get("pane_id").and_then(Value::as_str) == Some(pane_id))
}

fn agent_status_settled(status: &str) -> bool {
    matches!(status, "idle" | "blocked" | "done")
}

fn pane_close_blocked(pane_id: &str, agent: &Value, source: &str) -> Value {
    let status = agent
        .get("agent_status")
        .and_then(Value::as_str)
        .or_else(|| agent.get("status").and_then(Value::as_str))
        .unwrap_or("unknown");
    json!({
        "ok": false,
        "code": "pane_close_agent_not_settled",
        "method": "pane.close",
        "pane_id": pane_id,
        "agent": agent.get("name").cloned().or_else(|| agent.get("agent").cloned()).unwrap_or(Value::Null),
        "agent_status": status,
        "state_change_seq": agent.get("state_change_seq").cloned().unwrap_or(Value::Null),
        "state_source": source,
        "message": "pane.close is resource reclamation, not an Agent interrupt; attached Agent is not verified settled",
        "hint": "if the Agent is working, send agent.send_keys keys=[\"ESC\"]; verify with agent.get/herdr_since; if still working send keys=[\"ctrl+c\"] and verify again before closing",
        "pane_close_is_not_cancellation_proof": true,
    })
}

fn annotate_control_semantics(method: &str, params: &Value, result: &mut Value) {
    let ok = result.get("ok").and_then(Value::as_bool) == Some(true);
    let Some(object) = result.as_object_mut() else {
        return;
    };
    if method == "pane.close" && ok {
        object.insert(
            "pane_close_is_not_cancellation_proof".to_owned(),
            json!(true),
        );
        object.insert(
            "control_note".to_owned(),
            json!("pane closed after preflight; this does not prove that any prior Agent mutation was cancelled or side-effect free"),
        );
    }
    if method == "agent.send_keys" && ok {
        let keys = params
            .get("keys")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if keys.iter().any(|key| matches!(*key, "ESC" | "ctrl+c")) {
            object.insert("control_signal_sent".to_owned(), json!(true));
            object.insert("interrupt_state_verified".to_owned(), json!(false));
            object.insert(
                "control_note".to_owned(),
                json!("control key was sent, but Agent state is not proven settled; verify with agent.get or herdr_since before another control signal or pane.close"),
            );
        }
    }
}

fn method_json(method: &MethodSchema) -> Value {
    let target_kind = if method.method.starts_with("agent.") {
        Some("agent")
    } else if method.method.starts_with("pane.") {
        Some("pane")
    } else {
        None
    };
    let mut value = json!({
        "method": method.method,
        "source": "herdr_socket",
        "route": "workstation_routed",
        "params": {
            "properties": method.properties,
            "required": method.required,
            "empty": method.empty,
        },
    });
    if let Some(target_kind) = target_kind {
        value
            .as_object_mut()
            .expect("method schema is an object")
            .insert("target_kind".to_owned(), json!(target_kind));
    }
    if let Some(guidance) = native_method_guidance(&method.method) {
        value["guidance"] = json!(guidance);
    }
    value
}

fn native_method_guidance(method: &str) -> Option<&'static str> {
    match method {
        "agent.send_keys" => Some(
            "Agent interruption is terminal control, not business input: send keys=[\"ESC\"] first, then verify fresh state with agent.get or herdr_since; only if it is still working send keys=[\"ctrl+c\"], then verify again. agent.prompt/herdr_prompt never means stop/cancel.",
        ),
        "pane.close" => Some(
            "Resource reclamation only. herdr-mcp refuses pane.close while an attached Agent is working or its state is not settled. Interrupt and verify the Agent first. A closed pane is never proof that an Agent mutation was cancelled or had no side effects.",
        ),
        _ => None,
    }
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
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn agent_read_utility_pane_preflight_names_pane_read_surface() {
        let snapshot = json!({
            "panes": [
                {
                    "pane_id": "w1:p2",
                    "workspace_id": "w1",
                    "terminal_title": "herdr-mcp:utility"
                },
                {
                    "pane_id": "w1:p3",
                    "workspace_id": "w1",
                    "agent": "pi"
                }
            ]
        });
        let error = target_kind_preflight(
            &snapshot,
            "agent.read",
            &json!({"target": "w1:p2", "source": "recent_unwrapped"}),
        )
        .expect("utility pane must be rejected before socket delivery");
        assert_eq!(error["code"], "target_kind_mismatch");
        assert_eq!(error["target_kind"], "utility_pane");
        assert_eq!(error["correct_method"], "pane.read");
        assert!(
            target_kind_preflight(
                &snapshot,
                "agent.read",
                &json!({"target": "w1:p3", "source": "recent_unwrapped"}),
            )
            .is_none()
        );
    }

    #[test]
    fn method_schema_exposes_route_and_target_kind() {
        let method = MethodSchema {
            method: "agent.read".to_owned(),
            properties: serde_json::Map::new(),
            required: Vec::new(),
            empty: false,
        };
        let value = method_json(&method);
        assert_eq!(value["route"], "workstation_routed");
        assert_eq!(value["target_kind"], "agent");
    }

    fn temp_repo() -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_REPO.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "herdr-native-tools-{}-{unique}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        root
    }

    fn workspace_snapshot(root: &std::path::Path) -> Value {
        json!({
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "cwd": root.to_string_lossy(),
            }],
            "agents": []
        })
    }

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
    fn native_method_guidance_makes_interrupt_and_close_semantics_explicit() {
        let interrupt = native_method_guidance("agent.send_keys").unwrap();
        assert!(interrupt.contains("ESC"));
        assert!(interrupt.contains("ctrl+c"));
        assert!(interrupt.contains("never means stop/cancel"));

        let close = native_method_guidance("pane.close").unwrap();
        assert!(close.contains("Resource reclamation only"));
        assert!(close.contains("never proof"));
    }

    #[test]
    fn control_results_never_claim_interrupt_or_close_cancellation_proof() {
        let mut keys = json!({"ok": true, "result": {"type": "ok"}});
        annotate_control_semantics(
            "agent.send_keys",
            &json!({"target": "worker", "keys": ["ESC"]}),
            &mut keys,
        );
        assert_eq!(keys["control_signal_sent"], true);
        assert_eq!(keys["interrupt_state_verified"], false);

        let mut failed_keys = json!({
            "ok": false,
            "code": "herdr_socket_error",
        });
        annotate_control_semantics(
            "agent.send_keys",
            &json!({"target": "worker", "keys": ["ESC"]}),
            &mut failed_keys,
        );
        assert!(failed_keys.get("control_signal_sent").is_none());
        assert!(failed_keys.get("interrupt_state_verified").is_none());

        let mut close = json!({"ok": true, "result": {"type": "ok"}});
        annotate_control_semantics("pane.close", &json!({"pane_id": "w1:p1"}), &mut close);
        assert_eq!(close["pane_close_is_not_cancellation_proof"], true);
        assert!(
            close["control_note"]
                .as_str()
                .unwrap()
                .contains("does not prove")
        );
    }

    #[test]
    fn pane_close_guard_treats_working_and_unknown_as_not_settled() {
        assert!(agent_status_settled("idle"));
        assert!(agent_status_settled("blocked"));
        assert!(agent_status_settled("done"));
        assert!(!agent_status_settled("working"));
        assert!(!agent_status_settled("unknown"));

        let working = pane_close_blocked(
            "w1:p1",
            &json!({
                "name": "worker",
                "pane_id": "w1:p1",
                "agent_status": "working",
                "state_change_seq": 7,
            }),
            "fresh_agent_get",
        );
        assert_eq!(working["code"], "pane_close_agent_not_settled");
        assert_eq!(working["agent_status"], "working");
        assert_eq!(working["pane_close_is_not_cancellation_proof"], true);
    }

    #[test]
    fn historical_linked_worktree_membership_is_reconciled_before_one_remove_retry() {
        let root = temp_repo();
        let snapshot = workspace_snapshot(&root);
        let root_text = root.to_string_lossy().into_owned();
        let original = json!({
            "ok": false,
            "code": "not_linked_worktree",
            "message": "workspace is not a Herdr-managed worktree checkout",
            "method": "worktree.remove",
        });
        let mut calls = Vec::<(String, Value)>::new();
        let result = reconcile_historical_linked_worktree_remove_with(
            &snapshot,
            &json!({"workspace_id": "w1", "force": false}),
            original,
            |method, params| {
                calls.push((method.to_owned(), params.clone()));
                match method {
                    "worktree.list" => Ok(json!({
                        "source": {
                            "source_workspace_id": "w0",
                            "source_checkout_path": "/repo-main"
                        },
                        "worktrees": [{
                            "path": root_text,
                            "is_linked_worktree": true,
                            "open_workspace_id": "w1"
                        }]
                    })),
                    "worktree.open" => Ok(json!({
                        "workspace": {"workspace_id": "w1"},
                        "worktree": {
                            "path": root_text,
                            "is_linked_worktree": true,
                            "open_workspace_id": "w1"
                        },
                        "already_open": true
                    })),
                    "worktree.remove" => Ok(json!({
                        "type": "worktree_removed",
                        "workspace_id": "w1"
                    })),
                    other => panic!("unexpected method: {other}"),
                }
            },
        );

        assert_eq!(result["ok"], true);
        assert_eq!(
            result["compatibility_reconciled"]["kind"],
            "historical_linked_worktree_membership"
        );
        assert_eq!(
            calls
                .iter()
                .map(|(method, _)| method.as_str())
                .collect::<Vec<_>>(),
            vec!["worktree.list", "worktree.open", "worktree.remove"]
        );
        assert_eq!(calls[1].1["workspace_id"], "w0");
        assert_eq!(calls[1].1["path"], root_text);
        assert_eq!(calls[2].1["workspace_id"], "w1");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worktree_remove_reconcile_stays_fail_closed_without_linked_open_proof() {
        let root = temp_repo();
        let snapshot = workspace_snapshot(&root);
        let root_text = root.to_string_lossy().into_owned();
        let original = json!({
            "ok": false,
            "code": "not_linked_worktree",
            "message": "workspace is not a Herdr-managed worktree checkout",
            "method": "worktree.remove",
        });
        let mut calls = Vec::<String>::new();
        let result = reconcile_historical_linked_worktree_remove_with(
            &snapshot,
            &json!({"workspace_id": "w1", "force": false}),
            original.clone(),
            |method, _| {
                calls.push(method.to_owned());
                assert_eq!(method, "worktree.list");
                Ok(json!({
                    "source": {"source_workspace_id": "w0"},
                    "worktrees": [{
                        "path": root_text,
                        "is_linked_worktree": false,
                        "open_workspace_id": "w1"
                    }]
                }))
            },
        );

        assert_eq!(result, original);
        assert_eq!(calls, vec!["worktree.list"]);
        fs::remove_dir_all(root).unwrap();
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
