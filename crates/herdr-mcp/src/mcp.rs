use crate::contract;
use crate::exec_sessions::ExecRegistry;
use crate::exec_tools;
use crate::fs_mutation;
use crate::fs_patch;
use crate::fs_tools;
use crate::git_tools;
use crate::herdr::HerdrClient;
use crate::native_tools;
use crate::prompt::{self, PromptRegistry};
use crate::skill::SkillService;
use crate::state_cache::EventCache;
use crate::state_store::{
    BrowserDeliveryState, BrowserDispatchReservation, BrowserDispatchReserveInput,
    BrowserDispatchUpdateInput, BrowserResourceResolveInput, ContinuitySearchInput, StateStore,
    WorkMemoryBindingInput, WorkMemoryCheckpointInput, WorkMemoryEvidenceInput,
    WorkMemoryPortableSourceInput, WorkMemoryTurnInput,
};
use crate::tcc_broker;
use crate::utility_exec;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SDK_WIRE_PROTOCOL: &str = "2025-11-25";
/// ChatGPT/OpenAI connector probe version; advertised on discover and negotiated
/// down to [`SDK_WIRE_PROTOCOL`] for the actual wire session.
pub const OPENAI_PROBE_PROTOCOL: &str = "2026-07-28";
pub const SERVER_INSTRUCTIONS: &str = "Herdr control plane for a WEB planner. Session start: herdr_inspect then herdr_skill once. On fresh prior-work continue/resume intent, load herdr_skill and search durable Continuity before asking the user for an internal ID; never select a chain by recency or text similarity alone. Prefer deterministic herdr_fs_*/herdr_git/herdr_exec work before agent reasoning. Before unknown native API calls use herdr_methods, then herdr_call. Use explicit workspace/pane IDs and never blind-retry uncertain mutations.";

const SUPPORTED_VERSIONS: [&str; 5] = [
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCallerGrant {
    pub endpoint_ref: String,
    pub provider: String,
    pub account_ref: String,
}

pub struct RuntimeContext<'a> {
    pub client: &'a HerdrClient,
    pub cache: &'a EventCache,
    pub exec: &'a ExecRegistry,
    pub prompt: &'a PromptRegistry,
    pub skill: &'a SkillService,
    pub state_store: &'a std::sync::Arc<std::sync::Mutex<StateStore>>,
    pub caller_webchat_control_grants: &'a [BrowserCallerGrant],
    pub browser_actuator: Option<&'a dyn BrowserActuator>,
}

pub trait BrowserActuator: Send + Sync {
    fn actuate(
        &self,
        operation: &str,
        params: &Value,
        expected_generation: i64,
    ) -> Result<BrowserPostconditionEvidence, String>;
}

pub fn handle(request: &Value, context: &RuntimeContext<'_>) -> Option<Value> {
    let object = match request.as_object() {
        Some(object) => object,
        None => return Some(error(Value::Null, -32600, "Invalid Request")),
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(error(id(request), -32600, "Invalid Request"));
    }
    let method = match object.get("method").and_then(Value::as_str) {
        Some(method) => method,
        None => return Some(error(id(request), -32600, "Invalid Request")),
    };
    let request_id = id(request);
    let is_notification = object.get("id").is_none();

    let result = match method {
        "initialize" => Ok(initialize_result(request)),
        "server/discover" => Ok(discover_result()),
        "tools/list" => contract::tool_catalog().map(|tools| json!({"tools": tools})),
        "tools/call" => tool_call(request, context),
        "ping" => Ok(json!({})),
        "notifications/initialized" => return None,
        _ => {
            if is_notification {
                return None;
            }
            return Some(error(request_id, -32601, "Method not found"));
        }
    };

    if is_notification {
        return None;
    }
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": request_id, "result": result}),
        Err(message) => error(request_id, -32603, &message),
    })
}

pub fn negotiate_protocol_version(requested: &str) -> &'static str {
    match requested {
        "2025-11-25" => "2025-11-25",
        "2025-06-18" => "2025-06-18",
        "2025-03-26" => "2025-03-26",
        "2024-11-05" => "2024-11-05",
        "2024-10-07" => "2024-10-07",
        _ => SDK_WIRE_PROTOCOL,
    }
}

fn initialize_result(request: &Value) -> Value {
    let requested = request
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(SDK_WIRE_PROTOCOL);
    let protocol = negotiate_protocol_version(requested);
    let identity = contract::identity().ok();
    json!({
        "protocolVersion": protocol,
        "capabilities": {"tools": {"listChanged": true}},
        "serverInfo": {
            "name": "herdr-mcp",
            "version": crate::runtime_meta::runtime_version()
        },
        "instructions": SERVER_INSTRUCTIONS,
        "_meta": {
            "herdr_contract_epoch": identity.as_ref().map(|value| value.epoch),
            "herdr_contract_hash": identity.as_ref().map(|value| value.hash.as_str()),
        }
    })
}

fn discover_result() -> Value {
    let identity = contract::identity().ok();
    json!({
        "resultType": "complete",
        "supportedVersions": SUPPORTED_VERSIONS,
        "capabilities": {"tools": {"listChanged": true}},
        "instructions": SERVER_INSTRUCTIONS,
        "ttlMs": 3_600_000,
        "cacheScope": "private",
        "_meta": {
            "io.modelcontextprotocol/serverInfo": {
                "name": "herdr-mcp",
                "version": crate::runtime_meta::runtime_version()
            },
            "herdr_contract_epoch": identity.as_ref().map(|value| value.epoch),
            "herdr_contract_hash": identity.as_ref().map(|value| value.hash.as_str()),
        }
    })
}

fn tool_call(request: &Value, context: &RuntimeContext<'_>) -> Result<Value, String> {
    let name = request
        .pointer("/params/name")
        .and_then(Value::as_str)
        .ok_or_else(|| "tools/call requires params.name".to_owned())?;
    let arguments = request
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Ok(tool_result(
            json!({"ok": false, "code": "invalid_params", "message": "arguments must be an object"}),
            true,
        ));
    }

    let output = match name {
        "herdr_methods" => {
            let query = arguments.get("query").and_then(Value::as_str).unwrap_or("");
            native_tools::methods(query)
        }
        "herdr_inspect" => {
            native_tools::inspect(context.client, Some(context.cache), Some(context.exec))
        }
        "herdr_since" => {
            let cursor = arguments.get("cursor").and_then(Value::as_u64).unwrap_or(0);
            let workspace = arguments.get("workspace").and_then(Value::as_str);
            native_tools::since(context.cache, cursor, workspace)
        }
        "herdr_call" => {
            let method = arguments
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| "herdr_call requires arguments.method".to_owned())?;
            let params_text = arguments
                .get("params")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let params: Value = match serde_json::from_str::<Value>(params_text) {
                Ok(value) if value.is_object() => value,
                Ok(_) => {
                    return Ok(tool_result(
                        json!({"ok": false, "code": "invalid_params", "message": "herdr_call params must decode to an object"}),
                        false,
                    ));
                }
                Err(parse_error) => {
                    return Ok(tool_result(
                        json!({"ok": false, "code": "invalid_params_json", "message": parse_error.to_string()}),
                        false,
                    ));
                }
            };
            if method.starts_with("continuity.") {
                continuity_call(context.state_store, method, &params)
            } else if method.starts_with("work_memory.") {
                work_memory_call(context.state_store, method, &params)
            } else if BrowserOperation::parse(method).is_some() {
                browser_operation_call_with_grants(
                    context.state_store,
                    method,
                    &params,
                    context.caller_webchat_control_grants,
                    context.browser_actuator,
                )
            } else if method.starts_with("herdr_mcp.browser_endpoint.")
                || method.starts_with("herdr_mcp.browser_resource.")
            {
                browser_registry_call_with_grants(
                    context.state_store,
                    method,
                    &params,
                    context.caller_webchat_control_grants,
                )
            } else if method.starts_with("artifact.") {
                artifact_call(&config_dir(), &context.cache.snapshot(), method, &params)
            } else {
                native_tools::call_with_local(
                    context.client,
                    context.skill,
                    &context.cache.snapshot(),
                    method,
                    params,
                )
            }
        }
        "herdr_fs_read" => route_fs_git("fs_read", &context.cache.snapshot(), &arguments),
        "herdr_fs_list" => route_fs_git("fs_list", &context.cache.snapshot(), &arguments),
        "herdr_fs_grep" => route_fs_git("fs_grep", &context.cache.snapshot(), &arguments),
        "herdr_fs_image" => {
            return Ok(
                match tcc_broker::route_fs_git("fs_image", &context.cache.snapshot(), &arguments) {
                    Some(Ok(value)) => {
                        let value = crate::macos_permissions::map_fs_git_result(value);
                        match tcc_broker::image_tool_result_from_broker(&value) {
                            Ok(tool) => tool,
                            Err(_)
                                if value.get("code").and_then(Value::as_str)
                                    == Some("macos_tcc_access_blocked") =>
                            {
                                tool_result(value, true)
                            }
                            Err(message) => tool_result(
                                json!({ "ok": false, "code": "broker_image_invalid", "message": message }),
                                true,
                            ),
                        }
                    }
                    Some(Err(message)) => tool_result(
                        crate::macos_permissions::map_fs_git_result(json!({
                            "ok": false,
                            "code": "broker_failed",
                            "message": message
                        })),
                        true,
                    ),
                    None => match fs_tools::image(&context.cache.snapshot(), &arguments) {
                        Ok(image) => image_tool_result(image),
                        Err(error) => {
                            tool_result(crate::macos_permissions::map_fs_git_result(error), false)
                        }
                    },
                },
            );
        }
        "herdr_fs_edit" => route_fs_git("fs_edit", &context.cache.snapshot(), &arguments),
        "herdr_fs_write" => route_fs_git("fs_write", &context.cache.snapshot(), &arguments),
        "herdr_fs_patch" => route_fs_git("fs_patch", &context.cache.snapshot(), &arguments),
        "herdr_git" => route_fs_git("git", &context.cache.snapshot(), &arguments),
        "herdr_exec_start" => {
            exec_tools::start(&context.cache.snapshot(), context.exec, &arguments)
        }
        "herdr_exec_read" => exec_tools::read(context.exec, &arguments),
        "herdr_exec_kill" => exec_tools::kill(context.exec, &arguments),
        "herdr_exec" => utility_exec::run(context.client, &context.cache.snapshot(), &arguments),
        "herdr_prompt" => prompt::run(context.client, context.prompt, &arguments),
        "herdr_skill" => context
            .skill
            .fetch_for_runtime(&arguments, &context.cache.snapshot()),
        pending if contract::tool_names().contains(&pending) => {
            return Ok(tool_result(
                json!({
                    "ok": false,
                    "code": "native_tool_pending",
                    "tool": pending,
                    "message": "This epoch-2 tool has not migrated to the Rust candidate runtime yet"
                }),
                true,
            ));
        }
        _ => {
            return Ok(tool_result(
                json!({"ok": false, "code": "unknown_tool", "tool": name}),
                true,
            ));
        }
    };

    Ok(tool_result(output, false))
}

fn continuity_call(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    method: &str,
    params: &Value,
) -> Value {
    let Ok(store) = store.lock() else {
        return json!({"ok": false, "code": "continuity_store_unavailable"});
    };
    match method {
        "continuity.resume" => {
            let Some(continuity_id) = params
                .get("continuity_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return json!({"ok": false, "code": "continuity_id_required"});
            };
            match store.continuity_resume(continuity_id, 32) {
                Ok(Some(record)) => {
                    let turns = record
                        .turns
                        .into_iter()
                        .map(|turn| {
                            json!({
                                "conversation_id": turn.conversation_id,
                                "message_id": turn.message_id,
                                "role": turn.role,
                                "text": turn.text,
                                "observed_at": turn.observed_at,
                            })
                        })
                        .collect::<Vec<_>>();
                    json!({
                        "ok": true,
                        "continuity_id": record.continuity_id,
                        "title": record.title,
                        "project_id": record.project_id,
                        "status": record.status,
                        "checkpoint": record.checkpoint,
                        "turns": turns,
                        "updated_at": record.updated_at,
                        "instruction": "Treat this as persisted working context. Re-check live Herdr/runtime/Git state before any mutation."
                    })
                }
                Ok(None) => {
                    json!({"ok": false, "code": "continuity_not_found", "continuity_id": continuity_id})
                }
                Err(error) => {
                    json!({"ok": false, "code": "continuity_read_failed", "message": error})
                }
            }
        }
        "continuity.list" => match store.continuity_candidates(10) {
            Ok(records) => json!({
                "ok": true,
                "candidates": records.into_iter().map(|record| json!({
                    "continuity_id": record.continuity_id,
                    "title": record.title,
                    "project_id": record.project_id,
                    "status": record.status,
                    "updated_at": record.updated_at,
                })).collect::<Vec<_>>()
            }),
            Err(error) => json!({"ok": false, "code": "continuity_list_failed", "message": error}),
        },
        "continuity.search" => {
            let Some(object) = params.as_object() else {
                return json!({"ok": false, "code": "continuity_search_params_invalid", "message": "params must be an object"});
            };
            const ALLOWED: &[&str] = &[
                "project_id",
                "workspace_id",
                "conversation_id",
                "query",
                "limit",
            ];
            if let Some(key) = object.keys().find(|key| !ALLOWED.contains(&key.as_str())) {
                return json!({
                    "ok": false,
                    "code": "continuity_search_params_invalid",
                    "message": format!("unknown continuity.search param: {key}"),
                });
            }
            let project_id = match continuity_search_string(params, "project_id", 256) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let workspace_id = match continuity_search_string(params, "workspace_id", 128) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let conversation_id = match continuity_search_string(params, "conversation_id", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let query = match continuity_search_string(params, "query", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let limit = match params.get("limit") {
                None | Some(Value::Null) => 5,
                Some(value) => match value.as_u64() {
                    Some(value @ 1..=10) => value as usize,
                    _ => {
                        return json!({
                            "ok": false,
                            "code": "continuity_search_params_invalid",
                            "message": "limit must be an integer between 1 and 10",
                        });
                    }
                },
            };
            let exact_identity_hint =
                project_id.is_some() || workspace_id.is_some() || conversation_id.is_some();
            let mut match_reasons = Vec::new();
            if conversation_id.is_some() {
                match_reasons.push("conversation_id");
            }
            if project_id.is_some() {
                match_reasons.push("project_id");
            }
            if workspace_id.is_some() {
                match_reasons.push("workspace_id");
            }
            if query.is_some() {
                match_reasons.push("query");
            }
            match store.continuity_search(ContinuitySearchInput {
                project_id,
                workspace_id,
                conversation_id,
                query,
                limit,
            }) {
                Ok(records) => {
                    let identity_match = if exact_identity_hint {
                        store.continuity_search(ContinuitySearchInput {
                            project_id,
                            workspace_id,
                            conversation_id,
                            query: None,
                            limit: 2,
                        })
                    } else {
                        Ok(Vec::new())
                    };
                    let identity_match = match identity_match {
                        Ok(value) => value,
                        Err(error) => {
                            return json!({"ok": false, "code": "continuity_search_failed", "message": error});
                        }
                    };
                    let auto_resume_safe = records.len() == 1
                        && identity_match.len() == 1
                        && records[0].continuity_id == identity_match[0].continuity_id;
                    let resolution = if records.is_empty() {
                        "none"
                    } else if auto_resume_safe {
                        "unique_exact"
                    } else {
                        "confirmation_required"
                    };
                    let candidates = records
                        .into_iter()
                        .map(|record| {
                            json!({
                                "continuity_id": record.continuity_id,
                                "title": record.title,
                                "project_id": record.project_id,
                                "workspace_ids": record.workspace_ids,
                                "status": record.status,
                                "updated_at": record.updated_at,
                                "recent_user_excerpt": record.recent_user_excerpt,
                                "recent_assistant_excerpt": record.recent_assistant_excerpt,
                                "match_reasons": match_reasons,
                            })
                        })
                        .collect::<Vec<_>>();
                    json!({
                        "ok": true,
                        "resolution": resolution,
                        "auto_resume_safe": auto_resume_safe,
                        "confirmation_required": !auto_resume_safe && !candidates.is_empty(),
                        "candidates": candidates,
                        "instruction": if auto_resume_safe {
                            "Exactly one active chain matched a stable identity hint. Resume that continuity_id, then re-check live Herdr/runtime/Git state before mutation."
                        } else if resolution == "confirmation_required" {
                            "Do not choose by recency or textual similarity alone. Show the bounded candidate evidence to the user and ask which prior work chain to continue; after confirmation, resume exactly that continuity_id."
                        } else {
                            "No active continuity chain matched. Do not invent an id; ask for a distinguishing detail or proceed as fresh work if that is the user's intent."
                        },
                    })
                }
                Err(error) => {
                    json!({"ok": false, "code": "continuity_search_failed", "message": error})
                }
            }
        }
        "continuity.resolve" => {
            let Some(conversation_id) = params
                .get("conversation_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return json!({"ok": false, "code": "conversation_id_required"});
            };
            match store.continuity_for_conversation(conversation_id) {
                Ok(Some(continuity_id)) => json!({"ok": true, "continuity_id": continuity_id}),
                Ok(None) => json!({"ok": false, "code": "continuity_not_found"}),
                Err(error) if error == "continuity_binding_ambiguous" => {
                    json!({"ok": false, "code": "continuity_ambiguous"})
                }
                Err(error) => {
                    json!({"ok": false, "code": "continuity_resolve_failed", "message": error})
                }
            }
        }
        _ => json!({"ok": false, "code": "unknown_local_method", "method": method}),
    }
}

fn work_memory_call(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    method: &str,
    params: &Value,
) -> Value {
    let Some(object) = params.as_object() else {
        return json!({"ok": false, "code": "work_memory_params_invalid"});
    };
    let Ok(mut store) = store.lock() else {
        return json!({"ok": false, "code": "work_memory_store_unavailable"});
    };
    match method {
        "work_memory.bind" => {
            if let Some(error) = work_memory_reject_unknown(
                object,
                &[
                    "continuity_id",
                    "project_ref",
                    "repo_id",
                    "work_chain_id",
                    "provider",
                    "account_ref",
                    "space_ref",
                    "session_ref",
                    "bound_at",
                ],
            ) {
                return error;
            }
            let continuity_id = match work_memory_required_string(params, "continuity_id", 160) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let project_ref = match work_memory_required_string(params, "project_ref", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let repo_id = match work_memory_required_string(params, "repo_id", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let work_chain_id = match work_memory_required_string(params, "work_chain_id", 128) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let provider = match work_memory_required_string(params, "provider", 32) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let account_ref = match work_memory_optional_string(params, "account_ref", 256) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let space_ref = match work_memory_optional_string(params, "space_ref", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let session_ref = match work_memory_required_string(params, "session_ref", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let bound_at = match work_memory_required_i64(params, "bound_at") {
                Ok(value) => value,
                Err(error) => return error,
            };
            match store.bind_work_memory(WorkMemoryBindingInput {
                continuity_id,
                project_ref,
                repo_id,
                work_chain_id,
                provider,
                account_ref,
                space_ref,
                session_ref,
                bound_at,
            }) {
                Ok(()) => json!({
                    "ok": true,
                    "continuity_id": continuity_id,
                    "project_ref": project_ref,
                    "repo_id": repo_id,
                    "work_chain_id": work_chain_id,
                    "provider": provider,
                    "session_ref": session_ref,
                    "retention_policy": "retain_all",
                }),
                Err(error) => work_memory_store_error(error),
            }
        }
        "work_memory.append_turn" => {
            if let Some(error) = work_memory_reject_unknown(
                object,
                &[
                    "continuity_id",
                    "provider",
                    "account_ref",
                    "space_ref",
                    "session_ref",
                    "provider_message_ref",
                    "role",
                    "text",
                    "fingerprint",
                    "observed_at",
                ],
            ) {
                return error;
            }
            let continuity_id = match work_memory_required_string(params, "continuity_id", 160) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let provider = match work_memory_required_string(params, "provider", 32) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let account_ref = match work_memory_optional_string(params, "account_ref", 256) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let space_ref = match work_memory_optional_string(params, "space_ref", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let session_ref = match work_memory_required_string(params, "session_ref", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let provider_message_ref =
                match work_memory_required_string(params, "provider_message_ref", 512) {
                    Ok(value) => value,
                    Err(error) => return error,
                };
            let role = match work_memory_required_string(params, "role", 32) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let text = match work_memory_required_text(params, "text", 256 * 1024) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let fingerprint = match work_memory_optional_string(params, "fingerprint", 256) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let observed_at = match work_memory_required_i64(params, "observed_at") {
                Ok(value) => value,
                Err(error) => return error,
            };
            match store.append_work_memory_turn(WorkMemoryTurnInput {
                continuity_id,
                provider,
                account_ref,
                space_ref,
                session_ref,
                provider_message_ref,
                role,
                text,
                fingerprint,
                observed_at,
            }) {
                Ok(record) => json!({
                    "ok": true,
                    "inserted": record.inserted,
                    "message_id": record.message_id,
                }),
                Err(error) => work_memory_store_error(error),
            }
        }
        "work_memory.append_evidence" => {
            if let Some(error) = work_memory_reject_unknown(
                object,
                &[
                    "continuity_id",
                    "kind",
                    "content",
                    "provider",
                    "account_ref",
                    "space_ref",
                    "session_ref",
                    "portable_source",
                    "created_at",
                ],
            ) {
                return error;
            }
            let continuity_id = match work_memory_required_string(params, "continuity_id", 160) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let kind = match work_memory_required_string(params, "kind", 32) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let content = match work_memory_required_text(params, "content", 256 * 1024) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let provider = match work_memory_optional_string(params, "provider", 32) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let account_ref = match work_memory_optional_string(params, "account_ref", 256) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let space_ref = match work_memory_optional_string(params, "space_ref", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let session_ref = match work_memory_optional_string(params, "session_ref", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let portable_source = match params.get("portable_source") {
                None | Some(Value::Null) => None,
                Some(source @ Value::Object(source_object)) => {
                    if let Some(error) = work_memory_reject_unknown(
                        source_object,
                        &[
                            "repo_id",
                            "commit_sha",
                            "repo_relative_path",
                            "line_start",
                            "line_end",
                        ],
                    ) {
                        return error;
                    }
                    let repo_id = match work_memory_required_string(source, "repo_id", 512) {
                        Ok(value) => value,
                        Err(error) => return error,
                    };
                    let commit_sha = match work_memory_required_string(source, "commit_sha", 64) {
                        Ok(value) => value,
                        Err(error) => return error,
                    };
                    let repo_relative_path =
                        match work_memory_required_string(source, "repo_relative_path", 1024) {
                            Ok(value) => value,
                            Err(error) => return error,
                        };
                    let line_start = match source.get("line_start") {
                        None | Some(Value::Null) => None,
                        Some(value) => match value.as_i64() {
                            Some(value) => Some(value),
                            None => {
                                return json!({"ok": false, "code": "work_memory_line_start_invalid"});
                            }
                        },
                    };
                    let line_end = match source.get("line_end") {
                        None | Some(Value::Null) => None,
                        Some(value) => match value.as_i64() {
                            Some(value) => Some(value),
                            None => {
                                return json!({"ok": false, "code": "work_memory_line_end_invalid"});
                            }
                        },
                    };
                    Some(WorkMemoryPortableSourceInput {
                        repo_id,
                        commit_sha,
                        repo_relative_path,
                        line_start,
                        line_end,
                    })
                }
                Some(_) => {
                    return json!({"ok": false, "code": "work_memory_portable_source_invalid"});
                }
            };
            let created_at = match work_memory_required_i64(params, "created_at") {
                Ok(value) => value,
                Err(error) => return error,
            };
            match store.append_work_memory_evidence(WorkMemoryEvidenceInput {
                continuity_id,
                kind,
                content,
                provider,
                account_ref,
                space_ref,
                session_ref,
                portable_source,
                created_at,
            }) {
                Ok(record) => json!({
                    "ok": true,
                    "evidence_id": record.evidence_id,
                    "sha256": record.sha256,
                    "portable_evidence_ref": record.portable_ref.map(|reference| json!({
                        "kind": reference.kind,
                        "repo_id": reference.repo_id,
                        "commit_sha": reference.commit_sha,
                        "repo_relative_path": reference.repo_relative_path,
                        "line_start": reference.line_start,
                        "line_end": reference.line_end,
                        "evidence_sha256": reference.evidence_sha256,
                    })),
                }),
                Err(error) => work_memory_store_error(error),
            }
        }
        "work_memory.checkpoint.put" => {
            if let Some(error) = work_memory_reject_unknown(
                object,
                &[
                    "continuity_id",
                    "expected_checkpoint_revision",
                    "summary",
                    "checkpoint_json",
                    "through_message_id",
                    "through_evidence_id",
                    "created_at",
                ],
            ) {
                return error;
            }
            let continuity_id = match work_memory_required_string(params, "continuity_id", 160) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let expected_checkpoint_revision = match work_memory_required_i64(
                params,
                "expected_checkpoint_revision",
            ) {
                Ok(value) if value >= 0 => value,
                _ => {
                    return json!({"ok": false, "code": "work_memory_expected_checkpoint_revision_invalid"});
                }
            };
            let summary = match work_memory_required_text(params, "summary", 8 * 1024) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let checkpoint_json =
                match work_memory_required_text(params, "checkpoint_json", 64 * 1024) {
                    Ok(value) => value,
                    Err(error) => return error,
                };
            match serde_json::from_str::<Value>(checkpoint_json) {
                Ok(Value::Object(_)) => {}
                _ => return json!({"ok": false, "code": "work_memory_checkpoint_json_invalid"}),
            }
            let through_message_id =
                match work_memory_optional_string(params, "through_message_id", 512) {
                    Ok(value) => value,
                    Err(error) => return error,
                };
            let through_evidence_id =
                match work_memory_optional_string(params, "through_evidence_id", 128) {
                    Ok(value) => value,
                    Err(error) => return error,
                };
            let created_at = match work_memory_required_i64(params, "created_at") {
                Ok(value) => value,
                Err(error) => return error,
            };
            match store.put_work_memory_checkpoint(WorkMemoryCheckpointInput {
                continuity_id,
                expected_checkpoint_revision,
                summary,
                checkpoint_json,
                through_message_id,
                through_evidence_id,
                created_at,
            }) {
                Ok(checkpoint) => json!({
                    "ok": true,
                    "checkpoint": {
                        "revision": checkpoint.revision,
                        "summary": checkpoint.summary,
                        "checkpoint_json": checkpoint.checkpoint_json,
                        "sha256": checkpoint.sha256,
                        "through_message_id": checkpoint.through_message_id,
                        "through_evidence_id": checkpoint.through_evidence_id,
                        "created_at": checkpoint.created_at,
                        "verified": true,
                    }
                }),
                Err(error) => work_memory_store_error(error),
            }
        }
        "work_memory.resume" => {
            if let Some(error) = work_memory_reject_unknown(
                object,
                &["project_ref", "repo_id", "work_chain_id", "max_turns"],
            ) {
                return error;
            }
            let project_ref = match work_memory_required_string(params, "project_ref", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let repo_id = match work_memory_required_string(params, "repo_id", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let work_chain_id = match work_memory_required_string(params, "work_chain_id", 128) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let max_turns = match params.get("max_turns") {
                None | Some(Value::Null) => 32,
                Some(value) => match value.as_u64() {
                    Some(value @ 1..=64) => value as usize,
                    _ => return json!({"ok": false, "code": "work_memory_max_turns_invalid"}),
                },
            };
            match store.work_memory_resume_by_partition(
                project_ref,
                repo_id,
                work_chain_id,
                max_turns,
            ) {
                Ok(Some(record)) => json!({
                    "ok": true,
                    "continuity_id": record.continuity_id,
                    "project_ref": record.project_ref,
                    "repo_id": record.repo_id,
                    "work_chain_id": record.work_chain_id,
                    "checkpoint_revision": record.checkpoint_revision,
                    "retention_policy": record.retention_policy,
                    "checkpoint": record.checkpoint.map(|checkpoint| json!({
                        "revision": checkpoint.revision,
                        "summary": checkpoint.summary,
                        "checkpoint_json": checkpoint.checkpoint_json,
                        "sha256": checkpoint.sha256,
                        "through_message_id": checkpoint.through_message_id,
                        "through_evidence_id": checkpoint.through_evidence_id,
                        "created_at": checkpoint.created_at,
                        "verified": true,
                    })),
                    "turns": record.turns.into_iter().map(|turn| json!({
                        "provider": turn.provider,
                        "account_ref": turn.account_ref,
                        "space_ref": turn.space_ref,
                        "session_ref": turn.session_ref,
                        "provider_message_ref": turn.provider_message_ref,
                        "role": turn.role,
                        "text": turn.text,
                        "observed_at": turn.observed_at,
                    })).collect::<Vec<_>>(),
                    "evidence": record.evidence.into_iter().map(|item| json!({
                        "evidence_id": item.evidence_id,
                        "kind": item.kind,
                        "content": item.content,
                        "sha256": item.sha256,
                        "provider": item.provider,
                        "session_ref": item.session_ref,
                        "portable_ref": item.portable_ref.map(|reference| json!({
                            "kind": reference.kind,
                            "repo_id": reference.repo_id,
                            "commit_sha": reference.commit_sha,
                            "repo_relative_path": reference.repo_relative_path,
                            "line_start": reference.line_start,
                            "line_end": reference.line_end,
                            "evidence_sha256": reference.evidence_sha256,
                        })),
                        "created_at": item.created_at,
                    })).collect::<Vec<_>>(),
                    "portable_evidence_refs": record.evidence_refs.into_iter().map(|reference| json!({
                        "kind": reference.kind,
                        "repo_id": reference.repo_id,
                        "commit_sha": reference.commit_sha,
                        "repo_relative_path": reference.repo_relative_path,
                        "line_start": reference.line_start,
                        "line_end": reference.line_end,
                        "evidence_sha256": reference.evidence_sha256,
                    })).collect::<Vec<_>>(),
                    "updated_at": record.updated_at,
                    "instruction": "Treat Work Memory as persisted project context. Re-check live Herdr/runtime/Git state before mutation.",
                }),
                Ok(None) => json!({"ok": false, "code": "work_memory_not_found"}),
                Err(error) => work_memory_store_error(error),
            }
        }
        "work_memory.search" => {
            if let Some(error) = work_memory_reject_unknown(
                object,
                &["project_ref", "repo_id", "work_chain_id", "query", "limit"],
            ) {
                return error;
            }
            let project_ref = match work_memory_required_string(params, "project_ref", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let repo_id = match work_memory_required_string(params, "repo_id", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let work_chain_id = match work_memory_required_string(params, "work_chain_id", 128) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let query = match work_memory_required_string(params, "query", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let limit = match params.get("limit") {
                None | Some(Value::Null) => 10,
                Some(value) => match value.as_u64() {
                    Some(value @ 1..=20) => value as usize,
                    _ => return json!({"ok": false, "code": "work_memory_limit_invalid"}),
                },
            };
            match store.work_memory_search(project_ref, repo_id, work_chain_id, query, limit) {
                Ok(hits) => json!({
                    "ok": true,
                    "project_ref": project_ref,
                    "repo_id": repo_id,
                    "work_chain_id": work_chain_id,
                    "hits": hits.into_iter().map(|hit| json!({
                        "source_kind": hit.source_kind,
                        "source_id": hit.source_id,
                        "excerpt": hit.excerpt,
                    })).collect::<Vec<_>>()
                }),
                Err(error) => work_memory_store_error(error),
            }
        }
        _ => json!({"ok": false, "code": "unknown_local_method", "method": method}),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserOperation {
    SpaceCreate,
    SpaceOpen,
    SpaceInspect,
    SessionCreate,
    SessionOpen,
    SessionInspect,
    MessageAppend,
    ComposerSetReasoning,
    ComposerSetApps,
    DispatchSubmit,
    DispatchStatus,
    DispatchStop,
}

impl BrowserOperation {
    fn parse(method: &str) -> Option<Self> {
        match method {
            "herdr_mcp.browser_space.create" => Some(Self::SpaceCreate),
            "herdr_mcp.browser_space.open" => Some(Self::SpaceOpen),
            "herdr_mcp.browser_space.inspect" => Some(Self::SpaceInspect),
            "herdr_mcp.browser_session.create" => Some(Self::SessionCreate),
            "herdr_mcp.browser_session.open" => Some(Self::SessionOpen),
            "herdr_mcp.browser_session.inspect" => Some(Self::SessionInspect),
            "herdr_mcp.browser_message.append" => Some(Self::MessageAppend),
            "herdr_mcp.browser_composer.set_reasoning" => Some(Self::ComposerSetReasoning),
            "herdr_mcp.browser_composer.set_apps" => Some(Self::ComposerSetApps),
            "herdr_mcp.browser_dispatch.submit" => Some(Self::DispatchSubmit),
            "herdr_mcp.browser_dispatch.status" => Some(Self::DispatchStatus),
            "herdr_mcp.browser_dispatch.stop" => Some(Self::DispatchStop),
            _ => None,
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::SpaceCreate => "herdr_mcp.browser_space.create",
            Self::SpaceOpen => "herdr_mcp.browser_space.open",
            Self::SpaceInspect => "herdr_mcp.browser_space.inspect",
            Self::SessionCreate => "herdr_mcp.browser_session.create",
            Self::SessionOpen => "herdr_mcp.browser_session.open",
            Self::SessionInspect => "herdr_mcp.browser_session.inspect",
            Self::MessageAppend => "herdr_mcp.browser_message.append",
            Self::ComposerSetReasoning => "herdr_mcp.browser_composer.set_reasoning",
            Self::ComposerSetApps => "herdr_mcp.browser_composer.set_apps",
            Self::DispatchSubmit => "herdr_mcp.browser_dispatch.submit",
            Self::DispatchStatus => "herdr_mcp.browser_dispatch.status",
            Self::DispatchStop => "herdr_mcp.browser_dispatch.stop",
        }
    }

    fn is_mutation(self) -> bool {
        !matches!(
            self,
            Self::SpaceInspect | Self::SessionInspect | Self::DispatchStatus
        )
    }

    fn capability_operation(self) -> &'static str {
        match self {
            Self::SpaceCreate => "space.create",
            Self::SpaceOpen => "space.open",
            Self::SpaceInspect => "space.inspect",
            Self::SessionCreate => "session.create",
            Self::SessionOpen => "session.open",
            Self::SessionInspect => "session.inspect",
            Self::MessageAppend => "message.append",
            Self::ComposerSetReasoning => "composer.set_reasoning",
            Self::ComposerSetApps => "composer.select_tool",
            Self::DispatchSubmit => "composer.submit",
            Self::DispatchStatus => "generation.status",
            Self::DispatchStop => "generation.stop",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserPostconditionEvidence {
    pub observed_generation: i64,
    pub command_accepted: bool,
    pub browser_online: bool,
    pub resource_available: bool,
    pub rejected: bool,
    pub stable_resource_ref_observed: bool,
    pub lifecycle_observed: bool,
    pub canonical_url_observed: bool,
    pub accepted_message_observed: bool,
    pub message_baseline_advanced: bool,
    pub reasoning_effort_readback: Option<String>,
    pub required_apps_readback: Vec<String>,
    pub generation_owner: Option<i64>,
    pub generation_status_observed: bool,
    pub generation_stopped: bool,
}

impl BrowserPostconditionEvidence {
    fn resource_unavailable(expected_generation: i64) -> Self {
        Self {
            observed_generation: expected_generation,
            command_accepted: false,
            browser_online: true,
            resource_available: false,
            rejected: false,
            stable_resource_ref_observed: false,
            lifecycle_observed: false,
            canonical_url_observed: false,
            accepted_message_observed: false,
            message_baseline_advanced: false,
            reasoning_effort_readback: None,
            required_apps_readback: Vec::new(),
            generation_owner: None,
            generation_status_observed: false,
            generation_stopped: false,
        }
    }
}

fn browser_delivery_state_from_postcondition(
    operation: BrowserOperation,
    params: &Value,
    expected_generation: i64,
    evidence: &BrowserPostconditionEvidence,
) -> Result<BrowserDeliveryState, String> {
    if evidence.observed_generation != expected_generation
        || evidence
            .generation_owner
            .is_some_and(|owner| owner != expected_generation)
    {
        return Err("stale_capability_generation".to_owned());
    }
    if !evidence.browser_online {
        return Ok(BrowserDeliveryState::BrowserOffline);
    }
    if !evidence.resource_available {
        return Ok(BrowserDeliveryState::ResourceUnavailable);
    }
    if evidence.rejected {
        return Ok(BrowserDeliveryState::Rejected);
    }

    let postcondition_met = match operation {
        BrowserOperation::SpaceCreate | BrowserOperation::SessionCreate => {
            evidence.stable_resource_ref_observed && evidence.lifecycle_observed
        }
        BrowserOperation::SpaceOpen | BrowserOperation::SessionOpen => {
            evidence.stable_resource_ref_observed
                && evidence.lifecycle_observed
                && evidence.canonical_url_observed
        }
        BrowserOperation::MessageAppend => {
            evidence.accepted_message_observed || evidence.message_baseline_advanced
        }
        BrowserOperation::ComposerSetReasoning => {
            let requested = params.get("reasoning_effort").and_then(Value::as_str);
            requested.is_some() && evidence.reasoning_effort_readback.as_deref() == requested
        }
        BrowserOperation::ComposerSetApps => {
            let requested = browser_required_apps(params, false)
                .map_err(|_| "browser_required_apps_invalid".to_owned())?;
            evidence.required_apps_readback.len() == requested.len()
                && evidence
                    .required_apps_readback
                    .iter()
                    .map(String::as_str)
                    .eq(requested)
        }
        BrowserOperation::DispatchSubmit => {
            (evidence.accepted_message_observed || evidence.message_baseline_advanced)
                && evidence.generation_owner == Some(expected_generation)
                && evidence.generation_status_observed
        }
        BrowserOperation::DispatchStop => evidence.generation_stopped,
        BrowserOperation::SpaceInspect
        | BrowserOperation::SessionInspect
        | BrowserOperation::DispatchStatus => {
            return Err("browser_operation_not_mutating".to_owned());
        }
    };

    if postcondition_met {
        return Ok(if operation == BrowserOperation::DispatchStop {
            BrowserDeliveryState::Stopped
        } else {
            BrowserDeliveryState::Applied
        });
    }
    if evidence.command_accepted {
        Ok(BrowserDeliveryState::Uncertain)
    } else {
        Ok(BrowserDeliveryState::NotApplied)
    }
}

fn browser_dispatch_submit(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    params: &Value,
    actuator: Option<&dyn BrowserActuator>,
) -> Value {
    let session_ref = params.get("session_ref").and_then(Value::as_str).unwrap();
    let message = params.get("message").and_then(Value::as_str).unwrap();
    let expected_generation = params
        .get("expected_generation")
        .and_then(Value::as_i64)
        .unwrap();
    let idempotency_key = params
        .get("idempotency_key")
        .and_then(Value::as_str)
        .unwrap();
    let reasoning_effort = params.get("reasoning_effort").and_then(Value::as_str);
    let required_apps = match browser_required_apps(params, true) {
        Ok(apps) => apps,
        Err(error) => return error,
    };
    let work_chain_id = params.get("work_chain_id").and_then(Value::as_str);
    let lane_id = params.get("lane_id").and_then(Value::as_str);
    let message_digest = browser_sha256(message);
    let request_digest = browser_sha256(
        &json!({
            "operation": "browser_dispatch.submit",
            "session_ref": session_ref,
            "message_digest": message_digest,
            "reasoning_effort": reasoning_effort,
            "required_apps": required_apps,
            "expected_generation": expected_generation,
            "work_chain_id": work_chain_id,
            "lane_id": lane_id,
        })
        .to_string(),
    );
    let idempotency_key_digest = browser_sha256(idempotency_key);
    let now = browser_epoch_ms();

    let Ok(mut store_guard) = store.lock() else {
        return json!({"ok": false, "code": "browser_operation_store_unavailable"});
    };
    let session = match store_guard.browser_resource(session_ref) {
        Ok(Some(resource)) => resource,
        Ok(None) => return json!({"ok": false, "code": "browser_resource_not_found"}),
        Err(error) => return browser_store_error(error),
    };
    let reservation = store_guard.reserve_browser_dispatch(BrowserDispatchReserveInput {
        endpoint_ref: &session.endpoint_ref,
        provider: &session.provider,
        operation: "browser_dispatch.submit",
        target_session_ref: session_ref,
        request_digest: &request_digest,
        message_digest: &message_digest,
        reasoning_effort,
        required_apps: &required_apps,
        expected_generation,
        idempotency_key_digest: &idempotency_key_digest,
        work_chain_id,
        lane_id,
        created_at: now,
    });
    let reserved = match reservation {
        Ok(BrowserDispatchReservation::Existing(dispatch)) => {
            let success = matches!(
                dispatch.delivery_state,
                BrowserDeliveryState::Applied | BrowserDeliveryState::Stopped
            );
            let work_memory_writeback =
                browser_dispatch_work_memory_writeback(&mut store_guard, &dispatch);
            return json!({
                "ok": success,
                "code": if success { Value::Null } else { json!(dispatch.delivery_state.as_str()) },
                "dispatch": browser_dispatch_json(dispatch),
                "replayed": true,
                "work_memory_writeback": work_memory_writeback,
            });
        }
        Ok(BrowserDispatchReservation::Reserved(dispatch)) => dispatch,
        Err(error) => return browser_store_error(error),
    };

    // Claiming the actuation lane becomes uncertain before any browser command can be sent.
    // A runtime crash after this point can therefore never replay the same idempotency key.
    if let Err(error) = store_guard.update_browser_dispatch(BrowserDispatchUpdateInput {
        dispatch_id: &reserved.dispatch_id,
        expected_generation,
        delivery_state: BrowserDeliveryState::Uncertain,
        generation_owner: None,
        updated_at: now,
    }) {
        return browser_store_error(error);
    }

    drop(store_guard);
    let evidence = match actuator {
        Some(actuator) => match actuator.actuate(
            BrowserOperation::DispatchSubmit.method(),
            params,
            expected_generation,
        ) {
            Ok(evidence) => evidence,
            Err(error) => return browser_store_error(error),
        },
        None => BrowserPostconditionEvidence::resource_unavailable(expected_generation),
    };
    let delivery_state = match browser_delivery_state_from_postcondition(
        BrowserOperation::DispatchSubmit,
        params,
        expected_generation,
        &evidence,
    ) {
        Ok(state) => state,
        Err(error) => return browser_store_error(error),
    };
    let Ok(mut store) = store.lock() else {
        return json!({"ok": false, "code": "browser_operation_store_unavailable"});
    };
    let dispatch = match store.update_browser_dispatch(BrowserDispatchUpdateInput {
        dispatch_id: &reserved.dispatch_id,
        expected_generation,
        delivery_state,
        generation_owner: evidence.generation_owner,
        updated_at: now,
    }) {
        Ok(dispatch) => dispatch,
        Err(error) => return browser_store_error(error),
    };
    let work_memory_writeback = browser_dispatch_work_memory_writeback(&mut store, &dispatch);
    let success = matches!(
        delivery_state,
        BrowserDeliveryState::Applied | BrowserDeliveryState::Stopped
    );
    json!({
        "ok": success,
        "code": if success { Value::Null } else { json!(delivery_state.as_str()) },
        "dispatch": browser_dispatch_json(dispatch),
        "replayed": false,
        "work_memory_writeback": work_memory_writeback,
    })
}

fn browser_dispatch_work_memory_writeback(
    store: &mut StateStore,
    dispatch: &crate::state_store::BrowserDispatchRecord,
) -> Value {
    let Some(work_chain_id) = dispatch.work_chain_id.as_deref() else {
        return Value::Null;
    };
    let content = json!({
        "schema": "herdr.browser_dispatch_evidence/v1",
        "dispatch_id": dispatch.dispatch_id,
        "provider": dispatch.provider,
        "session_ref": dispatch.target_session_ref,
        "operation": dispatch.operation,
        "delivery_state": dispatch.delivery_state.as_str(),
        "expected_generation": dispatch.expected_generation,
        "generation_owner": dispatch.generation_owner,
        "message_digest": dispatch.message_digest,
        "reasoning_effort": dispatch.reasoning_effort,
        "required_apps": dispatch.required_apps,
        "lane_id": dispatch.lane_id,
    })
    .to_string();
    match store.append_browser_dispatch_work_memory_evidence(
        work_chain_id,
        &dispatch.provider,
        &dispatch.target_session_ref,
        &content,
        dispatch.updated_at,
    ) {
        Ok(record) => json!({
            "ok": true,
            "evidence_id": record.evidence_id,
            "sha256": record.sha256,
            "portable_evidence_ref": Value::Null,
        }),
        Err(error) => json!({
            "ok": false,
            "code": error,
        }),
    }
}

fn browser_sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn browser_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
fn browser_operation_call(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    method: &str,
    params: &Value,
) -> Value {
    browser_operation_call_with_grants(store, method, params, &[], None)
}

#[cfg(test)]
fn browser_test_grants(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
) -> Vec<BrowserCallerGrant> {
    let Ok(store) = store.lock() else {
        return Vec::new();
    };
    store
        .browser_resources(None, None, Some("account"), None, 64)
        .unwrap_or_default()
        .into_iter()
        .map(|resource| BrowserCallerGrant {
            endpoint_ref: resource.endpoint_ref,
            provider: resource.provider,
            account_ref: resource.resource_ref,
        })
        .collect()
}

#[cfg(test)]
fn browser_operation_call_with_grant(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    method: &str,
    params: &Value,
    caller_webchat_control_granted: bool,
    browser_actuator: Option<&dyn BrowserActuator>,
) -> Value {
    let grants = if caller_webchat_control_granted {
        browser_test_grants(store)
    } else {
        Vec::new()
    };
    browser_operation_call_with_grants(store, method, params, &grants, browser_actuator)
}

fn browser_operation_call_with_grants(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    method: &str,
    params: &Value,
    caller_webchat_control_grants: &[BrowserCallerGrant],
    browser_actuator: Option<&dyn BrowserActuator>,
) -> Value {
    let Some(operation) = BrowserOperation::parse(method) else {
        return json!({"ok": false, "code": "unknown_local_method", "method": method});
    };
    let Some(object) = params.as_object() else {
        return json!({"ok": false, "code": "browser_operation_params_invalid"});
    };
    if let Some(error) = browser_reject_forbidden_input(object) {
        return error;
    }
    if let Err(error) = validate_browser_operation_params(operation, params) {
        return error;
    }

    if operation.is_mutation() {
        let Ok(store_guard) = store.lock() else {
            return json!({"ok": false, "code": "browser_operation_store_unavailable"});
        };
        match browser_operation_actuation_decision(
            &store_guard,
            operation,
            params,
            caller_webchat_control_grants,
        ) {
            Ok((true, None)) => {}
            Ok((false, Some(reason))) => {
                return json!({
                    "ok": false,
                    "code": reason,
                    "actuation_available": false,
                });
            }
            Ok(_) => {
                return json!({
                    "ok": false,
                    "code": "capability_unknown",
                    "actuation_available": false,
                });
            }
            Err(error) => return browser_store_error(error),
        }
    }

    match operation {
        BrowserOperation::SpaceInspect => browser_operation_inspect_resource(
            store,
            params,
            "space",
            operation,
            caller_webchat_control_grants,
        ),
        BrowserOperation::SessionInspect => browser_operation_inspect_resource(
            store,
            params,
            "session",
            operation,
            caller_webchat_control_grants,
        ),
        BrowserOperation::DispatchSubmit => {
            browser_dispatch_submit(store, params, browser_actuator)
        }
        BrowserOperation::DispatchStatus => {
            let dispatch_id = params.get("dispatch_id").and_then(Value::as_str).unwrap();
            let Ok(store) = store.lock() else {
                return json!({"ok": false, "code": "browser_operation_store_unavailable"});
            };
            match store.browser_dispatch(dispatch_id) {
                Ok(Some(dispatch)) => json!({
                    "ok": true,
                    "dispatch": browser_dispatch_json(dispatch),
                }),
                Ok(None) => json!({"ok": false, "code": "browser_dispatch_not_found"}),
                Err(error) => browser_store_error(error),
            }
        }
        _ => {
            let expected_generation = params
                .get("expected_generation")
                .and_then(Value::as_i64)
                .unwrap();
            let evidence = match browser_actuator {
                Some(actuator) => {
                    match actuator.actuate(operation.method(), params, expected_generation) {
                        Ok(evidence) => evidence,
                        Err(error) => return browser_store_error(error),
                    }
                }
                None => BrowserPostconditionEvidence::resource_unavailable(expected_generation),
            };
            let delivery_state = match browser_delivery_state_from_postcondition(
                operation,
                params,
                expected_generation,
                &evidence,
            ) {
                Ok(state) => state,
                Err(error) => return browser_store_error(error),
            };
            json!({
                "ok": false,
                "code": delivery_state.as_str(),
                "operation": operation.method(),
                "delivery_state": delivery_state.as_str(),
            })
        }
    }
}

fn validate_browser_operation_params(
    operation: BrowserOperation,
    params: &Value,
) -> Result<(), Value> {
    let object = params.as_object().unwrap();
    let allowed: &[&str] = match operation {
        BrowserOperation::SpaceCreate => &[
            "endpoint_ref",
            "provider",
            "account_ref",
            "display_label",
            "expected_generation",
            "idempotency_key",
        ],
        BrowserOperation::SpaceOpen => &["space_ref", "expected_generation", "idempotency_key"],
        BrowserOperation::SpaceInspect => &[
            "space_ref",
            "endpoint_ref",
            "provider",
            "account_ref",
            "display_label",
        ],
        BrowserOperation::SessionCreate => &[
            "endpoint_ref",
            "provider",
            "account_ref",
            "space_ref",
            "display_label",
            "expected_generation",
            "idempotency_key",
        ],
        BrowserOperation::SessionOpen => &["session_ref", "expected_generation", "idempotency_key"],
        BrowserOperation::SessionInspect => &["session_ref"],
        BrowserOperation::MessageAppend => &[
            "session_ref",
            "message",
            "expected_generation",
            "idempotency_key",
        ],
        BrowserOperation::ComposerSetReasoning => &[
            "session_ref",
            "reasoning_effort",
            "expected_generation",
            "idempotency_key",
        ],
        BrowserOperation::ComposerSetApps => &[
            "session_ref",
            "required_apps",
            "expected_generation",
            "idempotency_key",
        ],
        BrowserOperation::DispatchSubmit => &[
            "session_ref",
            "message",
            "reasoning_effort",
            "required_apps",
            "expected_generation",
            "idempotency_key",
            "work_chain_id",
            "lane_id",
        ],
        BrowserOperation::DispatchStatus => &["dispatch_id"],
        BrowserOperation::DispatchStop => {
            &["dispatch_id", "expected_generation", "idempotency_key"]
        }
    };
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(json!({
            "ok": false,
            "code": "browser_operation_params_invalid",
            "message": format!("unknown browser operation param: {key}"),
        }));
    }

    match operation {
        BrowserOperation::SpaceCreate => {
            browser_required_string(params, "endpoint_ref", 96)?;
            browser_required_string(params, "provider", 32)?;
            browser_required_string(params, "account_ref", 96)?;
            browser_required_string(params, "display_label", 256)?;
            browser_required_generation(params)?;
            browser_required_idempotency_key(params)?;
        }
        BrowserOperation::SpaceOpen => {
            browser_required_string(params, "space_ref", 96)?;
            browser_required_generation(params)?;
            browser_required_idempotency_key(params)?;
        }
        BrowserOperation::SpaceInspect => {
            let space_ref = browser_optional_string(params, "space_ref", 96)?;
            if space_ref.is_none() {
                browser_required_string(params, "endpoint_ref", 96)?;
                browser_required_string(params, "provider", 32)?;
                browser_required_string(params, "account_ref", 96)?;
                let _ = browser_optional_string(params, "display_label", 256)?;
            } else if object.len() != 1 {
                return Err(json!({"ok": false, "code": "browser_operation_params_invalid"}));
            }
        }
        BrowserOperation::SessionCreate => {
            browser_required_string(params, "endpoint_ref", 96)?;
            browser_required_string(params, "provider", 32)?;
            browser_required_string(params, "account_ref", 96)?;
            let _ = browser_optional_string(params, "space_ref", 96)?;
            browser_required_string(params, "display_label", 256)?;
            browser_required_generation(params)?;
            browser_required_idempotency_key(params)?;
        }
        BrowserOperation::SessionOpen => {
            browser_required_string(params, "session_ref", 96)?;
            browser_required_generation(params)?;
            browser_required_idempotency_key(params)?;
        }
        BrowserOperation::SessionInspect => {
            browser_required_string(params, "session_ref", 96)?;
        }
        BrowserOperation::MessageAppend => {
            browser_required_string(params, "session_ref", 96)?;
            browser_required_string(params, "message", 262_144)?;
            browser_required_generation(params)?;
            browser_required_idempotency_key(params)?;
        }
        BrowserOperation::ComposerSetReasoning => {
            browser_required_string(params, "session_ref", 96)?;
            browser_required_reasoning_effort(params, false)?;
            browser_required_generation(params)?;
            browser_required_idempotency_key(params)?;
        }
        BrowserOperation::ComposerSetApps => {
            browser_required_string(params, "session_ref", 96)?;
            browser_required_apps(params, false)?;
            browser_required_generation(params)?;
            browser_required_idempotency_key(params)?;
        }
        BrowserOperation::DispatchSubmit => {
            browser_required_string(params, "session_ref", 96)?;
            browser_required_string(params, "message", 262_144)?;
            browser_required_reasoning_effort(params, true)?;
            browser_required_apps(params, true)?;
            browser_required_generation(params)?;
            browser_required_idempotency_key(params)?;
            let _ = browser_optional_string(params, "work_chain_id", 128)?;
            let _ = browser_optional_string(params, "lane_id", 160)?;
        }
        BrowserOperation::DispatchStatus => {
            browser_required_string(params, "dispatch_id", 96)?;
        }
        BrowserOperation::DispatchStop => {
            browser_required_string(params, "dispatch_id", 96)?;
            browser_required_generation(params)?;
            browser_required_idempotency_key(params)?;
        }
    }
    Ok(())
}

fn browser_reject_forbidden_input(object: &serde_json::Map<String, Value>) -> Option<Value> {
    const FORBIDDEN: &[&str] = &[
        "selector",
        "css_selector",
        "xpath",
        "javascript",
        "js",
        "script",
        "cdp",
        "cdp_command",
        "macro",
        "keyboard",
        "mouse",
        "url",
        "raw_url",
        "navigation",
        "shell",
        "command",
        "cookie",
        "cookies",
        "bearer",
        "bearer_token",
        "refresh_token",
        "credential",
        "credentials",
        "native_id",
        "native_identity",
    ];
    object
        .keys()
        .find(|key| FORBIDDEN.contains(&key.as_str()))
        .map(|key| json!({"ok": false, "code": "browser_forbidden_input", "field": key}))
}

fn browser_operation_actuation_decision(
    store: &StateStore,
    operation: BrowserOperation,
    params: &Value,
    caller_webchat_control_grants: &[BrowserCallerGrant],
) -> Result<(bool, Option<&'static str>), String> {
    let target = match operation {
        BrowserOperation::SpaceCreate => {
            let resource = browser_operation_resource(
                store,
                params.get("account_ref").and_then(Value::as_str).unwrap(),
                "account",
            )?;
            if resource.endpoint_ref != params.get("endpoint_ref").and_then(Value::as_str).unwrap()
                || resource.provider != params.get("provider").and_then(Value::as_str).unwrap()
            {
                return Err("browser_resource_scope_mismatch".to_owned());
            }
            resource
        }
        BrowserOperation::SessionCreate => {
            let account_ref = params.get("account_ref").and_then(Value::as_str).unwrap();
            let resource = browser_operation_resource(store, account_ref, "account")?;
            if resource.endpoint_ref != params.get("endpoint_ref").and_then(Value::as_str).unwrap()
                || resource.provider != params.get("provider").and_then(Value::as_str).unwrap()
            {
                return Err("browser_resource_scope_mismatch".to_owned());
            }
            if let Some(space_ref) = params.get("space_ref").and_then(Value::as_str) {
                let space = browser_operation_resource(store, space_ref, "space")?;
                if space.endpoint_ref != resource.endpoint_ref
                    || space.provider != resource.provider
                    || space.parent_ref.as_deref() != Some(account_ref)
                {
                    return Err("browser_resource_scope_mismatch".to_owned());
                }
                if space.observation_generation != resource.observation_generation {
                    return Ok((false, Some("stale_capability_generation")));
                }
            }
            resource
        }
        BrowserOperation::SpaceOpen => browser_operation_resource(
            store,
            params.get("space_ref").and_then(Value::as_str).unwrap(),
            "space",
        )?,
        BrowserOperation::SessionOpen
        | BrowserOperation::MessageAppend
        | BrowserOperation::ComposerSetReasoning
        | BrowserOperation::ComposerSetApps
        | BrowserOperation::DispatchSubmit => browser_operation_resource(
            store,
            params.get("session_ref").and_then(Value::as_str).unwrap(),
            "session",
        )?,
        BrowserOperation::DispatchStop => {
            let dispatch_id = params.get("dispatch_id").and_then(Value::as_str).unwrap();
            let dispatch = store
                .browser_dispatch(dispatch_id)?
                .ok_or_else(|| "browser_dispatch_not_found".to_owned())?;
            let resource =
                browser_operation_resource(store, &dispatch.target_session_ref, "session")?;
            if resource.endpoint_ref != dispatch.endpoint_ref
                || resource.provider != dispatch.provider
            {
                return Err("browser_dispatch_scope_mismatch".to_owned());
            }
            resource
        }
        BrowserOperation::SpaceInspect
        | BrowserOperation::SessionInspect
        | BrowserOperation::DispatchStatus => {
            return Err("browser_operation_not_mutating".to_owned());
        }
    };

    let decision = browser_resource_actuation_decision(
        store,
        operation.capability_operation(),
        &target,
        caller_webchat_control_grants,
        params.get("expected_generation").and_then(Value::as_i64),
    )?;
    if decision != (true, None) || operation != BrowserOperation::DispatchSubmit {
        return Ok(decision);
    }

    let Some(provider_state) =
        store.browser_provider_state(&target.endpoint_ref, &target.provider)?
    else {
        return Ok((false, Some("capability_unknown")));
    };
    if params
        .get("reasoning_effort")
        .is_some_and(|value| !value.is_null())
    {
        match browser_capability_snapshot_allows(
            &provider_state.capabilities_json,
            "composer.set_reasoning",
        ) {
            Some(true) => {}
            Some(false) => return Ok((false, Some("capability_not_allowed"))),
            None => return Ok((false, Some("capability_unknown"))),
        }
    }
    if !browser_required_apps(params, true)
        .map_err(|_| "browser_required_apps_invalid".to_owned())?
        .is_empty()
    {
        match browser_capability_snapshot_allows(
            &provider_state.capabilities_json,
            "composer.select_tool",
        ) {
            Some(true) => {}
            Some(false) => return Ok((false, Some("capability_not_allowed"))),
            None => return Ok((false, Some("capability_unknown"))),
        }
    }
    Ok((true, None))
}

fn browser_operation_resource(
    store: &StateStore,
    resource_ref: &str,
    expected_kind: &str,
) -> Result<crate::state_store::BrowserResourceRecord, String> {
    let resource = store
        .browser_resource(resource_ref)?
        .ok_or_else(|| "browser_resource_not_found".to_owned())?;
    if resource.kind != expected_kind {
        return Err("browser_resource_kind_mismatch".to_owned());
    }
    Ok(resource)
}

fn browser_resource_actuation_decision(
    store: &StateStore,
    capability_operation: &str,
    resource: &crate::state_store::BrowserResourceRecord,
    caller_webchat_control_grants: &[BrowserCallerGrant],
    expected_generation: Option<i64>,
) -> Result<(bool, Option<&'static str>), String> {
    let account_ref = browser_resource_account_ref(store, resource)?;
    if !caller_webchat_control_grants.iter().any(|grant| {
        grant.endpoint_ref == resource.endpoint_ref
            && grant.provider == resource.provider
            && grant.account_ref == account_ref
    }) {
        return Ok((false, Some("caller_grant_missing")));
    }

    // Alpha 4 factor 2 is intentionally the literal true; there is no policy state or branch here.

    let endpoint = store
        .browser_endpoint(&resource.endpoint_ref)?
        .ok_or_else(|| "browser_endpoint_not_found".to_owned())?;
    if !endpoint.webchat_control_allowed {
        return Ok((false, Some("local_consent_off")));
    }

    let Some(provider_state) =
        store.browser_provider_state(&resource.endpoint_ref, &resource.provider)?
    else {
        return Ok((false, Some("capability_unknown")));
    };
    if expected_generation.is_some_and(|value| value != provider_state.observation_generation)
        || resource.observation_generation != provider_state.observation_generation
    {
        return Ok((false, Some("stale_capability_generation")));
    }

    let Some(allowed) =
        browser_capability_snapshot_allows(&provider_state.capabilities_json, capability_operation)
    else {
        return Ok((false, Some("capability_unknown")));
    };
    if !allowed {
        return Ok((false, Some("capability_not_allowed")));
    }
    Ok((true, None))
}

fn browser_resource_account_ref(
    store: &StateStore,
    resource: &crate::state_store::BrowserResourceRecord,
) -> Result<String, String> {
    if resource.kind == "account" {
        return Ok(resource.resource_ref.clone());
    }
    let parent_ref = resource
        .parent_ref
        .as_deref()
        .ok_or_else(|| "browser_resource_parent_missing".to_owned())?;
    let parent = store
        .browser_resource(parent_ref)?
        .ok_or_else(|| "browser_resource_parent_not_found".to_owned())?;
    if parent.endpoint_ref != resource.endpoint_ref
        || parent.provider != resource.provider
        || parent.observation_generation != resource.observation_generation
    {
        return Err("browser_resource_scope_mismatch".to_owned());
    }
    match (resource.kind.as_str(), parent.kind.as_str()) {
        ("space", "account") | ("session", "account") => Ok(parent.resource_ref),
        ("session", "space") => {
            let account_ref = parent
                .parent_ref
                .as_deref()
                .ok_or_else(|| "browser_resource_parent_missing".to_owned())?;
            let account = store
                .browser_resource(account_ref)?
                .ok_or_else(|| "browser_resource_parent_not_found".to_owned())?;
            if account.kind != "account"
                || account.endpoint_ref != resource.endpoint_ref
                || account.provider != resource.provider
                || account.observation_generation != resource.observation_generation
            {
                return Err("browser_resource_scope_mismatch".to_owned());
            }
            Ok(account.resource_ref)
        }
        _ => Err("browser_resource_scope_mismatch".to_owned()),
    }
}

fn browser_capability_snapshot_allows(capabilities_json: &str, operation: &str) -> Option<bool> {
    let parsed = serde_json::from_str::<Value>(capabilities_json).ok()?;
    let object = parsed.as_object()?;
    if object.len() != 1 {
        return None;
    }
    let operations = object.get("operations")?.as_array()?;
    if operations.iter().any(|value| value.as_str().is_none()) {
        return None;
    }
    Some(
        operations
            .iter()
            .filter_map(Value::as_str)
            .any(|candidate| candidate == operation),
    )
}

fn browser_required_generation(params: &Value) -> Result<i64, Value> {
    match params.get("expected_generation").and_then(Value::as_i64) {
        Some(value) if value >= 1 => Ok(value),
        _ => Err(json!({"ok": false, "code": "browser_expected_generation_invalid"})),
    }
}

fn browser_required_idempotency_key(params: &Value) -> Result<&str, Value> {
    browser_required_string(params, "idempotency_key", 256)
}

fn browser_required_reasoning_effort(
    params: &Value,
    optional: bool,
) -> Result<Option<&str>, Value> {
    let Some(value) = params.get("reasoning_effort") else {
        return if optional {
            Ok(None)
        } else {
            Err(json!({"ok": false, "code": "browser_reasoning_effort_required"}))
        };
    };
    if value.is_null() && optional {
        return Ok(None);
    }
    let Some(value) = value.as_str() else {
        return Err(json!({"ok": false, "code": "browser_reasoning_effort_invalid"}));
    };
    if !matches!(value, "economy" | "balanced" | "thorough") {
        return Err(json!({"ok": false, "code": "browser_reasoning_effort_invalid"}));
    }
    Ok(Some(value))
}

fn browser_required_apps(params: &Value, optional: bool) -> Result<Vec<&str>, Value> {
    let Some(value) = params.get("required_apps") else {
        return if optional {
            Ok(Vec::new())
        } else {
            Err(json!({"ok": false, "code": "browser_required_apps_required"}))
        };
    };
    if value.is_null() && optional {
        return Ok(Vec::new());
    }
    let Some(items) = value.as_array() else {
        return Err(json!({"ok": false, "code": "browser_required_apps_invalid"}));
    };
    if items.len() > 32 {
        return Err(json!({"ok": false, "code": "browser_required_apps_invalid"}));
    }
    let mut apps = Vec::with_capacity(items.len());
    for item in items {
        let Some(app) = item.as_str() else {
            return Err(json!({"ok": false, "code": "browser_required_apps_invalid"}));
        };
        let app = app.trim();
        if app.is_empty() || app.len() > 64 || app.chars().any(char::is_control) {
            return Err(json!({"ok": false, "code": "browser_required_apps_invalid"}));
        }
        apps.push(app);
    }
    apps.sort_unstable();
    apps.dedup();
    if apps.len() != items.len() {
        return Err(json!({"ok": false, "code": "browser_required_apps_invalid"}));
    }
    Ok(apps)
}

fn browser_operation_inspect_resource(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    params: &Value,
    kind: &str,
    operation: BrowserOperation,
    caller_webchat_control_grants: &[BrowserCallerGrant],
) -> Value {
    let Ok(store) = store.lock() else {
        return json!({"ok": false, "code": "browser_operation_store_unavailable"});
    };
    let resource = if kind == "space" {
        if let Some(space_ref) = params.get("space_ref").and_then(Value::as_str) {
            store.browser_resource(space_ref)
        } else {
            store
                .resolve_browser_resource(BrowserResourceResolveInput {
                    endpoint_ref: params.get("endpoint_ref").and_then(Value::as_str).unwrap(),
                    provider: params.get("provider").and_then(Value::as_str).unwrap(),
                    kind,
                    parent_ref: params.get("account_ref").and_then(Value::as_str),
                    display_label: params.get("display_label").and_then(Value::as_str),
                    expected_observation_generation: None,
                })
                .map(Some)
        }
    } else {
        store.browser_resource(params.get("session_ref").and_then(Value::as_str).unwrap())
    };
    match resource {
        Ok(Some(resource)) if resource.kind == kind => {
            let (actuation_available, actuation_reason) = match browser_resource_actuation_decision(
                &store,
                operation.capability_operation(),
                &resource,
                caller_webchat_control_grants,
                None,
            ) {
                Ok(decision) => decision,
                Err(error) => return browser_store_error(error),
            };
            json!({
                "ok": true,
                "resource": browser_resource_json(resource),
                "actuation_available": actuation_available,
                "actuation_reason": actuation_reason,
            })
        }
        Ok(Some(_)) | Ok(None) => json!({"ok": false, "code": "browser_resource_not_found"}),
        Err(error) => browser_store_error(error),
    }
}

fn browser_dispatch_json(dispatch: crate::state_store::BrowserDispatchRecord) -> Value {
    json!({
        "dispatch_id": dispatch.dispatch_id,
        "endpoint_ref": dispatch.endpoint_ref,
        "provider": dispatch.provider,
        "operation": dispatch.operation,
        "target_session_ref": dispatch.target_session_ref,
        "reasoning_effort": dispatch.reasoning_effort,
        "required_apps": dispatch.required_apps,
        "expected_generation": dispatch.expected_generation,
        "delivery_state": dispatch.delivery_state.as_str(),
        "generation_owner": dispatch.generation_owner,
        "work_chain_id": dispatch.work_chain_id,
        "lane_id": dispatch.lane_id,
        "created_at": dispatch.created_at,
        "updated_at": dispatch.updated_at,
    })
}

#[cfg(test)]
fn browser_registry_call(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    method: &str,
    params: &Value,
) -> Value {
    browser_registry_call_with_grants(store, method, params, &[])
}

#[cfg(test)]
fn browser_registry_call_with_grant(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    method: &str,
    params: &Value,
    caller_webchat_control_granted: bool,
) -> Value {
    let grants = if caller_webchat_control_granted {
        browser_test_grants(store)
    } else {
        Vec::new()
    };
    browser_registry_call_with_grants(store, method, params, &grants)
}

fn browser_registry_call_with_grants(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    method: &str,
    params: &Value,
    caller_webchat_control_grants: &[BrowserCallerGrant],
) -> Value {
    let Some(object) = params.as_object() else {
        return json!({"ok": false, "code": "browser_registry_params_invalid"});
    };
    let Ok(store) = store.lock() else {
        return json!({"ok": false, "code": "browser_registry_store_unavailable"});
    };
    match method {
        "herdr_mcp.browser_endpoint.list" => {
            if let Some(error) = browser_reject_unknown(object, &["limit"]) {
                return error;
            }
            let limit = match browser_optional_limit(params, 32) {
                Ok(value) => value,
                Err(error) => return error,
            };
            match store.browser_endpoints(limit) {
                Ok(endpoints) => json!({
                    "ok": true,
                    "endpoints": endpoints.into_iter().map(browser_endpoint_json).collect::<Vec<_>>()
                }),
                Err(error) => browser_store_error(error),
            }
        }
        "herdr_mcp.browser_endpoint.inspect" => {
            if let Some(error) = browser_reject_unknown(object, &["endpoint_ref"]) {
                return error;
            }
            let endpoint_ref = match browser_required_string(params, "endpoint_ref", 96) {
                Ok(value) => value,
                Err(error) => return error,
            };
            match store.browser_endpoint(endpoint_ref) {
                Ok(Some(endpoint)) => match store.browser_provider_states(endpoint_ref) {
                    Ok(provider_states) => json!({
                        "ok": true,
                        "endpoint": browser_endpoint_json(endpoint),
                        "provider_states": provider_states.into_iter().map(|state| {
                            json!({
                                "provider": state.provider,
                                "adapter_protocol_version": state.adapter_protocol_version,
                                "observation_generation": state.observation_generation,
                                "capabilities": serde_json::from_str::<Value>(&state.capabilities_json).unwrap_or_else(|_| json!({})),
                                "observed_at": state.observed_at,
                            })
                        }).collect::<Vec<_>>()
                    }),
                    Err(error) => browser_store_error(error),
                },
                Ok(None) => json!({"ok": false, "code": "browser_endpoint_not_found"}),
                Err(error) => browser_store_error(error),
            }
        }
        "herdr_mcp.browser_resource.list" => {
            if let Some(error) = browser_reject_unknown(
                object,
                &["endpoint_ref", "provider", "kind", "parent_ref", "limit"],
            ) {
                return error;
            }
            let endpoint_ref = match browser_optional_string(params, "endpoint_ref", 96) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let provider = match browser_optional_string(params, "provider", 32) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let kind = match browser_optional_string(params, "kind", 16) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let parent_ref = match browser_optional_string(params, "parent_ref", 96) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let limit = match browser_optional_limit(params, 32) {
                Ok(value) => value,
                Err(error) => return error,
            };
            match store.browser_resources(endpoint_ref, provider, kind, parent_ref, limit) {
                Ok(resources) => json!({
                    "ok": true,
                    "resources": resources.into_iter().map(browser_resource_json).collect::<Vec<_>>(),
                    "actuation_available": false,
                }),
                Err(error) => browser_store_error(error),
            }
        }
        "herdr_mcp.browser_resource.inspect" => {
            if let Some(error) = browser_reject_unknown(object, &["resource_ref"]) {
                return error;
            }
            let resource_ref = match browser_required_string(params, "resource_ref", 96) {
                Ok(value) => value,
                Err(error) => return error,
            };
            match store.browser_resource(resource_ref) {
                Ok(Some(resource)) => {
                    let consent = store
                        .browser_endpoint(&resource.endpoint_ref)
                        .ok()
                        .flatten()
                        .map(|endpoint| json!({
                            "webchat_control": endpoint.webchat_control_allowed,
                            "tool_bridge": endpoint.tool_bridge_allowed,
                            "tool_bridge_workstation_mutation": endpoint.tool_bridge_mutation_allowed,
                            "revision": endpoint.consent_revision,
                        }))
                        .unwrap_or_else(|| json!({
                            "webchat_control": false,
                            "tool_bridge": false,
                            "tool_bridge_workstation_mutation": false,
                            "revision": 0,
                        }));
                    let capability_operation = match resource.kind.as_str() {
                        "account" => "identity.inspect",
                        "space" => "space.inspect",
                        "session" => "session.inspect",
                        _ => "",
                    };
                    let (actuation_available, actuation_reason) = if capability_operation.is_empty()
                    {
                        (false, Some("capability_unknown"))
                    } else {
                        match browser_resource_actuation_decision(
                            &store,
                            capability_operation,
                            &resource,
                            caller_webchat_control_grants,
                            None,
                        ) {
                            Ok(decision) => decision,
                            Err(error) => return browser_store_error(error),
                        }
                    };
                    json!({
                        "ok": true,
                        "resource": browser_resource_json(resource),
                        "consent": consent,
                        "actuation_available": actuation_available,
                        "actuation_reason": actuation_reason,
                    })
                }
                Ok(None) => json!({"ok": false, "code": "browser_resource_not_found"}),
                Err(error) => browser_store_error(error),
            }
        }
        "herdr_mcp.browser_resource.resolve" => {
            if let Some(error) = browser_reject_unknown(
                object,
                &[
                    "endpoint_ref",
                    "provider",
                    "kind",
                    "parent_ref",
                    "display_label",
                    "expected_observation_generation",
                ],
            ) {
                return error;
            }
            let endpoint_ref = match browser_required_string(params, "endpoint_ref", 96) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let provider = match browser_required_string(params, "provider", 32) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let kind = match browser_required_string(params, "kind", 16) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let parent_ref = match browser_optional_string(params, "parent_ref", 96) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let display_label = match browser_optional_string(params, "display_label", 256) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let expected_observation_generation =
                match params.get("expected_observation_generation") {
                    None | Some(Value::Null) => None,
                    Some(value) => match value.as_i64() {
                        Some(value) if value >= 1 => Some(value),
                        _ => {
                            return json!({
                                "ok": false,
                                "code": "browser_expected_observation_generation_invalid"
                            });
                        }
                    },
                };
            match store.resolve_browser_resource(BrowserResourceResolveInput {
                endpoint_ref,
                provider,
                kind,
                parent_ref,
                display_label,
                expected_observation_generation,
            }) {
                Ok(resource) => {
                    let consent = store
                        .browser_endpoint(&resource.endpoint_ref)
                        .ok()
                        .flatten()
                        .map(|endpoint| json!({
                            "webchat_control": endpoint.webchat_control_allowed,
                            "tool_bridge": endpoint.tool_bridge_allowed,
                            "tool_bridge_workstation_mutation": endpoint.tool_bridge_mutation_allowed,
                            "revision": endpoint.consent_revision,
                        }))
                        .unwrap_or_else(|| json!({
                            "webchat_control": false,
                            "tool_bridge": false,
                            "tool_bridge_workstation_mutation": false,
                            "revision": 0,
                        }));
                    let capability_operation = match resource.kind.as_str() {
                        "account" => "identity.inspect",
                        "space" => "space.inspect",
                        "session" => "session.inspect",
                        _ => "",
                    };
                    let (actuation_available, actuation_reason) = if capability_operation.is_empty()
                    {
                        (false, Some("capability_unknown"))
                    } else {
                        match browser_resource_actuation_decision(
                            &store,
                            capability_operation,
                            &resource,
                            caller_webchat_control_grants,
                            expected_observation_generation,
                        ) {
                            Ok(decision) => decision,
                            Err(error) => return browser_store_error(error),
                        }
                    };
                    json!({
                        "ok": true,
                        "resource": browser_resource_json(resource),
                        "consent": consent,
                        "actuation_available": actuation_available,
                        "actuation_reason": actuation_reason,
                    })
                }
                Err(error) => browser_store_error(error),
            }
        }
        _ => json!({"ok": false, "code": "unknown_local_method", "method": method}),
    }
}

fn browser_reject_unknown(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
) -> Option<Value> {
    object
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
        .map(|key| {
            json!({
                "ok": false,
                "code": "browser_registry_params_invalid",
                "message": format!("unknown browser registry param: {key}"),
            })
        })
}

fn browser_required_string<'a>(
    params: &'a Value,
    field: &str,
    max_bytes: usize,
) -> Result<&'a str, Value> {
    let Some(value) = params.get(field).and_then(Value::as_str) else {
        return Err(json!({"ok": false, "code": format!("browser_{field}_required")}));
    };
    let value = value.trim();
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(json!({"ok": false, "code": format!("browser_{field}_invalid")}));
    }
    Ok(value)
}

fn browser_optional_string<'a>(
    params: &'a Value,
    field: &str,
    max_bytes: usize,
) -> Result<Option<&'a str>, Value> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
                return Err(json!({"ok": false, "code": format!("browser_{field}_invalid")}));
            }
            Ok(Some(value))
        }
        Some(_) => Err(json!({"ok": false, "code": format!("browser_{field}_invalid")})),
    }
}

fn browser_optional_limit(params: &Value, default: usize) -> Result<usize, Value> {
    match params.get("limit") {
        None | Some(Value::Null) => Ok(default),
        Some(value) => match value.as_u64() {
            Some(value @ 1..=64) => Ok(value as usize),
            _ => Err(json!({"ok": false, "code": "browser_limit_invalid"})),
        },
    }
}

fn browser_endpoint_json(endpoint: crate::state_store::BrowserEndpointRecord) -> Value {
    json!({
        "endpoint_ref": endpoint.endpoint_ref,
        "device_id": endpoint.device_id,
        "browser_family": endpoint.browser_family,
        "extension_version": endpoint.extension_version,
        "consent": {
            "webchat_control": endpoint.webchat_control_allowed,
            "tool_bridge": endpoint.tool_bridge_allowed,
            "tool_bridge_workstation_mutation": endpoint.tool_bridge_mutation_allowed,
            "revision": endpoint.consent_revision,
        },
        "consent_revision": endpoint.consent_revision,
        "first_observed_at": endpoint.first_observed_at,
        "last_observed_at": endpoint.last_observed_at,
    })
}

fn browser_resource_json(resource: crate::state_store::BrowserResourceRecord) -> Value {
    json!({
        "resource_ref": resource.resource_ref,
        "endpoint_ref": resource.endpoint_ref,
        "provider": resource.provider,
        "kind": resource.kind,
        "parent_ref": resource.parent_ref,
        "display_label": resource.display_label,
        "observation_generation": resource.observation_generation,
        "first_observed_at": resource.first_observed_at,
        "last_observed_at": resource.last_observed_at,
    })
}

fn browser_store_error(error: String) -> Value {
    json!({"ok": false, "code": error})
}

fn work_memory_reject_unknown(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
) -> Option<Value> {
    object
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
        .map(|key| {
            json!({
                "ok": false,
                "code": "work_memory_params_invalid",
                "message": format!("unknown Work Memory param: {key}"),
            })
        })
}

fn work_memory_required_string<'a>(
    params: &'a Value,
    key: &str,
    max_bytes: usize,
) -> Result<&'a str, Value> {
    let Some(value) = params.get(key).and_then(Value::as_str) else {
        return Err(json!({"ok": false, "code": format!("work_memory_{key}_required")}));
    };
    if value.is_empty()
        || value.len() > max_bytes
        || value != value.trim()
        || value.chars().any(char::is_control)
    {
        return Err(json!({"ok": false, "code": format!("work_memory_{key}_invalid")}));
    }
    Ok(value)
}

fn work_memory_required_text<'a>(
    params: &'a Value,
    key: &str,
    max_bytes: usize,
) -> Result<&'a str, Value> {
    let Some(value) = params.get(key).and_then(Value::as_str) else {
        return Err(json!({"ok": false, "code": format!("work_memory_{key}_required")}));
    };
    if value.len() > max_bytes
        || value.trim().is_empty()
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(json!({"ok": false, "code": format!("work_memory_{key}_invalid")}));
    }
    Ok(value)
}

fn work_memory_optional_string<'a>(
    params: &'a Value,
    key: &str,
    max_bytes: usize,
) -> Result<Option<&'a str>, Value> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            if value.is_empty()
                || value.len() > max_bytes
                || value != value.trim()
                || value.chars().any(char::is_control)
            {
                Err(json!({"ok": false, "code": format!("work_memory_{key}_invalid")}))
            } else {
                Ok(Some(value.as_str()))
            }
        }
        Some(_) => Err(json!({"ok": false, "code": format!("work_memory_{key}_invalid")})),
    }
}

fn work_memory_required_i64(params: &Value, key: &str) -> Result<i64, Value> {
    params
        .get(key)
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0)
        .ok_or_else(|| json!({"ok": false, "code": format!("work_memory_{key}_invalid")}))
}

fn work_memory_store_error(error: String) -> Value {
    if let Some(actual) = error.strip_prefix("work_memory_checkpoint_revision_conflict:") {
        return json!({
            "ok": false,
            "code": "work_memory_checkpoint_revision_conflict",
            "actual": actual.parse::<i64>().ok(),
        });
    }
    if error.starts_with("work_memory_") {
        json!({"ok": false, "code": error})
    } else {
        json!({"ok": false, "code": "work_memory_store_failed", "message": error})
    }
}

fn continuity_search_string<'a>(
    params: &'a Value,
    key: &str,
    max_chars: usize,
) -> Result<Option<&'a str>, Value> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                return Ok(None);
            }
            if value.chars().count() > max_chars {
                return Err(json!({
                    "ok": false,
                    "code": "continuity_search_params_invalid",
                    "message": format!("{key} exceeds {max_chars} characters"),
                }));
            }
            Ok(Some(value))
        }
        Some(_) => Err(json!({
            "ok": false,
            "code": "continuity_search_params_invalid",
            "message": format!("{key} must be a string when provided"),
        })),
    }
}

fn config_dir() -> std::path::PathBuf {
    crate::paths::RuntimePaths::discover()
        .map(|paths| paths.config_dir)
        .unwrap_or_else(|_| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| ".".into())
                .join(".config")
                .join("herdr-mcp")
        })
}

/// Local dispatch for the internal `artifact.*` methods. These are reached over
/// `herdr_call` and never add an MCP catalog tool. They operate on the secure,
/// bounded, short-lived web-artifact cache written by the native-host capture
/// path, and `artifact.import` re-uses `fs_mutation::write_bytes` so every
/// managed-root/read-only/dirty/busy/overwrite/symlink gate still applies.
fn artifact_call(
    config_dir: &std::path::Path,
    snapshot: &Value,
    method: &str,
    params: &Value,
) -> Value {
    match method {
        "artifact.list" => match artifact_list(config_dir) {
            Ok(artifacts) => json!({"ok": true, "artifacts": artifacts}),
            Err(error) => {
                json!({"ok": false, "code": "artifact_list_failed", "message": error})
            }
        },
        "artifact.import" => artifact_import_call(config_dir, snapshot, params),
        "artifact.info" => {
            match params
                .get("artifact_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                Some(artifact_id) => {
                    match crate::web_artifact_cache::read(config_dir, artifact_id) {
                        Ok((metadata, _)) => {
                            json!({"ok": true, "artifact": metadata.metadata_json()})
                        }
                        Err(error) => {
                            json!({"ok": false, "code": error, "artifact_id": artifact_id})
                        }
                    }
                }
                None => json!({
                    "ok": false,
                    "code": "artifact_id_required",
                    "message": "artifact.info requires a non-empty artifact_id",
                }),
            }
        }
        _ => json!({
            "ok": false,
            "code": "artifact_unknown_method",
            "method": method,
            "message": "unknown artifact local method; no request was forwarded",
        }),
    }
}

fn artifact_list(config_dir: &std::path::Path) -> Result<Vec<Value>, String> {
    crate::web_artifact_cache::list(config_dir)
}

fn artifact_import_call(config_dir: &std::path::Path, snapshot: &Value, params: &Value) -> Value {
    let Some(artifact_id) = params
        .get("artifact_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return json!({
            "ok": false,
            "code": "artifact_id_required",
            "message": "artifact.import requires a non-empty artifact_id",
        });
    };
    let Some(destination) = params
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return json!({
            "ok": false,
            "code": "artifact_path_required",
            "message": "artifact.import requires an absolute managed destination path",
        });
    };
    let overwrite = params
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let confirm_dirty = params
        .get("confirm_dirty")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let confirm_busy = params
        .get("confirm_busy")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let (metadata, raw) = match crate::web_artifact_cache::read(config_dir, artifact_id) {
        Ok(pair) => pair,
        Err(error) => {
            return json!({
                "ok": false,
                "code": error,
                "artifact_id": artifact_id,
            });
        }
    };
    let result = crate::fs_mutation::write_bytes(
        snapshot,
        destination.trim(),
        &raw,
        overwrite,
        confirm_dirty,
        confirm_busy,
    );
    if result.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        // On success, consume the one-shot cache entry so a repeated import fails
        // closed instead of silently re-writing the same bytes.
        let _ = crate::web_artifact_cache::remove(config_dir, artifact_id);
        json!({
            "ok": true,
            "artifact_id": artifact_id,
            "mime": metadata.mime,
            "bytes": metadata.bytes,
            "sha256": metadata.sha256,
            "write": result,
        })
    } else {
        json!({
            "ok": false,
            "artifact_id": artifact_id,
            "message": "artifact write was rejected by managed-root gates",
            "write": result,
        })
    }
}

fn image_tool_result(image: fs_tools::ImageData) -> Value {
    let text = serde_json::to_string(&image.meta).unwrap_or_else(|_| "{}".to_owned());
    json!({
        "content": [
            {"type": "text", "text": text},
            {"type": "image", "data": image.data, "mimeType": image.mime_type}
        ]
    })
}

fn tool_result(value: Value, is_error: bool) -> Value {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
    if is_error {
        json!({"content": [{"type": "text", "text": text}], "isError": true})
    } else {
        json!({"content": [{"type": "text", "text": text}]})
    }
}

/// Route a focused fs/git tool through the stable TCC broker when
/// `HERDR_MCP_TCC_BROKER=1` is set. Returns `None` when broker routing is not
/// enabled, so the caller falls back to direct in-process execution. When
/// routing is enabled, the broker result is returned as the tool result value
/// (or an error value on broker failure).
fn route_fs_git(op: &str, snapshot: &Value, arguments: &Value) -> Value {
    let value = match tcc_broker::route_fs_git(op, snapshot, arguments) {
        None => match op {
            "fs_read" => fs_tools::read(snapshot, arguments),
            "fs_list" => fs_tools::list(snapshot, arguments),
            "fs_grep" => fs_tools::grep(snapshot, arguments),
            "fs_edit" => fs_mutation::edit(snapshot, arguments),
            "fs_write" => fs_mutation::write(snapshot, arguments),
            "fs_patch" => fs_patch::apply(snapshot, arguments),
            "git" => git_tools::run(snapshot, arguments),
            _ => json!({"ok": false, "code": "unknown_operation", "op": op}),
        },
        Some(Ok(value)) => value,
        Some(Err(message)) => json!({
            "ok": false,
            "code": "broker_failed",
            "message": message,
        }),
    };
    crate::macos_permissions::map_fs_git_result(value)
}

fn id(request: &Value) -> Value {
    request.get("id").cloned().unwrap_or(Value::Null)
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_uses_supported_requested_protocol() {
        let result = initialize_result(&json!({
            "params": {"protocolVersion": "2025-06-18"}
        }));
        assert_eq!(result["protocolVersion"], "2025-06-18");
        assert_eq!(result["serverInfo"]["name"], "herdr-mcp");
        assert_eq!(result["_meta"]["herdr_contract_epoch"], 2);
        let instructions = result["instructions"].as_str().unwrap();
        assert!(instructions.contains("continue/resume intent"));
        assert!(instructions.contains("search durable Continuity before asking"));
        assert!(instructions.contains("never select a chain by recency or text similarity alone"));
    }

    #[test]
    fn unsupported_protocol_negotiates_to_sdk_wire() {
        let result = initialize_result(&json!({
            "params": {"protocolVersion": OPENAI_PROBE_PROTOCOL}
        }));
        assert_eq!(result["protocolVersion"], SDK_WIRE_PROTOCOL);
    }

    #[test]
    fn negotiate_protocol_version_matches_runtime_parity_probe() {
        let parity: Value =
            serde_json::from_str(include_str!("../../../contracts/runtime-parity.json")).unwrap();
        let probe = parity["openai_discover_extra_versions"][0]
            .as_str()
            .unwrap();
        assert_eq!(probe, OPENAI_PROBE_PROTOCOL);
        assert_eq!(negotiate_protocol_version(probe), SDK_WIRE_PROTOCOL);
        assert_eq!(negotiate_protocol_version("2025-06-18"), "2025-06-18");
    }

    #[test]
    fn discover_advertises_current_native_identity_and_versions() {
        let result = discover_result();
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["supportedVersions"][0], SDK_WIRE_PROTOCOL);
        assert_eq!(result["_meta"]["herdr_contract_epoch"], 2);
    }

    #[test]
    fn fs_git_broker_timeout_maps_to_macos_tcc_without_masking_other_errors() {
        let timeout = route_fs_git_map_for_test(json!({
            "ok": false,
            "code": "broker_failed",
            "message": "broker request timed out"
        }));
        assert_eq!(timeout["code"], "macos_tcc_access_blocked");
        let outside = route_fs_git_map_for_test(json!({
            "ok": false,
            "reason": "outside_managed_roots",
            "message": "not in a project"
        }));
        assert_eq!(outside["reason"], "outside_managed_roots");
        assert_ne!(outside["code"], "macos_tcc_access_blocked");
    }

    fn route_fs_git_map_for_test(value: Value) -> Value {
        crate::macos_permissions::map_fs_git_result(value)
    }

    #[test]
    fn explicit_tool_errors_preserve_mcp_is_error() {
        let result = tool_result(
            json!({"ok": false, "code": "native_tool_pending", "tool": "herdr_exec"}),
            true,
        );
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("native_tool_pending")
        );
    }
    #[test]
    fn continuity_search_requires_confirmation_without_stable_identity() {
        use crate::state_store::ContinuityTurnInput;
        use std::sync::{Arc, Mutex};

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        {
            let mut guard = store.lock().unwrap();
            for (
                continuity_id,
                conversation_id,
                workspace_id,
                project_id,
                title,
                message_id,
                text,
                observed_at,
            ) in [
                (
                    "hc:alpha",
                    "conv-a",
                    "w19",
                    "project-a",
                    "Alpha release",
                    "msg-a",
                    "continue v0.4.2 release work",
                    100,
                ),
                (
                    "hc:beta",
                    "conv-b",
                    "w20",
                    "project-b",
                    "Beta provider",
                    "msg-b",
                    "continue provider work",
                    200,
                ),
            ] {
                guard
                    .append_continuity_turn(ContinuityTurnInput {
                        continuity_id,
                        conversation_id,
                        workspace_id: Some(workspace_id),
                        project_id: Some(project_id),
                        title: Some(title),
                        message_id,
                        role: "user",
                        text,
                        fingerprint: None,
                        observed_at,
                    })
                    .unwrap();
            }
        }

        let bare = continuity_call(&store, "continuity.search", &json!({}));
        assert_eq!(bare["ok"], true);
        assert_eq!(bare["resolution"], "confirmation_required");
        assert_eq!(bare["auto_resume_safe"], false);
        assert_eq!(bare["confirmation_required"], true);
        assert_eq!(bare["candidates"].as_array().unwrap().len(), 2);
        assert!(
            bare["instruction"]
                .as_str()
                .unwrap()
                .contains("Do not choose by recency")
        );

        let exact = continuity_call(&store, "continuity.search", &json!({"workspace_id": "w19"}));
        assert_eq!(exact["resolution"], "unique_exact");
        assert_eq!(exact["auto_resume_safe"], true);
        assert_eq!(exact["confirmation_required"], false);
        assert_eq!(exact["candidates"][0]["continuity_id"], "hc:alpha");
        assert_eq!(exact["candidates"][0]["match_reasons"][0], "workspace_id");

        let text_only = continuity_call(&store, "continuity.search", &json!({"query": "v0.4.2"}));
        assert_eq!(text_only["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(text_only["resolution"], "confirmation_required");
        assert_eq!(text_only["auto_resume_safe"], false);

        {
            let mut guard = store.lock().unwrap();
            guard
                .append_continuity_turn(ContinuityTurnInput {
                    continuity_id: "hc:gamma",
                    conversation_id: "conv-c",
                    workspace_id: Some("w19"),
                    project_id: Some("project-a"),
                    title: Some("Gamma release"),
                    message_id: "msg-c",
                    role: "user",
                    text: "another release chain",
                    fingerprint: None,
                    observed_at: 300,
                })
                .unwrap();
        }
        let ambiguous =
            continuity_call(&store, "continuity.search", &json!({"workspace_id": "w19"}));
        assert_eq!(ambiguous["resolution"], "confirmation_required");
        assert_eq!(ambiguous["auto_resume_safe"], false);
        assert_eq!(ambiguous["candidates"].as_array().unwrap().len(), 2);

        let identity_plus_text = continuity_call(
            &store,
            "continuity.search",
            &json!({"workspace_id": "w19", "query": "v0.4.2"}),
        );
        assert_eq!(
            identity_plus_text["candidates"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            identity_plus_text["candidates"][0]["continuity_id"],
            "hc:alpha"
        );
        assert_eq!(identity_plus_text["resolution"], "confirmation_required");
        assert_eq!(identity_plus_text["auto_resume_safe"], false);

        let invalid = continuity_call(
            &store,
            "continuity.search",
            &json!({"workspace_id": 19, "limit": 99}),
        );
        assert_eq!(invalid["ok"], false);
        assert_eq!(invalid["code"], "continuity_search_params_invalid");
    }

    #[test]
    fn work_memory_private_methods_share_state_store_and_provider_qualify_messages() {
        use std::sync::{Arc, Mutex};

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let mut message_ids = Vec::new();
        for (provider, account_ref) in [("chatgpt", "account-a"), ("gemini", "account-b")] {
            let bound = work_memory_call(
                &store,
                "work_memory.bind",
                &json!({
                    "continuity_id": "wm:mcp",
                    "project_ref": "project:herdr-mcp",
                    "repo_id": "github.com/whshang/herdr-mcp",
                    "work_chain_id": "wc_cccccccccccccccccccccccccccccccc",
                    "provider": provider,
                    "account_ref": account_ref,
                    "space_ref": "project-space",
                    "session_ref": "same-session",
                    "bound_at": 100,
                }),
            );
            assert_eq!(bound["ok"], true);

            let appended = work_memory_call(
                &store,
                "work_memory.append_turn",
                &json!({
                    "continuity_id": "wm:mcp",
                    "provider": provider,
                    "account_ref": account_ref,
                    "space_ref": "project-space",
                    "session_ref": "same-session",
                    "provider_message_ref": "same-message",
                    "role": if provider == "chatgpt" { "user" } else { "assistant" },
                    "text": format!("{provider} work memory turn\n\n```text\nline\t2\n```"),
                    "observed_at": if provider == "chatgpt" { 110 } else { 111 },
                }),
            );
            assert_eq!(appended["ok"], true);
            assert_eq!(appended["inserted"], true);
            message_ids.push(appended["message_id"].as_str().unwrap().to_owned());
        }
        assert_ne!(message_ids[0], message_ids[1]);

        let evidence = work_memory_call(
            &store,
            "work_memory.append_evidence",
            &json!({
                "continuity_id": "wm:mcp",
                "kind": "result",
                "content": "provider-neutral durable result\n\n```text\nexit\t0\n```",
                "provider": "gemini",
                "account_ref": "account-b",
                "space_ref": "project-space",
                "session_ref": "same-session",
                "portable_source": {
                    "repo_id": "github.com/whshang/herdr-mcp",
                    "commit_sha": "0123456789abcdef0123456789abcdef01234567",
                    "repo_relative_path": "crates/herdr-mcp/src/mcp.rs",
                    "line_start": 1,
                    "line_end": 10
                },
                "created_at": 112,
            }),
        );
        assert_eq!(evidence["ok"], true);
        assert!(evidence["evidence_id"].as_str().unwrap().starts_with("ev_"));
        assert_eq!(
            evidence["portable_evidence_ref"]["repo_relative_path"],
            "crates/herdr-mcp/src/mcp.rs"
        );

        let checkpoint = work_memory_call(
            &store,
            "work_memory.checkpoint.put",
            &json!({
                "continuity_id": "wm:mcp",
                "expected_checkpoint_revision": 0,
                "summary": "MCP checkpoint\nready for handoff",
                "checkpoint_json": "{\n\t\"goal\": \"alpha2\"\n}",
                "through_message_id": message_ids[1],
                "through_evidence_id": evidence["evidence_id"],
                "created_at": 120,
            }),
        );
        assert_eq!(checkpoint["ok"], true);
        assert_eq!(checkpoint["checkpoint"]["revision"], 1);
        assert_eq!(checkpoint["checkpoint"]["verified"], true);

        let resumed = work_memory_call(
            &store,
            "work_memory.resume",
            &json!({
                "project_ref": "project:herdr-mcp",
                "repo_id": "github.com/whshang/herdr-mcp",
                "work_chain_id": "wc_cccccccccccccccccccccccccccccccc",
                "max_turns": 8
            }),
        );
        assert_eq!(resumed["ok"], true);
        assert_eq!(
            resumed["work_chain_id"],
            "wc_cccccccccccccccccccccccccccccccc"
        );
        assert_eq!(resumed["turns"].as_array().unwrap().len(), 2);
        assert_eq!(resumed["turns"][0]["provider"], "chatgpt");
        assert_eq!(resumed["turns"][1]["provider"], "gemini");

        let searched = work_memory_call(
            &store,
            "work_memory.search",
            &json!({
                "project_ref": "project:herdr-mcp",
                "repo_id": "github.com/whshang/herdr-mcp",
                "work_chain_id": "wc_cccccccccccccccccccccccccccccccc",
                "query": "durable result"
            }),
        );
        assert_eq!(searched["ok"], true);
        assert_eq!(searched["hits"].as_array().unwrap().len(), 1);
        assert_eq!(searched["hits"][0]["source_kind"], "evidence");
    }

    #[test]
    fn browser_registry_private_methods_are_read_only_stable_ref_queries() {
        use crate::state_store::{
            BrowserEndpointRegistrationInput, BrowserProviderObservationInput,
            BrowserResourceObservationInput,
        };
        use std::sync::{Arc, Mutex};

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (endpoint_ref, account_ref) = {
            let mut guard = store.lock().unwrap();
            let endpoint = guard
                .register_browser_endpoint(BrowserEndpointRegistrationInput {
                    device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    profile_seed: "private-browser-profile-seed-1234",
                    browser_family: "chrome",
                    extension_version: "0.1.90",
                    observed_at: 10,
                })
                .unwrap();
            guard
                .observe_browser_provider(BrowserProviderObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    adapter_protocol_version: 1,
                    observation_generation: 7,
                    capabilities_json: r#"{"operations":["identity.inspect"]}"#,
                    observed_at: 11,
                })
                .unwrap();
            let account = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "account",
                    parent_ref: None,
                    native_identity: "native-account-hidden",
                    display_label: Some("Work"),
                    observation_generation: 7,
                    observed_at: 12,
                })
                .unwrap();
            (endpoint.endpoint_ref, account.resource_ref)
        };

        let listed = browser_registry_call(
            &store,
            "herdr_mcp.browser_endpoint.list",
            &json!({"limit": 4}),
        );
        assert_eq!(listed["ok"], true);
        assert_eq!(listed["endpoints"].as_array().unwrap().len(), 1);
        assert_eq!(listed["endpoints"][0]["endpoint_ref"], endpoint_ref);
        assert_eq!(listed["endpoints"][0]["consent"]["webchat_control"], false);

        let inspected = browser_registry_call(
            &store,
            "herdr_mcp.browser_endpoint.inspect",
            &json!({"endpoint_ref": endpoint_ref}),
        );
        assert_eq!(inspected["ok"], true);
        assert_eq!(inspected["provider_states"][0]["provider"], "chatgpt");
        assert_eq!(inspected["provider_states"][0]["observation_generation"], 7);

        let resolved = browser_registry_call(
            &store,
            "herdr_mcp.browser_resource.resolve",
            &json!({
                "endpoint_ref": endpoint_ref,
                "provider": "chatgpt",
                "kind": "account",
                "display_label": "Work",
                "expected_observation_generation": 7
            }),
        );
        assert_eq!(resolved["ok"], true);
        assert_eq!(resolved["resource"]["resource_ref"], account_ref);
        assert_eq!(resolved["resource"]["display_label"], "Work");
        assert_eq!(resolved["consent"]["webchat_control"], false);
        assert_eq!(resolved["consent"]["revision"], 0);
        assert_eq!(resolved["actuation_available"], false);
        assert!(!resolved.to_string().contains("native-account-hidden"));

        let inspected_res = browser_registry_call(
            &store,
            "herdr_mcp.browser_resource.inspect",
            &json!({"resource_ref": account_ref}),
        );
        assert_eq!(inspected_res["ok"], true);
        assert_eq!(inspected_res["resource"]["resource_ref"], account_ref);
        assert_eq!(inspected_res["consent"]["webchat_control"], false);
        assert_eq!(inspected_res["actuation_available"], false);

        let listed_res = browser_registry_call(
            &store,
            "herdr_mcp.browser_resource.list",
            &json!({"endpoint_ref": endpoint_ref}),
        );
        assert_eq!(listed_res["ok"], true);
        assert_eq!(listed_res["actuation_available"], false);

        let injected = browser_registry_call(
            &store,
            "herdr_mcp.browser_resource.resolve",
            &json!({
                "endpoint_ref": endpoint_ref,
                "provider": "chatgpt",
                "kind": "account",
                "native_identity": "attacker-controlled",
            }),
        );
        assert_eq!(injected["ok"], false);
        assert_eq!(injected["code"], "browser_registry_params_invalid");

        let device_override = browser_registry_call(
            &store,
            "herdr_mcp.browser_endpoint.list",
            &json!({"device_id": "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"}),
        );
        assert_eq!(device_override["ok"], false);
        assert_eq!(device_override["code"], "browser_registry_params_invalid");

        let remote_consent = browser_registry_call(
            &store,
            "herdr_mcp.browser_endpoint.consent",
            &json!({"endpoint_ref": endpoint_ref, "webchat_control": true}),
        );
        assert_eq!(remote_consent["ok"], false);
        assert_eq!(remote_consent["code"], "unknown_local_method");

        let identity = contract::identity().unwrap();
        assert_eq!(identity.epoch, 2);
        assert_eq!(identity.tool_count, 18);
    }

    #[test]
    fn browser_typed_operations_route_with_frozen_provider_neutral_inputs() {
        use std::sync::{Arc, Mutex};

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let endpoint_ref = format!("bep_{}", "a".repeat(64));
        let account_ref = format!("br_{}", "b".repeat(64));
        let space_ref = format!("br_{}", "c".repeat(64));
        let session_ref = format!("br_{}", "d".repeat(64));
        let dispatch_id = format!("bd_{}", "e".repeat(64));
        let cases = vec![
            (
                "herdr_mcp.browser_space.create",
                json!({
                    "endpoint_ref": endpoint_ref,
                    "provider": "chatgpt",
                    "account_ref": account_ref,
                    "display_label": "Project",
                    "expected_generation": 7,
                    "idempotency_key": "space-create-1"
                }),
            ),
            (
                "herdr_mcp.browser_space.open",
                json!({
                    "space_ref": space_ref,
                    "expected_generation": 7,
                    "idempotency_key": "space-open-1"
                }),
            ),
            (
                "herdr_mcp.browser_space.inspect",
                json!({"space_ref": space_ref}),
            ),
            (
                "herdr_mcp.browser_session.create",
                json!({
                    "endpoint_ref": endpoint_ref,
                    "provider": "chatgpt",
                    "account_ref": account_ref,
                    "space_ref": space_ref,
                    "display_label": "Conversation",
                    "expected_generation": 7,
                    "idempotency_key": "session-create-1"
                }),
            ),
            (
                "herdr_mcp.browser_session.open",
                json!({
                    "session_ref": session_ref,
                    "expected_generation": 7,
                    "idempotency_key": "session-open-1"
                }),
            ),
            (
                "herdr_mcp.browser_session.inspect",
                json!({"session_ref": session_ref}),
            ),
            (
                "herdr_mcp.browser_message.append",
                json!({
                    "session_ref": session_ref,
                    "message": "hello",
                    "expected_generation": 7,
                    "idempotency_key": "message-append-1"
                }),
            ),
            (
                "herdr_mcp.browser_composer.set_reasoning",
                json!({
                    "session_ref": session_ref,
                    "reasoning_effort": "balanced",
                    "expected_generation": 7,
                    "idempotency_key": "reasoning-1"
                }),
            ),
            (
                "herdr_mcp.browser_composer.set_apps",
                json!({
                    "session_ref": session_ref,
                    "required_apps": ["herdr"],
                    "expected_generation": 7,
                    "idempotency_key": "apps-1"
                }),
            ),
            (
                "herdr_mcp.browser_dispatch.submit",
                json!({
                    "session_ref": session_ref,
                    "message": "ship it",
                    "reasoning_effort": "thorough",
                    "required_apps": ["herdr"],
                    "expected_generation": 7,
                    "idempotency_key": "dispatch-1",
                    "work_chain_id": "wc_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "lane_id": "lane-alpha"
                }),
            ),
            (
                "herdr_mcp.browser_dispatch.status",
                json!({"dispatch_id": dispatch_id}),
            ),
            (
                "herdr_mcp.browser_dispatch.stop",
                json!({
                    "dispatch_id": dispatch_id,
                    "expected_generation": 7,
                    "idempotency_key": "dispatch-stop-1"
                }),
            ),
        ];

        for (method, params) in cases {
            let result = browser_operation_call(&store, method, &params);
            assert_ne!(result["code"], "unknown_local_method", "{method}");
            assert_ne!(
                result["code"], "browser_operation_params_invalid",
                "{method}"
            );
            assert_ne!(result["code"], "browser_forbidden_input", "{method}");
        }
    }

    #[test]
    fn browser_typed_operations_reject_forbidden_and_provider_native_inputs() {
        use std::sync::{Arc, Mutex};

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let session_ref = format!("br_{}", "d".repeat(64));
        for field in [
            "selector",
            "xpath",
            "javascript",
            "cdp",
            "macro",
            "url",
            "shell",
            "bearer_token",
            "refresh_token",
            "credentials",
            "native_identity",
        ] {
            let mut params = json!({
                "session_ref": session_ref,
                "message": "safe message",
                "expected_generation": 7,
                "idempotency_key": "dispatch-forbidden-1"
            });
            params
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), json!("attacker-controlled"));
            let result =
                browser_operation_call(&store, "herdr_mcp.browser_dispatch.submit", &params);
            assert_eq!(result["code"], "browser_forbidden_input", "{field}");
            assert_eq!(result["field"], field);
        }

        let native_reasoning = browser_operation_call(
            &store,
            "herdr_mcp.browser_composer.set_reasoning",
            &json!({
                "session_ref": session_ref,
                "reasoning_effort": "medium",
                "expected_generation": 7,
                "idempotency_key": "reasoning-native-1"
            }),
        );
        assert_eq!(native_reasoning["code"], "browser_reasoning_effort_invalid");

        let arbitrary_json = browser_operation_call(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &json!({
                "session_ref": session_ref,
                "message": "safe message",
                "expected_generation": 7,
                "idempotency_key": "dispatch-options-1",
                "options": {"anything": true}
            }),
        );
        assert_eq!(arbitrary_json["code"], "browser_operation_params_invalid");
    }

    #[test]
    fn browser_postconditions_never_promote_send_only_or_stale_evidence() {
        let params = json!({
            "session_ref": format!("br_{}", "a".repeat(64)),
            "message": "hello",
            "expected_generation": 7,
            "idempotency_key": "postcondition-test"
        });
        let mut evidence = BrowserPostconditionEvidence::resource_unavailable(7);
        evidence.resource_available = true;
        evidence.command_accepted = true;
        assert_eq!(
            browser_delivery_state_from_postcondition(
                BrowserOperation::DispatchSubmit,
                &params,
                7,
                &evidence,
            )
            .unwrap(),
            BrowserDeliveryState::Uncertain
        );

        evidence.accepted_message_observed = true;
        evidence.generation_owner = Some(7);
        evidence.generation_status_observed = true;
        assert_eq!(
            browser_delivery_state_from_postcondition(
                BrowserOperation::DispatchSubmit,
                &params,
                7,
                &evidence,
            )
            .unwrap(),
            BrowserDeliveryState::Applied
        );

        evidence.observed_generation = 8;
        assert_eq!(
            browser_delivery_state_from_postcondition(
                BrowserOperation::DispatchSubmit,
                &params,
                7,
                &evidence,
            )
            .unwrap_err(),
            "stale_capability_generation"
        );

        let mut stop_evidence = BrowserPostconditionEvidence::resource_unavailable(7);
        stop_evidence.resource_available = true;
        stop_evidence.command_accepted = true;
        stop_evidence.generation_stopped = true;
        assert_eq!(
            browser_delivery_state_from_postcondition(
                BrowserOperation::DispatchStop,
                &json!({
                    "dispatch_id": format!("bd_{}", "b".repeat(64)),
                    "expected_generation": 7,
                    "idempotency_key": "stop-postcondition-test"
                }),
                7,
                &stop_evidence,
            )
            .unwrap(),
            BrowserDeliveryState::Stopped
        );
    }

    #[test]
    fn browser_dispatch_gateway_persists_terminal_attempt_and_never_replays_actuation() {
        use crate::state_store::{
            BrowserEndpointConsentInput, BrowserEndpointRegistrationInput,
            BrowserProviderObservationInput, BrowserResourceObservationInput,
        };
        use std::sync::{Arc, Mutex};

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let session_ref = {
            let mut guard = store.lock().unwrap();
            let endpoint = guard
                .register_browser_endpoint(BrowserEndpointRegistrationInput {
                    device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    profile_seed: "alpha4-delivery-profile-seed",
                    browser_family: "chrome",
                    extension_version: "0.1.90",
                    observed_at: 10,
                })
                .unwrap();
            guard
                .observe_browser_provider(BrowserProviderObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    adapter_protocol_version: 1,
                    observation_generation: 7,
                    capabilities_json: r#"{"operations":["composer.submit"]}"#,
                    observed_at: 11,
                })
                .unwrap();
            let account = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "account",
                    parent_ref: None,
                    native_identity: "delivery-account-hidden",
                    display_label: None,
                    observation_generation: 7,
                    observed_at: 12,
                })
                .unwrap();
            let session = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "session",
                    parent_ref: Some(&account.resource_ref),
                    native_identity: "delivery-session-hidden",
                    display_label: None,
                    observation_generation: 7,
                    observed_at: 13,
                })
                .unwrap();
            guard
                .set_browser_endpoint_consent(BrowserEndpointConsentInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    expected_revision: 0,
                    webchat_control_allowed: true,
                    tool_bridge_allowed: false,
                    tool_bridge_mutation_allowed: false,
                    observed_at: 14,
                })
                .unwrap();
            session.resource_ref
        };
        let params = json!({
            "session_ref": session_ref,
            "message": "dispatch once",
            "expected_generation": 7,
            "idempotency_key": "delivery-idempotency-key"
        });

        let first = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &params,
            true,
            None,
        );
        assert_eq!(first["code"], "resource_unavailable");
        assert_eq!(first["replayed"], false);
        assert_eq!(first["dispatch"]["delivery_state"], "resource_unavailable");
        let dispatch_id = first["dispatch"]["dispatch_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let replay = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &params,
            true,
            None,
        );
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["dispatch"]["dispatch_id"], dispatch_id);
        assert_eq!(replay["dispatch"]["delivery_state"], "resource_unavailable");

        let status = browser_operation_call(
            &store,
            "herdr_mcp.browser_dispatch.status",
            &json!({"dispatch_id": dispatch_id}),
        );
        assert_eq!(status["ok"], true);
        assert_eq!(status["dispatch"]["delivery_state"], "resource_unavailable");
        assert!(!status.to_string().contains("delivery-session-hidden"));

        struct AppliedActuator;
        impl BrowserActuator for AppliedActuator {
            fn actuate(
                &self,
                _operation: &str,
                _params: &Value,
                expected_generation: i64,
            ) -> Result<BrowserPostconditionEvidence, String> {
                Ok(BrowserPostconditionEvidence {
                    observed_generation: expected_generation,
                    command_accepted: true,
                    browser_online: true,
                    resource_available: true,
                    rejected: false,
                    stable_resource_ref_observed: true,
                    lifecycle_observed: true,
                    canonical_url_observed: true,
                    accepted_message_observed: true,
                    message_baseline_advanced: false,
                    reasoning_effort_readback: None,
                    required_apps_readback: Vec::new(),
                    generation_owner: Some(expected_generation),
                    generation_status_observed: true,
                    generation_stopped: false,
                })
            }
        }

        store
            .lock()
            .unwrap()
            .bind_work_memory(WorkMemoryBindingInput {
                continuity_id: "wm:browser-dispatch",
                project_ref: "project:herdr-mcp",
                repo_id: "github.com/whshang/herdr-mcp",
                work_chain_id: "wc_dddddddddddddddddddddddddddddddd",
                provider: "chatgpt",
                account_ref: None,
                space_ref: None,
                session_ref: &session_ref,
                bound_at: 20,
            })
            .unwrap();
        let applied_params = json!({
            "session_ref": session_ref,
            "message": "dispatch and persist evidence",
            "expected_generation": 7,
            "idempotency_key": "delivery-applied-idempotency-key",
            "work_chain_id": "wc_dddddddddddddddddddddddddddddddd",
            "lane_id": "lane-browser"
        });
        let applied = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &applied_params,
            true,
            Some(&AppliedActuator),
        );
        assert_eq!(applied["ok"], true);
        assert!(applied["code"].is_null());
        assert_eq!(applied["dispatch"]["delivery_state"], "applied");
        assert_eq!(applied["work_memory_writeback"]["ok"], true);
        assert!(applied["work_memory_writeback"]["portable_evidence_ref"].is_null());
        let applied_dispatch_id = applied["dispatch"]["dispatch_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let evidence_search = work_memory_call(
            &store,
            "work_memory.search",
            &json!({
                "project_ref": "project:herdr-mcp",
                "repo_id": "github.com/whshang/herdr-mcp",
                "work_chain_id": "wc_dddddddddddddddddddddddddddddddd",
                "query": applied_dispatch_id,
                "limit": 10
            }),
        );
        assert_eq!(evidence_search["ok"], true);
        assert_eq!(evidence_search["hits"].as_array().unwrap().len(), 1);
        assert_eq!(evidence_search["hits"][0]["source_kind"], "evidence");
        assert!(
            !evidence_search
                .to_string()
                .contains("dispatch and persist evidence")
        );

        let applied_replay = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &applied_params,
            true,
            Some(&AppliedActuator),
        );
        assert_eq!(applied_replay["ok"], true);
        assert_eq!(applied_replay["replayed"], true);
        assert_eq!(
            applied_replay["work_memory_writeback"]["evidence_id"],
            applied["work_memory_writeback"]["evidence_id"]
        );
        let evidence_search = work_memory_call(
            &store,
            "work_memory.search",
            &json!({
                "project_ref": "project:herdr-mcp",
                "repo_id": "github.com/whshang/herdr-mcp",
                "work_chain_id": "wc_dddddddddddddddddddddddddddddddd",
                "query": applied_dispatch_id,
                "limit": 10
            }),
        );
        assert_eq!(evidence_search["hits"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn browser_actuation_intersection_fails_closed_with_stable_reasons() {
        use crate::state_store::{
            BrowserEndpointConsentInput, BrowserEndpointRegistrationInput,
            BrowserProviderObservationInput, BrowserResourceObservationInput,
            BrowserResourceRecord,
        };
        use std::sync::{Arc, Mutex};

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (endpoint_ref, account_ref, session_ref) = {
            let mut guard = store.lock().unwrap();
            let endpoint = guard
                .register_browser_endpoint(BrowserEndpointRegistrationInput {
                    device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    profile_seed: "alpha4-actuation-profile-seed",
                    browser_family: "chrome",
                    extension_version: "0.1.90",
                    observed_at: 10,
                })
                .unwrap();
            guard
                .observe_browser_provider(BrowserProviderObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    adapter_protocol_version: 1,
                    observation_generation: 7,
                    capabilities_json: r#"{"operations":["identity.inspect","session.inspect","composer.submit"]}"#,
                    observed_at: 11,
                })
                .unwrap();
            let account = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "account",
                    parent_ref: None,
                    native_identity: "native-account-hidden",
                    display_label: Some("Work"),
                    observation_generation: 7,
                    observed_at: 12,
                })
                .unwrap();
            let session = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "session",
                    parent_ref: Some(&account.resource_ref),
                    native_identity: "native-session-hidden",
                    display_label: Some("Conversation"),
                    observation_generation: 7,
                    observed_at: 13,
                })
                .unwrap();
            (
                endpoint.endpoint_ref,
                account.resource_ref,
                session.resource_ref,
            )
        };

        let dispatch_params = json!({
            "session_ref": session_ref,
            "message": "ship it",
            "expected_generation": 7,
            "idempotency_key": "actuation-dispatch-1"
        });

        let missing_grant = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &dispatch_params,
            false,
            None,
        );
        assert_eq!(missing_grant["code"], "caller_grant_missing");

        let remote_consent = browser_registry_call_with_grant(
            &store,
            "herdr_mcp.browser_endpoint.consent",
            &json!({"endpoint_ref": endpoint_ref, "webchat_control": true}),
            true,
        );
        assert_eq!(remote_consent["code"], "unknown_local_method");
        assert!(
            !store
                .lock()
                .unwrap()
                .browser_endpoint(&endpoint_ref)
                .unwrap()
                .unwrap()
                .webchat_control_allowed
        );

        let local_consent_off = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &dispatch_params,
            true,
            None,
        );
        assert_eq!(local_consent_off["code"], "local_consent_off");

        store
            .lock()
            .unwrap()
            .set_browser_endpoint_consent(BrowserEndpointConsentInput {
                endpoint_ref: &endpoint_ref,
                expected_revision: 0,
                webchat_control_allowed: true,
                tool_bridge_allowed: false,
                tool_bridge_mutation_allowed: false,
                observed_at: 14,
            })
            .unwrap();

        let wrong_account_grants = [BrowserCallerGrant {
            endpoint_ref: endpoint_ref.clone(),
            provider: "chatgpt".to_owned(),
            account_ref: format!("br_{}", "e".repeat(64)),
        }];
        let wrong_account = browser_operation_call_with_grants(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &dispatch_params,
            &wrong_account_grants,
            None,
        );
        assert_eq!(wrong_account["code"], "caller_grant_missing");

        let exact_grants = [BrowserCallerGrant {
            endpoint_ref: endpoint_ref.clone(),
            provider: "chatgpt".to_owned(),
            account_ref: account_ref.clone(),
        }];
        let inspect = browser_operation_call_with_grants(
            &store,
            "herdr_mcp.browser_session.inspect",
            &json!({"session_ref": session_ref}),
            &exact_grants,
            None,
        );
        assert_eq!(inspect["ok"], true);
        assert_eq!(inspect["actuation_available"], true);
        assert!(inspect["actuation_reason"].is_null());

        let all_factors_true = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &dispatch_params,
            true,
            None,
        );
        assert_eq!(all_factors_true["code"], "resource_unavailable");

        let dispatch_reasoning_not_allowed = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &json!({
                "session_ref": session_ref,
                "message": "ship with reasoning",
                "reasoning_effort": "balanced",
                "expected_generation": 7,
                "idempotency_key": "actuation-dispatch-reasoning-1"
            }),
            true,
            None,
        );
        assert_eq!(
            dispatch_reasoning_not_allowed["code"],
            "capability_not_allowed"
        );

        let dispatch_apps_not_allowed = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &json!({
                "session_ref": session_ref,
                "message": "ship with app",
                "required_apps": ["herdr"],
                "expected_generation": 7,
                "idempotency_key": "actuation-dispatch-app-1"
            }),
            true,
            None,
        );
        assert_eq!(dispatch_apps_not_allowed["code"], "capability_not_allowed");

        let capability_not_allowed = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_composer.set_reasoning",
            &json!({
                "session_ref": session_ref,
                "reasoning_effort": "balanced",
                "expected_generation": 7,
                "idempotency_key": "actuation-reasoning-1"
            }),
            true,
            None,
        );
        assert_eq!(capability_not_allowed["code"], "capability_not_allowed");

        let unknown_resource_ref = format!("br_{}", "f".repeat(64));
        let unknown_resource = BrowserResourceRecord {
            resource_ref: unknown_resource_ref.clone(),
            endpoint_ref: endpoint_ref.clone(),
            provider: "missing-provider".to_owned(),
            kind: "account".to_owned(),
            parent_ref: None,
            native_identity_sha256: "0".repeat(64),
            display_label: None,
            observation_generation: 7,
            first_observed_at: 15,
            last_observed_at: 15,
        };
        let unknown_grants = [BrowserCallerGrant {
            endpoint_ref: endpoint_ref.clone(),
            provider: "missing-provider".to_owned(),
            account_ref: unknown_resource_ref,
        }];
        let capability_unknown = browser_resource_actuation_decision(
            &store.lock().unwrap(),
            "identity.inspect",
            &unknown_resource,
            &unknown_grants,
            None,
        )
        .unwrap();
        assert_eq!(capability_unknown, (false, Some("capability_unknown")));

        store
            .lock()
            .unwrap()
            .observe_browser_provider(BrowserProviderObservationInput {
                endpoint_ref: &endpoint_ref,
                provider: "chatgpt",
                adapter_protocol_version: 1,
                observation_generation: 8,
                capabilities_json: r#"{"operations":["identity.inspect","session.inspect","composer.submit"]}"#,
                observed_at: 16,
            })
            .unwrap();
        let stale = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &dispatch_params,
            true,
            None,
        );
        assert_eq!(stale["code"], "stale_capability_generation");
    }

    #[test]
    fn runtime_parity_fixture_matches_native_protocol_constants() {
        let parity: Value =
            serde_json::from_str(include_str!("../../../contracts/runtime-parity.json")).unwrap();
        assert_eq!(parity["server_name"], "herdr-mcp");
        assert_eq!(parity["sdk_wire_protocol"], SDK_WIRE_PROTOCOL);
        assert_eq!(
            parity["contract_epoch"],
            contract::identity().unwrap().epoch
        );
        assert_eq!(parity["contract_hash"], contract::identity().unwrap().hash);
        assert_eq!(
            parity["tool_count"],
            contract::identity().unwrap().tool_count
        );
        let expected = parity["supported_versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(expected, SUPPORTED_VERSIONS);
    }

    #[test]
    fn artifact_dispatch_lists_and_fails_closed_without_leaking_secrets() {
        use crate::web_artifact_cache;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let config_dir = std::env::temp_dir().join(format!(
            "herdr-mcp-artifact-dispatch-{}-{}-{nonce:x}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        let snapshot = json!({});

        let unknown = artifact_call(&config_dir, &snapshot, "artifact.nope", &json!({}));
        assert_eq!(unknown["ok"], false);
        assert_eq!(unknown["code"], "artifact_unknown_method");

        let empty = artifact_call(&config_dir, &snapshot, "artifact.list", &json!({}));
        assert_eq!(empty["ok"], true);
        assert_eq!(empty["artifacts"].as_array().unwrap().len(), 0);

        let missing_import = artifact_call(
            &config_dir,
            &snapshot,
            "artifact.import",
            &json!({"path": "/tmp/x.png"}),
        );
        assert_eq!(missing_import["code"], "artifact_id_required");
        let missing_path = artifact_call(
            &config_dir,
            &snapshot,
            "artifact.import",
            &json!({"artifact_id": "abc"}),
        );
        assert_eq!(missing_path["code"], "artifact_path_required");
        let not_found = artifact_call(
            &config_dir,
            &snapshot,
            "artifact.import",
            &json!({"artifact_id": "abc", "path": "/tmp/x.png"}),
        );
        assert_eq!(not_found["ok"], false);
        assert_eq!(not_found["code"], "artifact_not_found");

        // A captured artifact surfaces in list with non-secret fields only.
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[1, 2, 3, 4]);
        let captured = web_artifact_cache::capture(
            &config_dir,
            "conv-dispatch-1",
            "file-dispatch-1",
            "image/png",
            &png,
            None,
        )
        .unwrap();
        let listed = artifact_call(&config_dir, &snapshot, "artifact.list", &json!({}));
        assert_eq!(listed["artifacts"].as_array().unwrap().len(), 1);
        let meta = &listed["artifacts"][0];
        for secret in [
            "bearer",
            "cookie",
            "accessToken",
            "authorization",
            "download_url",
        ] {
            assert!(meta.get(secret).is_none(), "{secret} must not be exposed");
        }
        assert_eq!(meta["artifact_id"], captured.artifact_id);

        let info = artifact_call(
            &config_dir,
            &snapshot,
            "artifact.info",
            &json!({"artifact_id": captured.artifact_id}),
        );
        assert_eq!(info["ok"], true);
        assert_eq!(info["artifact"]["sha256"], captured.sha256);
        assert!(info["artifact"].get("download_url").is_none());

        std::fs::remove_dir_all(&config_dir).unwrap();
    }

    #[test]
    fn artifact_import_uses_write_bytes_gates_and_consumes_one_shot() {
        use crate::web_artifact_cache;
        use std::process::Command;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let config_dir = std::env::temp_dir().join(format!(
            "herdr-mcp-artifact-import-{}-{}-{nonce:x}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-artifact-repo-{}-{}-{nonce:x}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(root.join("kept.txt"), "baseline\n").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "kept.txt"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=Herdr Test",
                    "-c",
                    "user.email=herdr@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "baseline",
                ])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        let snapshot = json!({
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}],
            "agents": [],
        });

        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[9, 9, 9]);
        let captured = web_artifact_cache::capture(
            &config_dir,
            "conv-import-1",
            "file-import-1",
            "image/png",
            &png,
            None,
        )
        .unwrap();
        let destination = root.join("generated.png");

        // First import writes into the managed root and consumes the cache entry.
        let first = artifact_call(
            &config_dir,
            &snapshot,
            "artifact.import",
            &json!({"artifact_id": captured.artifact_id, "path": destination}),
        );
        assert_eq!(first["ok"], true, "{}r", first);
        assert_eq!(std::fs::read(&destination).unwrap(), png);
        assert_eq!(
            first["write"]["created"], true,
            "write gates should report a fresh file"
        );

        // The cache entry was consumed: a second import fails closed.
        let second = artifact_call(
            &config_dir,
            &snapshot,
            "artifact.import",
            &json!({"artifact_id": captured.artifact_id, "path": destination}),
        );
        assert_eq!(second["ok"], false);
        assert_eq!(second["code"], "artifact_not_found");

        std::fs::remove_dir_all(&config_dir).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }
}
