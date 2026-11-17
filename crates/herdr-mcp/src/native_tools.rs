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

pub fn inspect(
    client: &HerdrClient,
    cache: Option<&EventCache>,
    exec: Option<&ExecRegistry>,
) -> Value {
    // Capture the cursor before reading any cached or live snapshot. Events
    // arriving after this point are intentionally returned by herdr_since.
    let since_cursor = cache.map(EventCache::current_cursor).unwrap_or(0);
    let cached_snapshot = cache.and_then(EventCache::fresh_snapshot);
    let mut view = inspect::inspect_core(client, cached_snapshot);
    runtime_meta::augment_inspect(&mut view, cache, exec);
    annotate_since_cursor(&mut view, since_cursor);
    view
}

pub fn inspect_with_selection(
    client: &HerdrClient,
    cache: Option<&EventCache>,
    exec: Option<&ExecRegistry>,
    selection: &crate::runtime_adapter::RuntimeBackendSelection,
    core_client: Option<&crate::runtime_adapter::HerdrCoreClient>,
) -> Value {
    // Capture the cursor before reading any cached or live snapshot. Events
    // arriving after this point are intentionally returned by herdr_since.
    let since_cursor = cache.map(EventCache::current_cursor).unwrap_or(0);
    let cached_snapshot = cache.and_then(EventCache::fresh_snapshot);
    let mut view = match core_client {
        Some(core_client)
            if selection.backend == crate::runtime_adapter::RuntimeBackend::HerdrCore =>
        {
            crate::runtime_adapter::inspect_herdr_core_with_client(selection, core_client)
        }
        _ => inspect::inspect_core_with_selection(client, cached_snapshot, selection),
    };
    runtime_meta::augment_inspect(&mut view, cache, exec);
    annotate_since_cursor(&mut view, since_cursor);
    view
}

fn annotate_since_cursor(view: &mut Value, since_cursor: u64) {
    if let Some(object) = view.as_object_mut() {
        object.insert("since_cursor".to_owned(), json!(since_cursor));
    }
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

    // One admission, one fact: the alias is validated and rewritten before the
    // live schema check (the rewrite is what the schema must validate), and the
    // `model_validation` fact is carried to the success envelope. Admitting a
    // second time here would see no `model` key left and silently drop the fact
    // for the `args` passthrough form (S5 review P1-3).
    let (params, model_fact) = if method == "agent.start" {
        match admit_agent_start_model(params) {
            Ok((params, fact)) => (params, fact),
            Err(refusal) => return refusal,
        }
    } else {
        (params, None)
    };
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
            let mut envelope = if warnings.is_empty() {
                json!({"ok": true, "result": result})
            } else {
                json!({"ok": true, "result": result, "warnings": warnings})
            };
            if let Some(fact) = model_fact {
                envelope["model_validation"] = fact;
            }
            envelope
        }
        Err(error) => json!({
            "ok": false,
            "code": error.code,
            "message": error.message,
            "method": method,
        }),
    }
}

/// Validate an explicit `model` alias on `agent.start` and translate it into
/// a wire form the live herdr schema can carry. Returns the (possibly
/// rewritten) params plus the `model_validation` fact for the success
/// envelope, or a fail-closed refusal. No model key means unchanged params.
fn admit_agent_start_model(mut params: Value) -> Result<(Value, Option<Value>), Value> {
    let Some(alias) = params.get("model").and_then(Value::as_str) else {
        return Ok((params, None));
    };
    let alias = alias.trim().to_owned();
    let kind = params
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let check = crate::model_alias::validate_model_alias(&kind, &alias);
    let fact = crate::model_alias::validation_fact(&kind, &alias, &check);
    let crate::model_alias::ModelAliasCheck::Validated { ref canonical } = check else {
        let mut refusal = crate::model_alias::model_alias_refusal(&kind, &alias, &check);
        refusal["method"] = json!("agent.start");
        return Err(refusal);
    };
    // The live schema decides how the validated canonical form is delivered.
    // A native `model` property passes it through; otherwise the `args`
    // passthrough carries `--model <canonical>` (pi's documented flag).
    // Neither means the alias cannot be delivered provably: refuse instead of
    // dropping it.
    let properties = match schema::agent_start_param_properties() {
        Ok(Some(properties)) => properties,
        Ok(None) => {
            return Err(model_delivery_unsupported(
                &kind,
                &alias,
                &fact,
                "agent.start is not represented in the live herdr schema",
            ));
        }
        Err(error) => {
            return Err(model_delivery_unsupported(
                &kind,
                &alias,
                &fact,
                &format!("live herdr schema unavailable: {error}"),
            ));
        }
    };
    if properties.contains_key("model") {
        params["model"] = json!(canonical);
        return Ok((params, Some(fact)));
    }
    if properties.contains_key("args") {
        if kind != "pi" {
            return Err(model_delivery_unsupported(
                &kind,
                &alias,
                &fact,
                "only pi has a documented --model passthrough",
            ));
        }
        let existing_args = params.get("args").and_then(Value::as_array);
        if existing_args.is_some_and(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .any(|arg| arg == "--model" || arg.starts_with("--model="))
        }) {
            let mut refusal = crate::model_alias::model_alias_refusal(&kind, &alias, &check);
            refusal["method"] = json!("agent.start");
            refusal["code"] = json!("invalid_params");
            refusal["message"] = json!(
                "agent.start args already carry --model; pass the model only via the model field"
            );
            return Err(refusal);
        }
        let mut args: Vec<Value> = existing_args.cloned().unwrap_or_default();
        args.push(json!("--model"));
        args.push(json!(canonical));
        params["args"] = Value::Array(args);
        if let Some(object) = params.as_object_mut() {
            object.remove("model");
        }
        return Ok((params, Some(fact)));
    }
    Err(model_delivery_unsupported(
        &kind,
        &alias,
        &fact,
        "the live herdr agent.start schema declares neither model nor args",
    ))
}

fn model_delivery_unsupported(kind: &str, alias: &str, fact: &Value, reason: &str) -> Value {
    json!({
        "ok": false,
        "code": "model_delivery_unsupported",
        "method": "agent.start",
        "kind": kind,
        "requested_model": alias,
        "delivery_state": "not_delivered",
        "submitted": false,
        "fallback_allowed": false,
        "retryable": false,
        "model_validation": fact,
        "message": format!("validated model alias '{alias}' cannot be delivered to agent.start: {reason}; the model was not silently dropped"),
    })
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
    let result = call(client, method, params.clone());
    if method == "worktree.remove"
        && result.get("ok").and_then(Value::as_bool) == Some(false)
        && result.get("code").and_then(Value::as_str) == Some("not_linked_worktree")
    {
        return reconcile_historical_linked_worktree_remove(client, snapshot, &params, result);
    }
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
    value
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
    fn inspect_cursor_is_directly_consumable_by_since() {
        let cache = EventCache::from_snapshot_for_test(json!({"panes": [], "agents": []}));
        let cursor = cache.current_cursor();
        let result = since(&cache, cursor, None, Ok(None));
        assert_eq!(result["ok"], true);
        assert_eq!(result["cursor"], cursor);
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

    /// 1.2 S5: an explicit model on agent.start for a kind without an
    /// obtainable list is refused before any socket contact, with
    /// validated:false and no fallback.
    #[test]
    fn agent_start_model_without_obtainable_list_fails_closed() {
        let client = HerdrClient::new("/path/that/does/not/exist");
        let result = call(
            &client,
            "agent.start",
            json!({"name": "a", "kind": "claude", "pane_id": "w1:p1", "model": "some-model"}),
        );
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "model_alias_unknown");
        assert_eq!(result["delivery_state"], "not_delivered");
        assert_eq!(result["submitted"], false);
        assert_eq!(result["fallback_allowed"], false);
        assert_eq!(result["model_validation"]["validated"], false);
        assert_eq!(result["available_models"], json!([]));
    }

    /// 1.2 S5: an unknown pi alias is refused with the obtainable list, taken
    /// from `pi --list-models` (S5 review P1-1), not the provider cache.
    #[test]
    #[cfg(unix)]
    fn agent_start_unknown_pi_alias_reports_available_models() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = crate::test_env::lock();
        let previous = std::env::var_os("HERDR_PI_BIN");
        let dir = std::env::temp_dir().join(format!(
            "herdr-native-model-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("pi");
        fs::write(
            &bin,
            "#!/bin/sh\nif [ \"$1\" = \"--list-models\" ]; then\necho 'provider model'\necho 'openai gpt-4'\nelse\nexit 1\nfi\n",
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        unsafe { std::env::set_var("HERDR_PI_BIN", &bin) };
        let client = HerdrClient::new("/path/that/does/not/exist");
        let result = call(
            &client,
            "agent.start",
            json!({"name": "a", "kind": "pi", "pane_id": "w1:p1", "model": "openai/gpt-99"}),
        );
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "model_alias_unknown");
        assert_eq!(result["model_validation"]["validated"], false);
        assert_eq!(result["available_models"], json!(["openai/gpt-4"]));
        assert_eq!(result["delivery_state"], "not_delivered");
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HERDR_PI_BIN", value),
                None => std::env::remove_var("HERDR_PI_BIN"),
            }
        }
        let _ = fs::remove_dir_all(dir);
    }

    /// 1.2 S5: no model key means the params pass through unchanged.
    #[test]
    fn agent_start_without_model_is_unchanged() {
        let params = json!({"name": "a", "kind": "pi", "pane_id": "w1:p1"});
        let (out, fact) = admit_agent_start_model(params.clone()).unwrap();
        assert_eq!(out, params);
        assert!(fact.is_none());
    }
}
