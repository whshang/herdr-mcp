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
    BrowserDeliveryState, BrowserDispatchAuthorizationInput, BrowserDispatchReservation,
    BrowserDispatchReserveInput, BrowserDispatchUpdateInput, BrowserResourceResolveInput,
    BrowserSessionReservation, BrowserSessionReservationInput, BrowserSessionReservationRecord,
    ContinuitySearchInput, OperationReservation, StateStore, WorkMemoryBindingInput,
    WorkMemoryCheckpointInput, WorkMemoryEvidenceInput, WorkMemoryPortableSourceInput,
    WorkMemorySearchBoundary, WorkMemorySearchPage, WorkMemorySearchPageOptions,
    WorkMemoryTurnInput,
};
use crate::tcc_broker;
use crate::utility_exec;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SDK_WIRE_PROTOCOL: &str = "2025-11-25";
/// ChatGPT/OpenAI connector probe version; advertised on discover and negotiated
/// down to [`SDK_WIRE_PROTOCOL`] for the actual wire session.
pub const OPENAI_PROBE_PROTOCOL: &str = "2026-07-28";
pub const SERVER_INSTRUCTIONS: &str = "Herdr control plane for a WEB planner. Before the first remote call, form the next dependency-aware call plan from facts already known. A call is justified only when it obtains decision-changing evidence, executes planned work, or verifies an acceptance boundary. On continue/resume intent, search durable Continuity before asking for an ID; when a ChatGPT conversation URL is supplied, call continuity.resume directly with conversation_url and use continuity.search only for bounded ambiguity discovery; never select a chain by recency or text similarity alone. For ChatGPT self-handoff, prefer browser_session.create with source_url, message, and idempotency_key; do not enumerate browser endpoints, accounts, projects, generations, or device ids first. When live state matters, establish one baseline with herdr_inspect, then reuse IDs/paths, herdr_since cursors, fingerprints, and exec offsets. Load herdr_skill only when the task needs its detailed operating policy or before Agent control; request include_native_reference=false unless native Herdr CLI semantics are specifically needed. Group independent reads into one wave. For deterministic steps whose arguments are already known and share one safety boundary, use one bounded herdr_exec or patch and perform local checks inside it; do not use remote tool results as thinking checkpoints between already-planned steps. Re-plan only when a result changes the next arguments or safety decision, requires user action, or creates delivery uncertainty. Prefer private summary methods such as cleanup.preview over reconstructing the same view. Discover an unknown native method once with herdr_methods, then reuse its schema. Never blind-retry uncertain mutations.";

const SUPPORTED_VERSIONS: [&str; 5] = [
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];
const BROWSER_ADAPTER_PROTOCOL_VERSION: i64 = 1;
const BROWSER_ACCOUNT_MUTATION_RETRY_AFTER_MS: i64 = 250;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCallerGrant {
    pub endpoint_ref: String,
    pub provider: String,
    pub account_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCallerAuthorization {
    pub principal_ref: String,
    pub connector_id: String,
    pub grant_generation: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageAssistCallerGrant {
    pub endpoint_ref: String,
}

#[derive(Default)]
pub struct BrowserMutationAdmission {
    active_accounts: std::sync::Mutex<std::collections::HashSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserMutationScope {
    endpoint_ref: String,
    provider: String,
    account_ref: String,
}

struct BrowserMutationPermit<'a> {
    admission: &'a BrowserMutationAdmission,
    key: String,
}

impl BrowserMutationAdmission {
    fn reserve<'a>(
        &'a self,
        scope: &BrowserMutationScope,
    ) -> Result<Option<BrowserMutationPermit<'a>>, String> {
        let key = format!(
            "{}\u{0}{}\u{0}{}",
            scope.endpoint_ref, scope.provider, scope.account_ref
        );
        let mut active = self
            .active_accounts
            .lock()
            .map_err(|_| "browser_mutation_admission_unavailable".to_owned())?;
        if !active.insert(key.clone()) {
            return Ok(None);
        }
        Ok(Some(BrowserMutationPermit {
            admission: self,
            key,
        }))
    }
}

impl Drop for BrowserMutationPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.admission.active_accounts.lock() {
            active.remove(&self.key);
        }
    }
}

pub struct RuntimeContext<'a> {
    pub client: &'a HerdrClient,
    pub cache: &'a EventCache,
    pub exec: &'a ExecRegistry,
    pub prompt: &'a PromptRegistry,
    pub skill: &'a SkillService,
    pub state_store: &'a std::sync::Arc<std::sync::Mutex<StateStore>>,
    pub caller_webchat_control_grants: &'a [BrowserCallerGrant],
    pub caller_webchat_authorization: Option<&'a BrowserCallerAuthorization>,
    pub caller_page_assist_grants: &'a [PageAssistCallerGrant],
    pub browser_actuator: Option<&'a dyn BrowserActuator>,
    pub browser_mutation_gate: Option<&'a std::sync::RwLock<()>>,
    pub browser_mutation_admission: Option<&'a BrowserMutationAdmission>,
}

pub trait BrowserActuator: Send + Sync {
    fn actuate(
        &self,
        operation: &str,
        params: &Value,
        expected_generation: i64,
        dispatch_id: Option<&str>,
    ) -> Result<BrowserPostconditionEvidence, String>;

    fn reconcile_dispatch(
        &self,
        _dispatch_id: &str,
        _expected_generation: i64,
    ) -> Result<Option<BrowserPostconditionEvidence>, String> {
        Ok(None)
    }
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
            let transition = context
                .state_store
                .lock()
                .map_err(|_| "state store lock is poisoned".to_owned())
                .and_then(|store| store.latest_generation_transition());
            native_tools::since(context.cache, cursor, workspace, transition)
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
            } else if method == "herdr_mcp.page_assist" {
                page_assist_call(
                    &params,
                    context.caller_page_assist_grants,
                    context.browser_actuator,
                )
            } else if BrowserOperation::parse(method).is_some() {
                browser_operation_call_with_controls(
                    context.state_store,
                    method,
                    &params,
                    context.caller_webchat_control_grants,
                    context.browser_actuator,
                    BrowserOperationControls {
                        mutation_gate: context.browser_mutation_gate,
                        mutation_admission: context.browser_mutation_admission,
                        caller_authorization: context.caller_webchat_authorization,
                    },
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
        "herdr_exec_start" => exec_tools::start(
            context.client,
            &context.cache.snapshot(),
            context.exec,
            &arguments,
        ),
        "herdr_exec_read" => exec_tools::read(context.exec, &arguments),
        "herdr_exec_kill" => exec_tools::kill(context.exec, &arguments),
        "herdr_exec" => utility_exec::run_durable(
            context.client,
            &context.cache.snapshot(),
            context.exec,
            &arguments,
        ),
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

fn chatgpt_conversation_url_ref(raw: &str) -> Option<(String, Option<String>)> {
    let parsed = url::Url::parse(raw.trim()).ok()?;
    if parsed.scheme() != "https"
        || !matches!(
            parsed.host_str()?.to_ascii_lowercase().as_str(),
            "chatgpt.com" | "www.chatgpt.com"
        )
    {
        return None;
    }
    let segments = parsed
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    match segments.as_slice() {
        ["c", conversation_id] => Some(((*conversation_id).to_owned(), None)),
        ["g", project_segment, "c", conversation_id] if project_segment.starts_with("g-p-") => {
            let resource_id = project_segment.strip_prefix("g-p-")?.split('-').next()?;
            let project_id = if resource_id.len() == 32
                && resource_id.chars().all(|ch| ch.is_ascii_hexdigit())
            {
                format!("g-p-{resource_id}")
            } else {
                (*project_segment).to_owned()
            };
            Some(((*conversation_id).to_owned(), Some(project_id)))
        }
        _ => None,
    }
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
            let Some(object) = params.as_object() else {
                return json!({"ok": false, "code": "continuity_resume_params_invalid"});
            };
            const ALLOWED: &[&str] = &["continuity_id", "conversation_url"];
            if let Some(key) = object.keys().find(|key| !ALLOWED.contains(&key.as_str())) {
                return json!({
                    "ok": false,
                    "code": "continuity_resume_params_invalid",
                    "message": format!("unknown continuity.resume param: {key}"),
                });
            }
            let explicit_id = match continuity_search_string(params, "continuity_id", 160) {
                Ok(value) => value,
                Err(_) => return json!({"ok": false, "code": "continuity_resume_params_invalid"}),
            };
            let conversation_url = match continuity_search_string(params, "conversation_url", 2048)
            {
                Ok(value) => value,
                Err(_) => return json!({"ok": false, "code": "continuity_resume_params_invalid"}),
            };
            if explicit_id.is_some() && conversation_url.is_some() {
                return json!({"ok": false, "code": "continuity_resume_params_invalid", "message": "pass continuity_id or conversation_url, not both"});
            }
            let continuity_id = if let Some(value) = explicit_id {
                value.to_owned()
            } else if let Some(raw_url) = conversation_url {
                let Some((conversation_id, project_id)) = chatgpt_conversation_url_ref(raw_url)
                else {
                    return json!({"ok": false, "code": "continuity_resume_params_invalid", "message": "conversation_url must be a ChatGPT conversation URL"});
                };
                match store.continuity_for_conversation(&conversation_id) {
                    Ok(Some(value)) => value,
                    Ok(None) => {
                        let Some(project_id) = project_id else {
                            return json!({"ok": false, "code": "continuity_not_found", "conversation_url": raw_url});
                        };
                        let records = match store.continuity_search(ContinuitySearchInput {
                            project_id: Some(&project_id),
                            workspace_id: None,
                            conversation_id: None,
                            query: None,
                            limit: 2,
                        }) {
                            Ok(records) => records,
                            Err(error) => {
                                return json!({"ok": false, "code": "continuity_read_failed", "message": error});
                            }
                        };
                        match records.as_slice() {
                            [record] => record.continuity_id.clone(),
                            [] => {
                                return json!({"ok": false, "code": "continuity_not_found", "conversation_url": raw_url});
                            }
                            _ => {
                                return json!({"ok": false, "code": "continuity_ambiguous", "conversation_url": raw_url});
                            }
                        }
                    }
                    Err(error) if error == "continuity_binding_ambiguous" => {
                        return json!({"ok": false, "code": "continuity_ambiguous", "conversation_url": raw_url});
                    }
                    Err(error) => {
                        return json!({"ok": false, "code": "continuity_read_failed", "message": error});
                    }
                }
            } else {
                return json!({"ok": false, "code": "continuity_id_or_url_required"});
            };
            match store.continuity_resume(&continuity_id, 32) {
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
                "conversation_url",
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
            let explicit_project_id = match continuity_search_string(params, "project_id", 256) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let workspace_id = match continuity_search_string(params, "workspace_id", 128) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let explicit_conversation_id =
                match continuity_search_string(params, "conversation_id", 512) {
                    Ok(value) => value,
                    Err(error) => return error,
                };
            let conversation_url = match continuity_search_string(params, "conversation_url", 2048)
            {
                Ok(value) => value,
                Err(error) => return error,
            };
            let (url_conversation_id, url_project_id) = match conversation_url {
                Some(raw) => match chatgpt_conversation_url_ref(raw) {
                    Some((conversation_id, project_id)) => (Some(conversation_id), project_id),
                    None => {
                        return json!({
                            "ok": false,
                            "code": "continuity_search_params_invalid",
                            "message": "conversation_url must be a ChatGPT conversation URL",
                        });
                    }
                },
                None => (None, None),
            };
            if explicit_conversation_id
                .is_some_and(|value| Some(value) != url_conversation_id.as_deref())
                && url_conversation_id.is_some()
            {
                return json!({
                    "ok": false,
                    "code": "continuity_search_params_invalid",
                    "message": "conversation_id conflicts with conversation_url",
                });
            }
            if explicit_project_id.is_some_and(|value| Some(value) != url_project_id.as_deref())
                && url_project_id.is_some()
            {
                return json!({
                    "ok": false,
                    "code": "continuity_search_params_invalid",
                    "message": "project_id conflicts with conversation_url",
                });
            }
            let project_id_owned = explicit_project_id.map(str::to_owned).or(url_project_id);
            let conversation_id_owned = explicit_conversation_id
                .map(str::to_owned)
                .or(url_conversation_id);
            let project_id = project_id_owned.as_deref();
            let conversation_id = conversation_id_owned.as_deref();
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

const WORK_MEMORY_CURSOR_PREFIX: &str = "wmc1";
const WORK_MEMORY_CURSOR_MAX_BYTES: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WorkMemoryCursorV1 {
    version: u8,
    kind: String,
    continuity_id: String,
    project_ref: String,
    repo_id: String,
    work_chain_id: String,
    query: String,
    query_sha256: String,
    checkpoint_revision: i64,
    through_evidence_id: Option<String>,
    max_fts_rowid: i64,
    page_size: u64,
    offset: u64,
}

impl WorkMemoryCursorV1 {
    fn boundary(&self) -> WorkMemorySearchBoundary {
        WorkMemorySearchBoundary {
            continuity_id: self.continuity_id.clone(),
            checkpoint_revision: self.checkpoint_revision,
            through_evidence_id: self.through_evidence_id.clone(),
            max_fts_rowid: self.max_fts_rowid,
        }
    }
}

fn work_memory_cursor_sha256(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn encode_work_memory_cursor(cursor: &WorkMemoryCursorV1) -> Result<String, &'static str> {
    let payload = serde_json::to_vec(cursor).map_err(|_| "work_memory_cursor_invalid")?;
    let encoded = URL_SAFE_NO_PAD.encode(&payload);
    let digest = work_memory_cursor_sha256(&payload);
    let token = format!("{WORK_MEMORY_CURSOR_PREFIX}.{encoded}.{digest}");
    if token.len() > WORK_MEMORY_CURSOR_MAX_BYTES {
        return Err("work_memory_cursor_invalid");
    }
    Ok(token)
}

fn decode_work_memory_cursor(value: &str) -> Result<WorkMemoryCursorV1, &'static str> {
    if value.is_empty() || value.len() > WORK_MEMORY_CURSOR_MAX_BYTES {
        return Err("work_memory_cursor_invalid");
    }
    let mut parts = value.split('.');
    let Some(prefix) = parts.next() else {
        return Err("work_memory_cursor_invalid");
    };
    if prefix != WORK_MEMORY_CURSOR_PREFIX {
        return if prefix.starts_with("wmc") {
            Err("work_memory_cursor_version_unsupported")
        } else {
            Err("work_memory_cursor_invalid")
        };
    }
    let (Some(encoded), Some(expected_digest)) = (parts.next(), parts.next()) else {
        return Err("work_memory_cursor_invalid");
    };
    if parts.next().is_some() {
        return Err("work_memory_cursor_invalid");
    }
    let payload = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "work_memory_cursor_invalid")?;
    if expected_digest != work_memory_cursor_sha256(&payload) {
        return Err("work_memory_cursor_invalid");
    }
    let cursor: WorkMemoryCursorV1 =
        serde_json::from_slice(&payload).map_err(|_| "work_memory_cursor_invalid")?;
    if cursor.version != 1 || cursor.kind != "work_memory.search" {
        return Err("work_memory_cursor_version_unsupported");
    }
    if cursor.query.is_empty()
        || cursor.query.len() > 512
        || cursor.query_sha256 != work_memory_cursor_sha256(cursor.query.as_bytes())
        || cursor.checkpoint_revision < 0
        || cursor.max_fts_rowid < 0
        || !(1..=20).contains(&cursor.page_size)
        || cursor.offset > 1_000_000
    {
        return Err("work_memory_cursor_invalid");
    }
    Ok(cursor)
}

fn work_memory_coverage(
    checkpoint_revision: i64,
    through_evidence_id: Option<&str>,
    has_portable_source_claims: bool,
    display_page_truncated: bool,
    display_excerpt_truncated: bool,
) -> Value {
    let mut limitations = Vec::new();
    if display_page_truncated {
        limitations.push("display_page_truncated");
    }
    if display_excerpt_truncated {
        limitations.push("display_excerpt_truncated");
    }
    if has_portable_source_claims {
        limitations.push("source_revision_unverified");
    }
    json!({
        "boundary": {
            "checkpoint_revision": checkpoint_revision,
            "through_evidence_id": through_evidence_id,
        },
        "result_completeness": "complete",
        "source_verification": if has_portable_source_claims { "unverified" } else { "not_applicable" },
        "display_truncated": display_page_truncated || display_excerpt_truncated,
        "limitations": limitations,
    })
}

fn work_memory_search_page_json(
    project_ref: &str,
    repo_id: &str,
    work_chain_id: &str,
    query: &str,
    page_size: usize,
    page: WorkMemorySearchPage,
) -> Value {
    let display_excerpt_truncated = page.hits.iter().any(|hit| hit.excerpt.contains('…'));
    let coverage = work_memory_coverage(
        page.boundary.checkpoint_revision,
        page.boundary.through_evidence_id.as_deref(),
        page.has_portable_source_claims,
        page.has_more,
        display_excerpt_truncated,
    );
    let next_cursor = if page.has_more {
        let cursor = WorkMemoryCursorV1 {
            version: 1,
            kind: "work_memory.search".to_owned(),
            continuity_id: page.boundary.continuity_id.clone(),
            project_ref: project_ref.to_owned(),
            repo_id: repo_id.to_owned(),
            work_chain_id: work_chain_id.to_owned(),
            query: query.to_owned(),
            query_sha256: work_memory_cursor_sha256(query.as_bytes()),
            checkpoint_revision: page.boundary.checkpoint_revision,
            through_evidence_id: page.boundary.through_evidence_id.clone(),
            max_fts_rowid: page.boundary.max_fts_rowid,
            page_size: page_size as u64,
            offset: page.next_offset as u64,
        };
        match encode_work_memory_cursor(&cursor) {
            Ok(value) => Some(value),
            Err(code) => return json!({"ok": false, "code": code}),
        }
    } else {
        None
    };
    json!({
        "ok": true,
        "continuity_id": page.boundary.continuity_id,
        "project_ref": project_ref,
        "repo_id": repo_id,
        "work_chain_id": work_chain_id,
        "hits": page.hits.into_iter().map(|hit| json!({
            "source_kind": hit.source_kind,
            "source_id": hit.source_id,
            "excerpt": hit.excerpt,
        })).collect::<Vec<_>>(),
        "cursor": next_cursor,
        "coverage": coverage,
    })
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
                Ok(Some(record)) => {
                    let coverage = work_memory_coverage(
                        record.checkpoint_revision,
                        record
                            .checkpoint
                            .as_ref()
                            .and_then(|checkpoint| checkpoint.through_evidence_id.as_deref()),
                        !record.evidence_refs.is_empty(),
                        record.turns_truncated || record.evidence_truncated,
                        false,
                    );
                    json!({
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
                    "coverage": coverage,
                    "updated_at": record.updated_at,
                    "instruction": "Treat Work Memory as persisted project context. Re-check live Herdr/runtime/Git state before mutation.",
                    })
                }
                Ok(None) => json!({"ok": false, "code": "work_memory_not_found"}),
                Err(error) => work_memory_store_error(error),
            }
        }
        "work_memory.search" => {
            if let Some(error) = work_memory_reject_unknown(
                object,
                &[
                    "project_ref",
                    "repo_id",
                    "work_chain_id",
                    "query",
                    "limit",
                    "cursor",
                ],
            ) {
                return error;
            }
            if let Some(cursor_value) = params.get("cursor") {
                if object.len() != 1 {
                    return json!({"ok": false, "code": "work_memory_cursor_conflict"});
                }
                let Some(cursor_text) = cursor_value.as_str() else {
                    return json!({"ok": false, "code": "work_memory_cursor_invalid"});
                };
                let cursor = match decode_work_memory_cursor(cursor_text) {
                    Ok(cursor) => cursor,
                    Err(code) => return json!({"ok": false, "code": code}),
                };
                let offset = match usize::try_from(cursor.offset) {
                    Ok(value) => value,
                    Err(_) => {
                        return json!({"ok": false, "code": "work_memory_cursor_invalid"});
                    }
                };
                let page_size = cursor.page_size as usize;
                let boundary = cursor.boundary();
                return match store.work_memory_search_page(
                    &cursor.project_ref,
                    &cursor.repo_id,
                    &cursor.work_chain_id,
                    &cursor.query,
                    WorkMemorySearchPageOptions {
                        limit: page_size,
                        offset,
                        expected_boundary: Some(&boundary),
                    },
                ) {
                    Ok(Some(page)) => work_memory_search_page_json(
                        &cursor.project_ref,
                        &cursor.repo_id,
                        &cursor.work_chain_id,
                        &cursor.query,
                        page_size,
                        page,
                    ),
                    Ok(None) => {
                        json!({"ok": false, "code": "work_memory_cursor_partition_mismatch"})
                    }
                    Err(error) => work_memory_store_error(error),
                };
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
            match store.work_memory_search_page(
                project_ref,
                repo_id,
                work_chain_id,
                query,
                WorkMemorySearchPageOptions {
                    limit,
                    offset: 0,
                    expected_boundary: None,
                },
            ) {
                Ok(Some(page)) => work_memory_search_page_json(
                    project_ref,
                    repo_id,
                    work_chain_id,
                    query,
                    limit,
                    page,
                ),
                Ok(None) => json!({"ok": false, "code": "work_memory_not_found"}),
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
    pub result: Option<Value>,
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
            result: None,
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
        BrowserOperation::SpaceCreate => {
            evidence.stable_resource_ref_observed && evidence.lifecycle_observed
        }
        BrowserOperation::SessionCreate => {
            evidence.stable_resource_ref_observed
                && evidence.lifecycle_observed
                && evidence.canonical_url_observed
                && (evidence.accepted_message_observed || evidence.message_baseline_advanced)
                // The exact provider user-message identity is terminal proof that
                // the first assignment crossed the browser/provider boundary.
                // Assistant generation can begin after the bounded create
                // observation window, so generation-status timing must not turn an
                // already accepted message into delivery-uncertain. Session
                // materialization + locator generation above still fence the new
                // resource to the requested browser generation.
                && browser_evidence_accepted_user_message_ref(evidence)?.is_some()
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
                && browser_evidence_accepted_user_message_ref(evidence)?.is_some()
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

/// Read the exact provider user-message identity that browser actuation proved
/// accepted. The existing `evidence.result` object carries it, so no new
/// top-level postcondition field is introduced. Absent/`null` means the
/// provider did not expose a stable identity and settlement must skip rather
/// than guess.
fn browser_evidence_accepted_user_message_ref(
    evidence: &BrowserPostconditionEvidence,
) -> Result<Option<String>, String> {
    let Some(result) = evidence.result.as_ref() else {
        return Ok(None);
    };
    let Some(value) = result.get("accepted_user_message_ref") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(text) = value.as_str() else {
        return Err("browser_evidence_accepted_user_message_invalid".to_owned());
    };
    if text.is_empty()
        || text.len() > 512
        || text != text.trim()
        || text.chars().any(char::is_control)
    {
        return Err("browser_evidence_accepted_user_message_invalid".to_owned());
    }
    Ok(Some(text.to_owned()))
}

fn browser_dispatch_submit(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    params: &Value,
    actuator: Option<&dyn BrowserActuator>,
    caller_authorization: Option<&BrowserCallerAuthorization>,
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
        parent_dispatch_id: None,
        authorization: caller_authorization.map(|authorization| {
            BrowserDispatchAuthorizationInput {
                principal_ref: &authorization.principal_ref,
                connector_id: &authorization.connector_id,
                grant_generation: authorization.grant_generation,
            }
        }),
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
        accepted_user_message_ref: None,
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
            Some(&reserved.dispatch_id),
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
    let accepted_user_message_ref = match browser_evidence_accepted_user_message_ref(&evidence) {
        Ok(value) => value,
        Err(error) => return browser_store_error(error),
    };
    let current = match store.browser_dispatch(&reserved.dispatch_id) {
        Ok(Some(dispatch)) => dispatch,
        Ok(None) => return json!({"ok": false, "code": "browser_dispatch_not_found"}),
        Err(error) => return browser_store_error(error),
    };
    let dispatch = match current.delivery_state {
        BrowserDeliveryState::Uncertain if delivery_state != BrowserDeliveryState::Uncertain => {
            match store.settle_uncertain_browser_dispatch(BrowserDispatchUpdateInput {
                dispatch_id: &reserved.dispatch_id,
                expected_generation,
                delivery_state,
                generation_owner: evidence.generation_owner,
                accepted_user_message_ref: if delivery_state == BrowserDeliveryState::Applied {
                    accepted_user_message_ref.as_deref()
                } else {
                    None
                },
                updated_at: now,
            }) {
                Ok(dispatch) => dispatch,
                Err(error) => return browser_store_error(error),
            }
        }
        BrowserDeliveryState::Uncertain => current,
        BrowserDeliveryState::NotApplied => {
            return json!({"ok": false, "code": "browser_dispatch_not_claimed"});
        }
        _ => current,
    };
    let work_memory_writeback = browser_dispatch_work_memory_writeback(&mut store, &dispatch);
    let success = matches!(
        dispatch.delivery_state,
        BrowserDeliveryState::Applied | BrowserDeliveryState::Stopped
    );
    json!({
        "ok": success,
        "code": if success { Value::Null } else { json!(dispatch.delivery_state.as_str()) },
        "dispatch": browser_dispatch_json(dispatch),
        "replayed": false,
        "work_memory_writeback": work_memory_writeback,
    })
}

fn browser_dispatch_stop(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    params: &Value,
    actuator: Option<&dyn BrowserActuator>,
    caller_authorization: Option<&BrowserCallerAuthorization>,
) -> Value {
    let parent_dispatch_id = params.get("dispatch_id").and_then(Value::as_str).unwrap();
    let expected_generation = params
        .get("expected_generation")
        .and_then(Value::as_i64)
        .unwrap();
    let idempotency_key = params
        .get("idempotency_key")
        .and_then(Value::as_str)
        .unwrap();
    let now = browser_epoch_ms();
    let idempotency_key_digest = browser_sha256(idempotency_key);
    let request_digest = browser_sha256(
        &json!({
            "operation": "browser_dispatch.stop",
            "dispatch_id": parent_dispatch_id,
            "expected_generation": expected_generation,
        })
        .to_string(),
    );
    let empty_digest = browser_sha256("");

    let Ok(mut store_guard) = store.lock() else {
        return json!({"ok": false, "code": "browser_operation_store_unavailable"});
    };
    let parent = match store_guard.browser_dispatch(parent_dispatch_id) {
        Ok(Some(dispatch)) => dispatch,
        Ok(None) => return json!({"ok": false, "code": "browser_dispatch_not_found"}),
        Err(error) => return browser_store_error(error),
    };
    if parent.operation != "browser_dispatch.submit" {
        return json!({"ok": false, "code": "browser_dispatch_not_stoppable"});
    }
    if parent.expected_generation != expected_generation {
        return json!({"ok": false, "code": "stale_capability_generation"});
    }
    if parent.delivery_state == BrowserDeliveryState::Stopped {
        return json!({
            "ok": true,
            "code": Value::Null,
            "operation": BrowserOperation::DispatchStop.method(),
            "delivery_state": BrowserDeliveryState::Stopped.as_str(),
            "target_dispatch": browser_dispatch_json(parent),
            "replayed": true,
        });
    }
    if parent.delivery_state != BrowserDeliveryState::Applied
        || parent.generation_owner != Some(expected_generation)
    {
        return json!({"ok": false, "code": "browser_dispatch_not_stoppable"});
    }

    let reservation = store_guard.reserve_browser_dispatch(BrowserDispatchReserveInput {
        endpoint_ref: &parent.endpoint_ref,
        provider: &parent.provider,
        operation: "browser_dispatch.stop",
        target_session_ref: &parent.target_session_ref,
        request_digest: &request_digest,
        message_digest: &empty_digest,
        reasoning_effort: None,
        required_apps: &[],
        expected_generation,
        idempotency_key_digest: &idempotency_key_digest,
        parent_dispatch_id: Some(parent_dispatch_id),
        authorization: caller_authorization.map(|authorization| {
            BrowserDispatchAuthorizationInput {
                principal_ref: &authorization.principal_ref,
                connector_id: &authorization.connector_id,
                grant_generation: authorization.grant_generation,
            }
        }),
        work_chain_id: parent.work_chain_id.as_deref(),
        lane_id: parent.lane_id.as_deref(),
        created_at: now,
    });
    let reserved = match reservation {
        Ok(BrowserDispatchReservation::Existing(stop_dispatch)) => {
            let success = stop_dispatch.delivery_state == BrowserDeliveryState::Stopped;
            let target_dispatch = if success {
                match store_guard
                    .mark_browser_dispatch_stopped_from_child(&stop_dispatch.dispatch_id, now)
                {
                    Ok(dispatch) => dispatch,
                    Err(error) => return browser_store_error(error),
                }
            } else {
                parent
            };
            return json!({
                "ok": success,
                "code": if success { Value::Null } else { json!(stop_dispatch.delivery_state.as_str()) },
                "operation": BrowserOperation::DispatchStop.method(),
                "delivery_state": stop_dispatch.delivery_state.as_str(),
                "dispatch": browser_dispatch_json(stop_dispatch),
                "target_dispatch": browser_dispatch_json(target_dispatch),
                "replayed": true,
            });
        }
        Ok(BrowserDispatchReservation::Reserved(dispatch)) => dispatch,
        Err(error) => return browser_store_error(error),
    };

    if let Err(error) = store_guard.update_browser_dispatch(BrowserDispatchUpdateInput {
        dispatch_id: &reserved.dispatch_id,
        expected_generation,
        delivery_state: BrowserDeliveryState::Uncertain,
        generation_owner: None,
        accepted_user_message_ref: None,
        updated_at: now,
    }) {
        return browser_store_error(error);
    }
    drop(store_guard);

    let evidence = match actuator {
        Some(actuator) => match actuator.actuate(
            BrowserOperation::DispatchStop.method(),
            params,
            expected_generation,
            Some(&reserved.dispatch_id),
        ) {
            Ok(evidence) => evidence,
            Err(error) => return browser_store_error(error),
        },
        None => BrowserPostconditionEvidence::resource_unavailable(expected_generation),
    };
    let delivery_state = match browser_delivery_state_from_postcondition(
        BrowserOperation::DispatchStop,
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
    let stop_dispatch = if delivery_state == BrowserDeliveryState::Uncertain {
        match store.browser_dispatch(&reserved.dispatch_id) {
            Ok(Some(dispatch)) => dispatch,
            Ok(None) => return json!({"ok": false, "code": "browser_dispatch_not_found"}),
            Err(error) => return browser_store_error(error),
        }
    } else {
        match store.settle_uncertain_browser_dispatch(BrowserDispatchUpdateInput {
            dispatch_id: &reserved.dispatch_id,
            expected_generation,
            delivery_state,
            generation_owner: evidence.generation_owner,
            accepted_user_message_ref: None,
            updated_at: browser_epoch_ms(),
        }) {
            Ok(dispatch) => dispatch,
            Err(error) => return browser_store_error(error),
        }
    };
    let target_dispatch = if delivery_state == BrowserDeliveryState::Stopped {
        match store.mark_browser_dispatch_stopped_from_child(
            &stop_dispatch.dispatch_id,
            browser_epoch_ms(),
        ) {
            Ok(dispatch) => dispatch,
            Err(error) => return browser_store_error(error),
        }
    } else {
        parent
    };
    let work_memory_writeback = browser_dispatch_work_memory_writeback(&mut store, &stop_dispatch);
    let success = delivery_state == BrowserDeliveryState::Stopped;
    json!({
        "ok": success,
        "code": if success { Value::Null } else { json!(delivery_state.as_str()) },
        "operation": BrowserOperation::DispatchStop.method(),
        "delivery_state": delivery_state.as_str(),
        "dispatch": browser_dispatch_json(stop_dispatch),
        "target_dispatch": browser_dispatch_json(target_dispatch),
        "replayed": false,
        "work_memory_writeback": work_memory_writeback,
    })
}

fn browser_dispatch_status(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    dispatch_id: &str,
    actuator: Option<&dyn BrowserActuator>,
) -> Value {
    let dispatch = {
        let Ok(store) = store.lock() else {
            return json!({"ok": false, "code": "browser_operation_store_unavailable"});
        };
        match store.browser_dispatch(dispatch_id) {
            Ok(Some(dispatch)) => dispatch,
            Ok(None) => return json!({"ok": false, "code": "browser_dispatch_not_found"}),
            Err(error) => return browser_store_error(error),
        }
    };

    if dispatch.delivery_state == BrowserDeliveryState::Uncertain
        && let Some(actuator) = actuator
    {
        let evidence = match actuator
            .reconcile_dispatch(&dispatch.dispatch_id, dispatch.expected_generation)
        {
            Ok(Some(evidence)) => Some(evidence),
            Ok(None) => None,
            Err(error) => return browser_store_error(error),
        };
        if let Some(evidence) = evidence {
            let operation = match dispatch.operation.as_str() {
                "browser_dispatch.submit" => BrowserOperation::DispatchSubmit,
                "browser_dispatch.stop" => BrowserOperation::DispatchStop,
                _ => {
                    return json!({
                        "ok": true,
                        "dispatch": browser_dispatch_json(dispatch),
                        "reconciled": false,
                        "reconciliation_code": "unsupported_dispatch_operation",
                    });
                }
            };
            let delivery_state = match browser_delivery_state_from_postcondition(
                operation,
                &json!({}),
                dispatch.expected_generation,
                &evidence,
            ) {
                Ok(state) => state,
                Err(code) => {
                    return json!({
                        "ok": true,
                        "dispatch": browser_dispatch_json(dispatch),
                        "reconciled": false,
                        "reconciliation_code": code,
                    });
                }
            };
            if delivery_state != BrowserDeliveryState::Uncertain {
                let accepted_user_message_ref =
                    match browser_evidence_accepted_user_message_ref(&evidence) {
                        Ok(value) => value,
                        Err(error) => return browser_store_error(error),
                    };
                let Ok(mut store) = store.lock() else {
                    return json!({"ok": false, "code": "browser_operation_store_unavailable"});
                };
                let settled = match store.settle_uncertain_browser_dispatch(
                    BrowserDispatchUpdateInput {
                        dispatch_id: &dispatch.dispatch_id,
                        expected_generation: dispatch.expected_generation,
                        delivery_state,
                        generation_owner: evidence.generation_owner,
                        accepted_user_message_ref: if operation == BrowserOperation::DispatchSubmit
                            && delivery_state == BrowserDeliveryState::Applied
                        {
                            accepted_user_message_ref.as_deref()
                        } else {
                            None
                        },
                        updated_at: browser_epoch_ms(),
                    },
                ) {
                    Ok(record) => record,
                    Err(error)
                        if matches!(
                            error.as_str(),
                            "browser_dispatch_already_settled"
                                | "browser_dispatch_settlement_raced"
                        ) =>
                    {
                        match store.browser_dispatch(&dispatch.dispatch_id) {
                            Ok(Some(record)) => record,
                            Ok(None) => {
                                return json!({"ok": false, "code": "browser_dispatch_not_found"});
                            }
                            Err(error) => return browser_store_error(error),
                        }
                    }
                    Err(error) => return browser_store_error(error),
                };
                let target_dispatch = if operation == BrowserOperation::DispatchStop
                    && settled.delivery_state == BrowserDeliveryState::Stopped
                {
                    match store.mark_browser_dispatch_stopped_from_child(
                        &settled.dispatch_id,
                        browser_epoch_ms(),
                    ) {
                        Ok(record) => Some(browser_dispatch_json(record)),
                        Err(error) => return browser_store_error(error),
                    }
                } else {
                    None
                };
                let work_memory_writeback =
                    browser_dispatch_work_memory_writeback(&mut store, &settled);
                return json!({
                    "ok": true,
                    "dispatch": browser_dispatch_json(settled),
                    "target_dispatch": target_dispatch,
                    "reconciled": true,
                    "reconciliation_code": Value::Null,
                    "work_memory_writeback": work_memory_writeback,
                });
            }
            return json!({
                "ok": true,
                "dispatch": browser_dispatch_json(dispatch),
                "reconciled": false,
                "reconciliation_code": "uncertain",
            });
        }
    }

    json!({
        "ok": true,
        "dispatch": browser_dispatch_json(dispatch),
        "reconciled": false,
        "reconciliation_code": Value::Null,
    })
}

fn browser_created_session_dispatch(
    store: &mut StateStore,
    reservation: &BrowserSessionReservationRecord,
    params: &Value,
    caller_authorization: Option<&BrowserCallerAuthorization>,
) -> Result<(crate::state_store::BrowserDispatchRecord, Value), String> {
    let session_ref = reservation
        .session_ref
        .as_deref()
        .ok_or_else(|| "browser_session_materialization_missing".to_owned())?;
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| "browser_message_required".to_owned())?;
    let reasoning_effort = params.get("reasoning_effort").and_then(Value::as_str);
    let required_apps = browser_required_apps(params, true)
        .map_err(|_| "browser_required_apps_invalid".to_owned())?;
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
            "expected_generation": reservation.expected_generation,
            "work_chain_id": work_chain_id,
            "lane_id": lane_id,
        })
        .to_string(),
    );
    let reservation_result = store.reserve_browser_dispatch(BrowserDispatchReserveInput {
        endpoint_ref: &reservation.endpoint_ref,
        provider: &reservation.provider,
        operation: "browser_dispatch.submit",
        target_session_ref: session_ref,
        request_digest: &request_digest,
        message_digest: &message_digest,
        reasoning_effort,
        required_apps: &required_apps,
        expected_generation: reservation.expected_generation,
        idempotency_key_digest: &reservation.idempotency_key_digest,
        parent_dispatch_id: None,
        authorization: caller_authorization.map(|authorization| {
            BrowserDispatchAuthorizationInput {
                principal_ref: &authorization.principal_ref,
                connector_id: &authorization.connector_id,
                grant_generation: authorization.grant_generation,
            }
        }),
        work_chain_id,
        lane_id,
        created_at: browser_epoch_ms(),
    })?;
    let dispatch = match reservation_result {
        BrowserDispatchReservation::Existing(dispatch) => {
            // A replay may encounter a dispatch created by an older runtime or
            // a crash-recovery path before accepted-message linkage was folded
            // into the delivery update. Repair only that existing row here.
            match reservation.accepted_user_message_ref.as_deref() {
                Some(accepted_user_message_ref)
                    if dispatch.accepted_user_message_ref.as_deref()
                        != Some(accepted_user_message_ref) =>
                {
                    store.record_browser_dispatch_accepted_message(
                        &dispatch.dispatch_id,
                        reservation.expected_generation,
                        accepted_user_message_ref,
                        browser_epoch_ms(),
                    )?
                }
                _ => dispatch,
            }
        }
        BrowserDispatchReservation::Reserved(dispatch) => {
            // Fresh materialization persists Applied + provider user-message
            // identity in one transaction, closing the acceptance crash window.
            store.update_browser_dispatch(BrowserDispatchUpdateInput {
                dispatch_id: &dispatch.dispatch_id,
                expected_generation: reservation.expected_generation,
                delivery_state: BrowserDeliveryState::Applied,
                generation_owner: Some(reservation.expected_generation),
                accepted_user_message_ref: reservation.accepted_user_message_ref.as_deref(),
                updated_at: browser_epoch_ms(),
            })?
        }
    };
    if dispatch.delivery_state != BrowserDeliveryState::Applied
        || dispatch.generation_owner != Some(reservation.expected_generation)
    {
        return Err("browser_created_session_dispatch_not_applied".to_owned());
    }
    let writeback = browser_dispatch_work_memory_writeback(store, &dispatch);
    Ok((dispatch, writeback))
}

fn promote_materialized_browser_session_delivery(
    store: &mut StateStore,
    reservation: &BrowserSessionReservationRecord,
    expected_generation: i64,
) -> Result<Option<BrowserSessionReservationRecord>, String> {
    if reservation.state != "materialized"
        || BrowserDeliveryState::parse(&reservation.delivery_state)?
            != BrowserDeliveryState::Uncertain
    {
        return Ok(None);
    }
    let Some(accepted_user_message_ref) = reservation.accepted_user_message_ref.as_deref() else {
        return Ok(None);
    };
    store
        .update_browser_session_reservation_delivery(
            &reservation.reservation_ref,
            expected_generation,
            BrowserDeliveryState::Applied,
            Some(accepted_user_message_ref),
            browser_epoch_ms(),
        )
        .map(Some)
}

fn browser_session_create_success(
    store: &mut StateStore,
    reservation_ref: &str,
    params: &Value,
    replayed: bool,
    reconciled: bool,
    caller_authorization: Option<&BrowserCallerAuthorization>,
) -> Value {
    let reservation = match store.browser_session_reservation(reservation_ref) {
        Ok(Some(record)) => record,
        Ok(None) => return json!({"ok": false, "code": "browser_session_reservation_not_found"}),
        Err(error) => return browser_store_error(error),
    };
    if reservation.state != "materialized" || reservation.delivery_state != "applied" {
        return json!({
            "ok": false,
            "code": if reservation.delivery_state == "uncertain" {
                "uncertain"
            } else {
                "browser_session_materialization_missing"
            },
            "reservation_ref": reservation.reservation_ref,
            "reservation_state": reservation.state,
            "delivery_state": reservation.delivery_state,
            "replayed": replayed,
            "reconciled": reconciled,
        });
    }
    let session_ref = reservation.session_ref.clone().unwrap();
    let (dispatch, work_memory_writeback) =
        match browser_created_session_dispatch(store, &reservation, params, caller_authorization) {
            Ok(value) => value,
            Err(error) => return browser_store_error(error),
        };
    json!({
        "ok": true,
        "code": Value::Null,
        "operation": BrowserOperation::SessionCreate.method(),
        "reservation_ref": reservation.reservation_ref,
        "reservation_state": reservation.state,
        "session_ref": session_ref,
        "delivery_state": BrowserDeliveryState::Applied.as_str(),
        "dispatch": browser_dispatch_json(dispatch),
        "replayed": replayed,
        "reconciled": reconciled,
        "work_memory_writeback": work_memory_writeback,
    })
}

fn browser_session_create_params_from_source(
    store: &StateStore,
    params: &Value,
) -> Result<Option<Value>, Value> {
    let Some(source_url) = params.get("source_url") else {
        return Ok(None);
    };
    let Some(source_url) = source_url
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(json!({"ok": false, "code": "browser_operation_params_invalid"}));
    };
    let Some(object) = params.as_object() else {
        return Err(json!({"ok": false, "code": "browser_operation_params_invalid"}));
    };
    const ALLOWED: &[&str] = &[
        "source_url",
        "message",
        "idempotency_key",
        "work_chain_id",
        "lane_id",
    ];
    if object.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        return Err(json!({"ok": false, "code": "browser_operation_params_invalid"}));
    }
    let message = browser_required_string(params, "message", 262_144)?;
    let idempotency_key = browser_required_idempotency_key(params)?;
    let work_chain_id = browser_optional_string(params, "work_chain_id", 128)?;
    let lane_id = browser_optional_string(params, "lane_id", 160)?;
    let session_ref = match store.browser_session_ref_for_canonical_url(source_url) {
        Ok(Some(value)) => value,
        Ok(None) => return Err(json!({"ok": false, "code": "browser_source_session_not_found"})),
        Err(error) => return Err(browser_store_error(error)),
    };
    let session = match store.browser_resource(&session_ref) {
        Ok(Some(value)) => value,
        Ok(None) => return Err(json!({"ok": false, "code": "browser_source_session_not_found"})),
        Err(error) => return Err(browser_store_error(error)),
    };
    let Some(parent_ref) = session.parent_ref.as_deref() else {
        return Err(json!({"ok": false, "code": "browser_source_scope_missing"}));
    };
    let parent = match store.browser_resource(parent_ref) {
        Ok(Some(value)) => value,
        Ok(None) => return Err(json!({"ok": false, "code": "browser_source_scope_missing"})),
        Err(error) => return Err(browser_store_error(error)),
    };
    let (space_ref, account) = if parent.kind == "space" {
        let Some(account_ref) = parent.parent_ref.as_deref() else {
            return Err(json!({"ok": false, "code": "browser_source_scope_missing"}));
        };
        let account = match store.browser_resource(account_ref) {
            Ok(Some(value)) => value,
            Ok(None) => return Err(json!({"ok": false, "code": "browser_source_scope_missing"})),
            Err(error) => return Err(browser_store_error(error)),
        };
        (Some(parent.resource_ref.as_str()), account)
    } else if parent.kind == "account" {
        (None, parent.clone())
    } else {
        return Err(json!({"ok": false, "code": "browser_source_scope_missing"}));
    };
    if session.provider != "chatgpt"
        || parent.provider != session.provider
        || account.kind != "account"
        || account.provider != session.provider
        || parent.endpoint_ref != session.endpoint_ref
        || account.endpoint_ref != session.endpoint_ref
        || parent.observation_generation != session.observation_generation
        || account.observation_generation != session.observation_generation
    {
        return Err(json!({"ok": false, "code": "browser_source_scope_mismatch"}));
    }
    let display_label = parent
        .display_label
        .as_deref()
        .or(session.display_label.as_deref())
        .unwrap_or("ChatGPT continuation");
    Ok(Some(json!({
        "endpoint_ref": session.endpoint_ref,
        "provider": "chatgpt",
        "account_ref": account.resource_ref,
        "space_ref": space_ref,
        "display_label": display_label,
        "message": message,
        "expected_generation": session.observation_generation,
        "idempotency_key": idempotency_key,
        "work_chain_id": work_chain_id,
        "lane_id": lane_id,
    })))
}

fn browser_session_create(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    params: &Value,
    actuator: Option<&dyn BrowserActuator>,
    caller_authorization: Option<&BrowserCallerAuthorization>,
) -> Value {
    let endpoint_ref = params.get("endpoint_ref").and_then(Value::as_str).unwrap();
    let provider = params.get("provider").and_then(Value::as_str).unwrap();
    let account_ref = params.get("account_ref").and_then(Value::as_str).unwrap();
    let space_ref = params.get("space_ref").and_then(Value::as_str);
    let display_label = params.get("display_label").and_then(Value::as_str).unwrap();
    let message = params.get("message").and_then(Value::as_str).unwrap();
    let reasoning_effort = params.get("reasoning_effort").and_then(Value::as_str);
    let required_apps = match browser_required_apps(params, true) {
        Ok(apps) => apps,
        Err(error) => return error,
    };
    let expected_generation = params
        .get("expected_generation")
        .and_then(Value::as_i64)
        .unwrap();
    let idempotency_key = params
        .get("idempotency_key")
        .and_then(Value::as_str)
        .unwrap();
    let work_chain_id = params.get("work_chain_id").and_then(Value::as_str);
    let lane_id = params.get("lane_id").and_then(Value::as_str);
    let message_digest = browser_sha256(message);
    let request_digest = browser_sha256(
        &json!({
            "operation": BrowserOperation::SessionCreate.method(),
            "endpoint_ref": endpoint_ref,
            "provider": provider,
            "account_ref": account_ref,
            "space_ref": space_ref,
            "display_label": display_label,
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
    let expires_at = now.saturating_add(30 * 60 * 1000);

    let (reservation, launch_url) = {
        let Ok(mut guard) = store.lock() else {
            return json!({"ok": false, "code": "browser_operation_store_unavailable"});
        };
        let launch_ref = space_ref.unwrap_or(account_ref);
        let locator = match guard.browser_resource_locator(launch_ref) {
            Ok(Some(locator)) if locator.observation_generation == expected_generation => locator,
            Ok(Some(_)) => return json!({"ok": false, "code": "stale_capability_generation"}),
            Ok(None) => {
                return json!({
                    "ok": false,
                    "code": "resource_unavailable",
                    "message": "browser session creation requires an observed local account/project launcher",
                });
            }
            Err(error) => return browser_store_error(error),
        };
        let reserved = match guard.reserve_browser_session(BrowserSessionReservationInput {
            endpoint_ref,
            provider,
            account_ref,
            space_ref,
            display_label,
            expected_generation,
            idempotency_key_digest: &idempotency_key_digest,
            request_digest: &request_digest,
            created_at: now,
            expires_at,
        }) {
            Ok(value) => value,
            Err(error) => return browser_store_error(error),
        };
        (reserved, locator.canonical_url)
    };

    let (reservation, replayed) = match reservation {
        BrowserSessionReservation::Reserved(record) => (record, false),
        BrowserSessionReservation::Existing(record) => (record, true),
    };
    if matches!(reservation.state.as_str(), "cancelled" | "expired") {
        return json!({
            "ok": false,
            "code": format!("browser_session_reservation_{}", reservation.state),
            "reservation_ref": reservation.reservation_ref,
        });
    }
    let current_delivery = match BrowserDeliveryState::parse(&reservation.delivery_state) {
        Ok(state) => state,
        Err(error) => return browser_store_error(error),
    };
    if current_delivery == BrowserDeliveryState::Applied {
        let Ok(mut guard) = store.lock() else {
            return json!({"ok": false, "code": "browser_operation_store_unavailable"});
        };
        return browser_session_create_success(
            &mut guard,
            &reservation.reservation_ref,
            params,
            true,
            false,
            caller_authorization,
        );
    }
    if current_delivery == BrowserDeliveryState::Uncertain {
        {
            let Ok(mut guard) = store.lock() else {
                return json!({"ok": false, "code": "browser_operation_store_unavailable"});
            };
            match promote_materialized_browser_session_delivery(
                &mut guard,
                &reservation,
                expected_generation,
            ) {
                Ok(Some(promoted)) => {
                    return browser_session_create_success(
                        &mut guard,
                        &promoted.reservation_ref,
                        params,
                        true,
                        true,
                        caller_authorization,
                    );
                }
                Ok(None) => {}
                Err(error) => return browser_store_error(error),
            }
        }
        let Some(actuator) = actuator else {
            return json!({
                "ok": false,
                "code": "uncertain",
                "reservation_ref": reservation.reservation_ref,
                "reservation_state": reservation.state,
                "delivery_state": current_delivery.as_str(),
                "replayed": true,
            });
        };
        let evidence =
            match actuator.reconcile_dispatch(&reservation.reservation_ref, expected_generation) {
                Ok(Some(evidence)) => evidence,
                Ok(None) => {
                    return json!({
                        "ok": false,
                        "code": "uncertain",
                        "reservation_ref": reservation.reservation_ref,
                        "reservation_state": reservation.state,
                        "delivery_state": current_delivery.as_str(),
                        "replayed": true,
                    });
                }
                Err(error) => return browser_store_error(error),
            };
        let delivery_state = match browser_delivery_state_from_postcondition(
            BrowserOperation::SessionCreate,
            params,
            expected_generation,
            &evidence,
        ) {
            Ok(state) => state,
            Err(error) => return browser_store_error(error),
        };
        let accepted_user_message_ref = match browser_evidence_accepted_user_message_ref(&evidence)
        {
            Ok(value) => value,
            Err(error) => return browser_store_error(error),
        };
        let Ok(mut guard) = store.lock() else {
            return json!({"ok": false, "code": "browser_operation_store_unavailable"});
        };
        let settled = match guard.update_browser_session_reservation_delivery(
            &reservation.reservation_ref,
            expected_generation,
            delivery_state,
            accepted_user_message_ref.as_deref(),
            browser_epoch_ms(),
        ) {
            Ok(record) => record,
            Err(error) => return browser_store_error(error),
        };
        if delivery_state == BrowserDeliveryState::Applied {
            return browser_session_create_success(
                &mut guard,
                &settled.reservation_ref,
                params,
                true,
                true,
                caller_authorization,
            );
        }
        if delivery_state == BrowserDeliveryState::Uncertain {
            match promote_materialized_browser_session_delivery(
                &mut guard,
                &settled,
                expected_generation,
            ) {
                Ok(Some(promoted)) => {
                    return browser_session_create_success(
                        &mut guard,
                        &promoted.reservation_ref,
                        params,
                        true,
                        true,
                        caller_authorization,
                    );
                }
                Ok(None) => {}
                Err(error) => return browser_store_error(error),
            }
        }
        return json!({
            "ok": false,
            "code": delivery_state.as_str(),
            "reservation_ref": settled.reservation_ref,
            "reservation_state": settled.state,
            "delivery_state": settled.delivery_state,
            "replayed": true,
            "reconciled": delivery_state != BrowserDeliveryState::Uncertain,
        });
    }
    if current_delivery != BrowserDeliveryState::NotApplied {
        return json!({
            "ok": false,
            "code": current_delivery.as_str(),
            "reservation_ref": reservation.reservation_ref,
            "reservation_state": reservation.state,
            "delivery_state": current_delivery.as_str(),
            "replayed": replayed,
        });
    }

    {
        let Ok(mut guard) = store.lock() else {
            return json!({"ok": false, "code": "browser_operation_store_unavailable"});
        };
        if let Err(error) = guard.update_browser_session_reservation_delivery(
            &reservation.reservation_ref,
            expected_generation,
            BrowserDeliveryState::Uncertain,
            None,
            browser_epoch_ms(),
        ) {
            return browser_store_error(error);
        }
    }
    let mut actuation_params = params.clone();
    if let Some(object) = actuation_params.as_object_mut() {
        object.insert(
            "reservation_ref".to_owned(),
            json!(reservation.reservation_ref),
        );
        object.insert("launch_url".to_owned(), json!(launch_url));
    }
    let evidence = match actuator {
        Some(actuator) => match actuator.actuate(
            BrowserOperation::SessionCreate.method(),
            &actuation_params,
            expected_generation,
            Some(&reservation.reservation_ref),
        ) {
            Ok(evidence) => evidence,
            Err(error) => return browser_store_error(error),
        },
        None => BrowserPostconditionEvidence::resource_unavailable(expected_generation),
    };
    let delivery_state = match browser_delivery_state_from_postcondition(
        BrowserOperation::SessionCreate,
        params,
        expected_generation,
        &evidence,
    ) {
        Ok(state) => state,
        Err(error) => return browser_store_error(error),
    };
    let accepted_user_message_ref = match browser_evidence_accepted_user_message_ref(&evidence) {
        Ok(value) => value,
        Err(error) => return browser_store_error(error),
    };
    let Ok(mut guard) = store.lock() else {
        return json!({"ok": false, "code": "browser_operation_store_unavailable"});
    };
    let updated = match guard.update_browser_session_reservation_delivery(
        &reservation.reservation_ref,
        expected_generation,
        delivery_state,
        accepted_user_message_ref.as_deref(),
        browser_epoch_ms(),
    ) {
        Ok(record) => record,
        Err(error) => return browser_store_error(error),
    };
    if delivery_state == BrowserDeliveryState::Applied {
        return browser_session_create_success(
            &mut guard,
            &updated.reservation_ref,
            params,
            replayed,
            false,
            caller_authorization,
        );
    }
    if delivery_state == BrowserDeliveryState::Uncertain {
        match promote_materialized_browser_session_delivery(
            &mut guard,
            &updated,
            expected_generation,
        ) {
            Ok(Some(promoted)) => {
                return browser_session_create_success(
                    &mut guard,
                    &promoted.reservation_ref,
                    params,
                    replayed,
                    true,
                    caller_authorization,
                );
            }
            Ok(None) => {}
            Err(error) => return browser_store_error(error),
        }
    }
    json!({
        "ok": false,
        "code": delivery_state.as_str(),
        "operation": BrowserOperation::SessionCreate.method(),
        "reservation_ref": updated.reservation_ref,
        "reservation_state": updated.state,
        "delivery_state": updated.delivery_state,
        "replayed": replayed,
        "reconciled": false,
    })
}

fn browser_session_open(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    params: &Value,
    actuator: Option<&dyn BrowserActuator>,
) -> Value {
    let session_ref = params.get("session_ref").and_then(Value::as_str).unwrap();
    let expected_generation = params
        .get("expected_generation")
        .and_then(Value::as_i64)
        .unwrap();
    let idempotency_key = params
        .get("idempotency_key")
        .and_then(Value::as_str)
        .unwrap();
    let request_hash = browser_sha256(
        &json!({
            "operation": BrowserOperation::SessionOpen.method(),
            "session_ref": session_ref,
            "expected_generation": expected_generation,
        })
        .to_string(),
    );
    let idempotency_digest = browser_sha256(idempotency_key);
    let op_id = format!("op:browser_session_open:{}", &idempotency_digest[..32]);
    let now = browser_epoch_ms();
    let expires_at = now.saturating_add(10 * 60 * 1000);

    let (reservation, provider, canonical_url) = {
        let Ok(mut guard) = store.lock() else {
            return json!({"ok": false, "code": "browser_operation_store_unavailable"});
        };
        let session = match guard.browser_resource(session_ref) {
            Ok(Some(resource)) if resource.kind == "session" => resource,
            Ok(Some(_)) => return json!({"ok": false, "code": "browser_resource_kind_mismatch"}),
            Ok(None) => return json!({"ok": false, "code": "browser_resource_not_found"}),
            Err(error) => return browser_store_error(error),
        };
        if session.observation_generation != expected_generation {
            return json!({"ok": false, "code": "stale_capability_generation"});
        }
        let canonical_url = match guard.browser_resource_locator(session_ref) {
            Ok(Some(locator)) if locator.observation_generation == expected_generation => {
                Some(locator.canonical_url)
            }
            Ok(Some(_)) => return json!({"ok": false, "code": "stale_capability_generation"}),
            Ok(None) => None,
            Err(error) => return browser_store_error(error),
        };
        let reservation = match guard.reserve_operation(
            "browser_session.open",
            &idempotency_digest,
            &request_hash,
            &op_id,
            now,
            expires_at,
        ) {
            Ok(value) => value,
            Err(error) => return browser_store_error(error),
        };
        (reservation, session.provider, canonical_url)
    };

    match reservation {
        OperationReservation::Existing(record) => {
            if record.request_hash != request_hash {
                return json!({
                    "ok": false,
                    "code": "idempotency_key_conflict",
                    "op_id": record.op_id,
                });
            }
            match record.state.as_deref() {
                Some("pending") => {
                    return json!({
                        "ok": false,
                        "code": "idempotency_in_flight",
                        "op_id": record.op_id,
                    });
                }
                Some("complete") => {
                    let Some(result_json) = record.result_json else {
                        return json!({"ok": false, "code": "idempotency_record_corrupt", "op_id": record.op_id});
                    };
                    let mut replay: Value = match serde_json::from_str(&result_json) {
                        Ok(value) => value,
                        Err(_) => {
                            return json!({"ok": false, "code": "idempotency_record_corrupt", "op_id": record.op_id});
                        }
                    };
                    if let Some(object) = replay.as_object_mut() {
                        object.insert("idempotent_replay".to_owned(), json!(true));
                        object.insert("op_id".to_owned(), json!(record.op_id));
                    }
                    return replay;
                }
                _ => {
                    return json!({"ok": false, "code": "idempotency_record_corrupt", "op_id": record.op_id});
                }
            }
        }
        OperationReservation::Reserved => {}
    }

    let mut actuation_params = params.clone();
    if let Some(object) = actuation_params.as_object_mut() {
        object.insert("provider".to_owned(), json!(provider));
        if let Some(canonical_url) = canonical_url {
            object.insert("canonical_url".to_owned(), json!(canonical_url));
        }
    }
    let evidence = match actuator {
        Some(actuator) => match actuator.actuate(
            BrowserOperation::SessionOpen.method(),
            &actuation_params,
            expected_generation,
            None,
        ) {
            Ok(evidence) => evidence,
            Err(error) => return browser_store_error(error),
        },
        None => BrowserPostconditionEvidence::resource_unavailable(expected_generation),
    };
    let delivery_state = match browser_delivery_state_from_postcondition(
        BrowserOperation::SessionOpen,
        params,
        expected_generation,
        &evidence,
    ) {
        Ok(state) => state,
        Err(error) => return browser_store_error(error),
    };
    let mut result =
        browser_operation_delivery_result(BrowserOperation::SessionOpen, delivery_state);
    if let Some(object) = result.as_object_mut() {
        object.insert("op_id".to_owned(), json!(op_id));
        object.insert("idempotent_replay".to_owned(), json!(false));
    }
    let result_json = serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_owned());
    let Ok(mut guard) = store.lock() else {
        return json!({"ok": false, "code": "browser_operation_store_unavailable"});
    };
    if let Err(error) = guard.complete_operation(
        "browser_session.open",
        &idempotency_digest,
        &request_hash,
        &result_json,
        browser_epoch_ms(),
        expires_at,
    ) {
        return json!({
            "ok": false,
            "code": "idempotency_completion_persist_failed",
            "message": error,
            "op_id": op_id,
        });
    }
    result
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
    browser_operation_call_with_grants(store, method, params, &[], None, None)
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
    browser_operation_call_with_grants(store, method, params, &grants, browser_actuator, None)
}

fn browser_operation_alpha4_supported(operation: BrowserOperation, params: &Value) -> bool {
    if matches!(
        operation,
        BrowserOperation::SessionOpen | BrowserOperation::DispatchStop
    ) {
        return true;
    }
    if operation == BrowserOperation::SessionCreate {
        return params.get("provider").and_then(Value::as_str) == Some("chatgpt")
            && params
                .get("reasoning_effort")
                .is_none_or(|value| value.is_null())
            && browser_required_apps(params, true).is_ok();
    }
    if operation != BrowserOperation::DispatchSubmit {
        return !operation.is_mutation();
    }
    params.get("reasoning_effort").is_none_or(Value::is_null)
        && browser_required_apps(params, true).is_ok_and(|apps| apps.is_empty())
}

struct BrowserOperationControls<'a> {
    mutation_gate: Option<&'a std::sync::RwLock<()>>,
    mutation_admission: Option<&'a BrowserMutationAdmission>,
    caller_authorization: Option<&'a BrowserCallerAuthorization>,
}

#[cfg(test)]
fn browser_operation_call_with_grants(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    method: &str,
    params: &Value,
    caller_webchat_control_grants: &[BrowserCallerGrant],
    browser_actuator: Option<&dyn BrowserActuator>,
    browser_mutation_gate: Option<&std::sync::RwLock<()>>,
) -> Value {
    browser_operation_call_with_controls(
        store,
        method,
        params,
        caller_webchat_control_grants,
        browser_actuator,
        BrowserOperationControls {
            mutation_gate: browser_mutation_gate,
            mutation_admission: None,
            caller_authorization: None,
        },
    )
}

fn browser_operation_call_with_controls(
    store: &std::sync::Arc<std::sync::Mutex<StateStore>>,
    method: &str,
    params: &Value,
    caller_webchat_control_grants: &[BrowserCallerGrant],
    browser_actuator: Option<&dyn BrowserActuator>,
    controls: BrowserOperationControls<'_>,
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
    let normalized_params;
    let params = if operation == BrowserOperation::SessionCreate
        && object.contains_key("source_url")
    {
        let Ok(store_guard) = store.lock() else {
            return json!({"ok": false, "code": "browser_operation_store_unavailable"});
        };
        normalized_params = match browser_session_create_params_from_source(&store_guard, params) {
            Ok(Some(value)) => value,
            Ok(None) => unreachable!(),
            Err(error) => return error,
        };
        &normalized_params
    } else {
        params
    };
    if let Err(error) = validate_browser_operation_params(operation, params) {
        return error;
    }

    // Serialize the authorization decision through browser actuation against
    // trusted local consent changes. If consent-off wins this lock, the mutation
    // observes it below; if the mutation wins, that already-authorized attempt
    // completes before the consent revision may change.
    let _mutation_gate = if operation.is_mutation() {
        match controls.mutation_gate {
            Some(gate) => match gate.read() {
                Ok(guard) => Some(guard),
                Err(_) => {
                    return json!({
                        "ok": false,
                        "code": "browser_mutation_gate_unavailable",
                    });
                }
            },
            None => None,
        }
    } else {
        None
    };

    let mutation_scope = if operation.is_mutation() {
        let Ok(store_guard) = store.lock() else {
            return json!({"ok": false, "code": "browser_operation_store_unavailable"});
        };
        if operation == BrowserOperation::SessionOpen {
            let session_ref = params.get("session_ref").and_then(Value::as_str).unwrap();
            if let Ok(Some(resource)) = store_guard.browser_resource(session_ref)
                && resource.provider != "chatgpt"
            {
                return json!({
                    "ok": false,
                    "code": "unsupported",
                    "operation": operation.method(),
                    "actuation_available": false,
                });
            }
        }
        let alpha4_supported = browser_operation_alpha4_supported(operation, params);
        match browser_operation_actuation_decision(
            &store_guard,
            operation,
            params,
            caller_webchat_control_grants,
        ) {
            Ok((true, None)) if alpha4_supported => {}
            Ok((true, None)) => {
                return json!({
                    "ok": false,
                    "code": "unsupported",
                    "operation": operation.method(),
                    "actuation_available": false,
                });
            }
            Ok((false, Some("capability_not_allowed"))) if !alpha4_supported => {
                return json!({
                    "ok": false,
                    "code": "unsupported",
                    "operation": operation.method(),
                    "actuation_available": false,
                });
            }
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
        match browser_operation_mutation_scope(&store_guard, operation, params) {
            Ok(scope) => Some(scope),
            Err(error) => return browser_store_error(error),
        }
    } else {
        None
    };

    let _mutation_permit = match (controls.mutation_admission, mutation_scope.as_ref()) {
        (Some(admission), Some(scope)) => match admission.reserve(scope) {
            Ok(Some(permit)) => Some(permit),
            Ok(None) => {
                return json!({
                    "ok": false,
                    "code": "browser_account_backpressure",
                    "retryable": true,
                    "resource_key": format!(
                        "{}:{}:{}",
                        scope.endpoint_ref, scope.provider, scope.account_ref
                    ),
                    "limit": 1,
                    "retry_after_ms": BROWSER_ACCOUNT_MUTATION_RETRY_AFTER_MS,
                });
            }
            Err(error) => return browser_store_error(error),
        },
        _ => None,
    };

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
        BrowserOperation::SessionCreate => browser_session_create(
            store,
            params,
            browser_actuator,
            controls.caller_authorization,
        ),
        BrowserOperation::SessionOpen => browser_session_open(store, params, browser_actuator),
        BrowserOperation::DispatchSubmit => browser_dispatch_submit(
            store,
            params,
            browser_actuator,
            controls.caller_authorization,
        ),
        BrowserOperation::DispatchStatus => {
            let dispatch_id = params.get("dispatch_id").and_then(Value::as_str).unwrap();
            browser_dispatch_status(store, dispatch_id, browser_actuator)
        }
        BrowserOperation::DispatchStop => browser_dispatch_stop(
            store,
            params,
            browser_actuator,
            controls.caller_authorization,
        ),
        _ => {
            let expected_generation = params
                .get("expected_generation")
                .and_then(Value::as_i64)
                .unwrap();
            let evidence = match browser_actuator {
                Some(actuator) => {
                    match actuator.actuate(operation.method(), params, expected_generation, None) {
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
            browser_operation_delivery_result(operation, delivery_state)
        }
    }
}

fn browser_operation_delivery_result(
    operation: BrowserOperation,
    delivery_state: BrowserDeliveryState,
) -> Value {
    let success = matches!(
        delivery_state,
        BrowserDeliveryState::Applied | BrowserDeliveryState::Stopped
    );
    json!({
        "ok": success,
        "code": if success { Value::Null } else { json!(delivery_state.as_str()) },
        "operation": operation.method(),
        "delivery_state": delivery_state.as_str(),
    })
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
            "message",
            "reasoning_effort",
            "required_apps",
            "expected_generation",
            "idempotency_key",
            "work_chain_id",
            "lane_id",
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
            browser_required_string(params, "message", 262_144)?;
            browser_required_reasoning_effort(params, true)?;
            browser_required_apps(params, true)?;
            browser_required_generation(params)?;
            browser_required_idempotency_key(params)?;
            let _ = browser_optional_string(params, "work_chain_id", 128)?;
            let _ = browser_optional_string(params, "lane_id", 160)?;
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
    if operation == BrowserOperation::SessionOpen && target.provider != "chatgpt" {
        return Ok((false, Some("capability_not_allowed")));
    }
    if decision != (true, None)
        || !matches!(
            operation,
            BrowserOperation::DispatchSubmit | BrowserOperation::SessionCreate
        )
    {
        return Ok(decision);
    }

    let Some(provider_state) =
        store.browser_provider_state(&target.endpoint_ref, &target.provider)?
    else {
        return Ok((false, Some("capability_unknown")));
    };
    if operation == BrowserOperation::SessionCreate {
        match browser_capability_snapshot_allows(
            &provider_state.capabilities_json,
            "composer.submit",
        ) {
            Some(true) => {}
            Some(false) => return Ok((false, Some("capability_not_allowed"))),
            None => return Ok((false, Some("capability_unknown"))),
        }
    }
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

fn browser_operation_mutation_scope(
    store: &StateStore,
    operation: BrowserOperation,
    params: &Value,
) -> Result<BrowserMutationScope, String> {
    let target = match operation {
        BrowserOperation::SpaceCreate | BrowserOperation::SessionCreate => {
            browser_operation_resource(
                store,
                params.get("account_ref").and_then(Value::as_str).unwrap(),
                "account",
            )?
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
            browser_operation_resource(store, &dispatch.target_session_ref, "session")?
        }
        BrowserOperation::SpaceInspect
        | BrowserOperation::SessionInspect
        | BrowserOperation::DispatchStatus => {
            return Err("browser_operation_not_mutating".to_owned());
        }
    };
    let account_ref = browser_resource_account_ref(store, &target)?;
    Ok(BrowserMutationScope {
        endpoint_ref: target.endpoint_ref,
        provider: target.provider,
        account_ref,
    })
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
    if provider_state.adapter_protocol_version != BROWSER_ADAPTER_PROTOCOL_VERSION {
        return Ok((false, Some("browser_adapter_protocol_unsupported")));
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

fn browser_public_capabilities(capabilities_json: &str) -> Value {
    let parsed = serde_json::from_str::<Value>(capabilities_json).unwrap_or_else(|_| json!({}));
    let operations = parsed
        .get("operations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    json!({
        "schema_version": 1,
        "operations": operations,
        "input_contract": {
            "message": {
                "accepted_modalities": ["text"],
                "max_bytes": 262_144,
                "control_characters": "rejected"
            },
            "attachments": {
                "supported": false,
                "reason": "not_exposed_by_browser_dispatch_contract"
            },
            "required_apps": {
                "max_items": 32,
                "item_max_bytes": 64
            },
            "reasoning_effort": {
                "values": ["economy", "balanced", "thorough"],
                "requires_operation": "composer.set_reasoning"
            }
        },
        "provider_dynamic_limits": {
            "message_bytes": {"status": "unknown"},
            "attachment_count": {"status": "unknown"},
            "turn_timeout_ms": {"status": "unknown"},
            "output_modalities": {"status": "unknown"},
            "model_effort_combinations": {"status": "unknown"}
        }
    })
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
            let account_ref = match browser_resource_account_ref(&store, &resource) {
                Ok(account_ref) => account_ref,
                Err(error) => return browser_store_error(error),
            };
            let provider_state =
                match store.browser_provider_state(&resource.endpoint_ref, &resource.provider) {
                    Ok(provider_state) => provider_state,
                    Err(error) => return browser_store_error(error),
                };
            let route_endpoint_ref = resource.endpoint_ref.clone();
            let route_provider = resource.provider.clone();
            let resource_observation_generation = resource.observation_generation;
            let route_status = match provider_state.as_ref() {
                None => "capability_unknown",
                Some(state) if state.observation_generation != resource.observation_generation => {
                    "stale_capability_generation"
                }
                Some(state)
                    if state.adapter_protocol_version != BROWSER_ADAPTER_PROTOCOL_VERSION =>
                {
                    "browser_adapter_protocol_unsupported"
                }
                Some(_) => "current",
            };
            let capabilities = provider_state
                .as_ref()
                .map(|state| browser_public_capabilities(&state.capabilities_json))
                .unwrap_or_else(|| json!({"status": "unknown"}));
            json!({
                "ok": true,
                "resource": browser_resource_json(resource),
                "route": {
                    "endpoint_ref": route_endpoint_ref,
                    "provider": route_provider,
                    "account_ref": account_ref,
                    "configuration_source": "browser_registry_observation",
                    "resource_observation_generation": resource_observation_generation,
                    "provider_observation_generation": provider_state.as_ref().map(|state| state.observation_generation),
                    "adapter_protocol_version": provider_state.as_ref().map(|state| state.adapter_protocol_version),
                    "provider_observed_at": provider_state.as_ref().map(|state| state.observed_at),
                    "status": route_status,
                },
                "capabilities": capabilities,
                "actuation_available": actuation_available,
                "actuation_reason": actuation_reason,
            })
        }
        Ok(Some(_)) | Ok(None) => json!({"ok": false, "code": "browser_resource_not_found"}),
        Err(error) => browser_store_error(error),
    }
}

fn browser_dispatch_result_is_durable(
    dispatch: &crate::state_store::BrowserDispatchRecord,
) -> bool {
    if dispatch.result_assistant_message_ref.is_none() || dispatch.result_settled_at.is_none() {
        return false;
    }
    match dispatch.work_chain_id {
        Some(_) => {
            dispatch.result_turn_message_id.is_some() && dispatch.result_evidence_id.is_some()
        }
        None => dispatch.result_turn_message_id.is_none() && dispatch.result_evidence_id.is_none(),
    }
}

/// Conservative beta.2 assignment projection derived from existing authorities.
/// `uncertain` delivery deliberately has no projected execution state: treating it
/// as queued or submitted would invent evidence and could make failover unsafe.
fn browser_dispatch_execution_state(
    dispatch: &crate::state_store::BrowserDispatchRecord,
) -> Option<&'static str> {
    if browser_dispatch_result_is_durable(dispatch) {
        return Some("settled");
    }
    match dispatch.delivery_state {
        BrowserDeliveryState::NotApplied => Some("queued"),
        BrowserDeliveryState::Applied => Some("submitted"),
        BrowserDeliveryState::Uncertain => None,
        BrowserDeliveryState::Rejected
        | BrowserDeliveryState::BrowserOffline
        | BrowserDeliveryState::ResourceUnavailable => Some("failed"),
        BrowserDeliveryState::Stopped => Some("stopped"),
    }
}

fn browser_dispatch_json(dispatch: crate::state_store::BrowserDispatchRecord) -> Value {
    let result_settled = browser_dispatch_result_is_durable(&dispatch);
    let execution_state = browser_dispatch_execution_state(&dispatch);
    json!({
        "dispatch_id": dispatch.dispatch_id,
        "endpoint_ref": dispatch.endpoint_ref,
        "provider": dispatch.provider,
        "operation": dispatch.operation,
        "target_session_ref": dispatch.target_session_ref,
        "reasoning_effort": dispatch.reasoning_effort,
        "required_apps": dispatch.required_apps,
        "expected_generation": dispatch.expected_generation,
        "parent_dispatch_id": dispatch.parent_dispatch_id,
        "authorization_principal_ref": dispatch.authorization_principal_ref,
        "authorization_connector_id": dispatch.authorization_connector_id,
        "authorization_grant_generation": dispatch.authorization_grant_generation,
        "delivery_state": dispatch.delivery_state.as_str(),
        "generation_owner": dispatch.generation_owner,
        "accepted_user_message_ref": dispatch.accepted_user_message_ref,
        "execution_state": execution_state,
        "result": {
            "settled": result_settled,
            "assistant_message_ref": dispatch.result_assistant_message_ref,
            "turn_message_id": dispatch.result_turn_message_id,
            "evidence_id": dispatch.result_evidence_id,
            "settled_at": dispatch.result_settled_at,
        },
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
                                "capabilities": browser_public_capabilities(&state.capabilities_json),
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
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(json!({"ok": false, "code": format!("browser_{field}_invalid")}));
    }
    if value.len() > max_bytes {
        return Err(json!({
            "ok": false,
            "code": format!("browser_{field}_invalid"),
            "limit": {
                "kind": "max_bytes",
                "max_bytes": max_bytes,
                "actual_bytes": value.len(),
            }
        }));
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

fn page_assist_call(
    params: &Value,
    caller_grants: &[PageAssistCallerGrant],
    browser_actuator: Option<&dyn BrowserActuator>,
) -> Value {
    let Some(object) = params.as_object() else {
        return json!({"ok": false, "code": "invalid_params", "message": "page assist params must be an object"});
    };
    const ALLOWED_KEYS: &[&str] = &[
        "endpoint_ref",
        "action",
        "target_origin",
        "tab_id",
        "max_chars",
        "generation",
        "ref",
        "value",
    ];
    if let Some(key) = object
        .keys()
        .find(|key| !ALLOWED_KEYS.contains(&key.as_str()))
    {
        return json!({"ok": false, "code": "invalid_params", "message": format!("unknown page assist parameter '{key}'")});
    }
    let endpoint_ref = match object
        .get("endpoint_ref")
        .and_then(Value::as_str)
        .map(str::trim)
    {
        Some(value)
            if !value.is_empty() && value.len() <= 96 && !value.chars().any(char::is_control) =>
        {
            value
        }
        _ => {
            return json!({"ok": false, "code": "invalid_params", "message": "endpoint_ref is required"});
        }
    };
    if !caller_grants
        .iter()
        .any(|grant| grant.endpoint_ref == endpoint_ref)
    {
        return json!({
            "ok": false,
            "code": "caller_grant_missing",
            "retryable": false,
            "delivery_state": "not_delivered",
        });
    }
    let action = match object.get("action").and_then(Value::as_str) {
        Some(value @ ("inspect" | "click" | "fill")) => value,
        _ => {
            return json!({"ok": false, "code": "invalid_params", "message": "action must be inspect, click, or fill"});
        }
    };
    let target_origin_raw = match object
        .get("target_origin")
        .and_then(Value::as_str)
        .map(str::trim)
    {
        Some(value) if !value.is_empty() && value.len() <= 4096 => value,
        _ => {
            return json!({"ok": false, "code": "invalid_params", "message": "target_origin is required"});
        }
    };
    let target_url = match url::Url::parse(target_origin_raw) {
        Ok(url)
            if matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.host_str().is_some() =>
        {
            url
        }
        _ => {
            return json!({"ok": false, "code": "invalid_params", "message": "target_origin must be a valid http or https origin"});
        }
    };
    let target_origin = target_url.origin().ascii_serialization();
    let tab_id = match object.get("tab_id") {
        None | Some(Value::Null) => None,
        Some(Value::Number(value)) => match value.as_i64() {
            Some(value) if value > 0 && value <= i64::from(i32::MAX) => Some(value),
            _ => {
                return json!({"ok": false, "code": "invalid_params", "message": "tab_id must be a positive integer"});
            }
        },
        Some(_) => {
            return json!({"ok": false, "code": "invalid_params", "message": "tab_id must be a positive integer"});
        }
    };
    let max_chars = match object.get("max_chars") {
        None | Some(Value::Null) => 16_384_u64,
        Some(Value::Number(value)) => match value.as_u64() {
            Some(value) if (1..=16_384).contains(&value) => value,
            _ => {
                return json!({"ok": false, "code": "invalid_params", "message": "max_chars must be between 1 and 16384"});
            }
        },
        Some(_) => {
            return json!({"ok": false, "code": "invalid_params", "message": "max_chars must be an integer"});
        }
    };
    let bounded_string = |key: &str, max_chars: usize| -> Result<String, Value> {
        match object.get(key).and_then(Value::as_str) {
            Some(value) if !value.is_empty() && value.chars().count() <= max_chars => {
                Ok(value.to_owned())
            }
            _ => Err(
                json!({"ok": false, "code": "invalid_params", "message": format!("{key} is required and must be at most {max_chars} characters")}),
            ),
        }
    };
    let mut bridge_params = json!({
        "endpoint_ref": endpoint_ref,
        "action": action,
        "target_origin": target_origin,
    });
    if let Some(tab_id) = tab_id {
        bridge_params["tab_id"] = json!(tab_id);
    }
    match action {
        "inspect" => {
            bridge_params["max_chars"] = json!(max_chars);
        }
        "click" => {
            let generation = match bounded_string("generation", 256) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let element_ref = match bounded_string("ref", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            bridge_params["generation"] = json!(generation);
            bridge_params["ref"] = json!(element_ref);
        }
        "fill" => {
            let generation = match bounded_string("generation", 256) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let element_ref = match bounded_string("ref", 512) {
                Ok(value) => value,
                Err(error) => return error,
            };
            let value = match object.get("value").and_then(Value::as_str) {
                Some(value) if value.chars().count() <= 10_000 => value.to_owned(),
                _ => {
                    return json!({"ok": false, "code": "invalid_params", "message": "value is required and must be at most 10000 characters"});
                }
            };
            bridge_params["generation"] = json!(generation);
            bridge_params["ref"] = json!(element_ref);
            bridge_params["value"] = json!(value);
        }
        _ => unreachable!(),
    }
    let Some(actuator) = browser_actuator else {
        return json!({
            "ok": false,
            "code": "page_assist_unavailable",
            "retryable": true,
            "delivery_state": "not_delivered",
        });
    };
    match actuator.actuate("herdr_mcp.page_assist", &bridge_params, 1, None) {
        Ok(evidence) => {
            if let Some(result) = evidence.result {
                return result;
            }
            if !evidence.browser_online || !evidence.command_accepted {
                return json!({
                    "ok": false,
                    "code": "page_assist_unavailable",
                    "retryable": true,
                    "delivery_state": "not_delivered",
                });
            }
            json!({
                "ok": false,
                "code": "page_assist_delivery_unknown",
                "retryable": false,
                "delivery_state": "delivery_unknown",
            })
        }
        Err(error) => json!({
            "ok": false,
            "code": "page_assist_unavailable",
            "message": error,
            "retryable": true,
            "delivery_state": "not_delivered",
        }),
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
                    "g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
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

        let url_exact = continuity_call(
            &store,
            "continuity.search",
            &json!({
                "conversation_url": "https://chatgpt.com/g/g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-project/c/conv-a?foo=bar#tail"
            }),
        );
        assert_eq!(url_exact["resolution"], "unique_exact");
        assert_eq!(url_exact["auto_resume_safe"], true);
        assert_eq!(url_exact["candidates"][0]["continuity_id"], "hc:alpha");

        let resumed_url = continuity_call(
            &store,
            "continuity.resume",
            &json!({"conversation_url": "https://chatgpt.com/g/g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-project/c/conv-a"}),
        );
        assert_eq!(resumed_url["ok"], true);
        assert_eq!(resumed_url["continuity_id"], "hc:alpha");

        let resumed_project = continuity_call(
            &store,
            "continuity.resume",
            &json!({"conversation_url": "https://chatgpt.com/g/g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-project/c/unseen-conv"}),
        );
        assert_eq!(resumed_project["ok"], true);
        assert_eq!(resumed_project["continuity_id"], "hc:alpha");

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
                    project_id: Some("g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
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
        assert_eq!(resumed["coverage"]["result_completeness"], "complete");
        assert_eq!(resumed["coverage"]["source_verification"], "unverified");
        assert_eq!(resumed["coverage"]["display_truncated"], false);
        assert!(
            resumed["coverage"]["limitations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "source_revision_unverified")
        );

        let tight_resume = work_memory_call(
            &store,
            "work_memory.resume",
            &json!({
                "project_ref": "project:herdr-mcp",
                "repo_id": "github.com/whshang/herdr-mcp",
                "work_chain_id": "wc_cccccccccccccccccccccccccccccccc",
                "max_turns": 1
            }),
        );
        assert_eq!(tight_resume["ok"], true);
        assert_eq!(tight_resume["turns"].as_array().unwrap().len(), 1);
        assert_eq!(tight_resume["coverage"]["result_completeness"], "complete");
        assert_eq!(tight_resume["coverage"]["display_truncated"], true);
        assert!(
            tight_resume["coverage"]["limitations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "display_page_truncated")
        );

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
        assert_eq!(searched["coverage"]["result_completeness"], "complete");
        assert_eq!(searched["coverage"]["source_verification"], "unverified");
    }

    #[test]
    fn work_memory_search_cursor_freezes_boundary_and_rejects_tampering() {
        use std::sync::{Arc, Mutex};

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let bound = work_memory_call(
            &store,
            "work_memory.bind",
            &json!({
                "continuity_id": "wm:cursor",
                "project_ref": "project:cursor",
                "repo_id": "github.com/whshang/herdr-mcp",
                "work_chain_id": "wc_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                "provider": "chatgpt",
                "session_ref": "cursor-session",
                "bound_at": 1,
            }),
        );
        assert_eq!(bound["ok"], true);

        for index in 1..=3 {
            let appended = work_memory_call(
                &store,
                "work_memory.append_evidence",
                &json!({
                    "continuity_id": "wm:cursor",
                    "kind": "result",
                    "content": format!("stable-cursor-keyword evidence-{index}"),
                    "created_at": 10 + index,
                }),
            );
            assert_eq!(appended["ok"], true);
        }

        let first = work_memory_call(
            &store,
            "work_memory.search",
            &json!({
                "project_ref": "project:cursor",
                "repo_id": "github.com/whshang/herdr-mcp",
                "work_chain_id": "wc_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                "query": "stable-cursor-keyword",
                "limit": 2
            }),
        );
        assert_eq!(first["ok"], true);
        assert_eq!(first["hits"].as_array().unwrap().len(), 2);
        assert_eq!(first["coverage"]["result_completeness"], "complete");
        assert_eq!(first["coverage"]["display_truncated"], true);
        let cursor = first["cursor"].as_str().unwrap().to_owned();

        let appended_later = work_memory_call(
            &store,
            "work_memory.append_evidence",
            &json!({
                "continuity_id": "wm:cursor",
                "kind": "result",
                "content": "stable-cursor-keyword evidence-4-later",
                "created_at": 20,
            }),
        );
        assert_eq!(appended_later["ok"], true);

        let continued = work_memory_call(&store, "work_memory.search", &json!({"cursor": cursor}));
        assert_eq!(continued["ok"], true);
        assert_eq!(continued["hits"].as_array().unwrap().len(), 1);
        assert!(
            continued["hits"][0]["excerpt"]
                .as_str()
                .unwrap()
                .contains("evidence-1")
        );
        assert!(!continued.to_string().contains("evidence-4-later"));
        assert!(continued["cursor"].is_null());

        let replay = work_memory_call(
            &store,
            "work_memory.search",
            &json!({"cursor": first["cursor"]}),
        );
        assert_eq!(replay, continued);

        let fresh = work_memory_call(
            &store,
            "work_memory.search",
            &json!({
                "project_ref": "project:cursor",
                "repo_id": "github.com/whshang/herdr-mcp",
                "work_chain_id": "wc_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                "query": "stable-cursor-keyword",
                "limit": 1
            }),
        );
        assert_eq!(fresh["ok"], true);
        assert!(
            fresh["hits"][0]["excerpt"]
                .as_str()
                .unwrap()
                .contains("evidence-4-later")
        );

        let conflict = work_memory_call(
            &store,
            "work_memory.search",
            &json!({"cursor": first["cursor"], "limit": 1}),
        );
        assert_eq!(conflict["code"], "work_memory_cursor_conflict");

        let mut tampered = first["cursor"].as_str().unwrap().to_owned();
        let last = tampered.pop().unwrap();
        tampered.push(if last == '0' { '1' } else { '0' });
        let tampered_result =
            work_memory_call(&store, "work_memory.search", &json!({"cursor": tampered}));
        assert_eq!(tampered_result["code"], "work_memory_cursor_invalid");
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
        assert_eq!(
            inspected["provider_states"][0]["capabilities"]["schema_version"],
            1
        );
        assert_eq!(
            inspected["provider_states"][0]["capabilities"]["operations"][0],
            "identity.inspect"
        );
        assert_eq!(
            inspected["provider_states"][0]["capabilities"]["input_contract"]["message"]["max_bytes"],
            262_144
        );
        assert_eq!(
            inspected["provider_states"][0]["capabilities"]["provider_dynamic_limits"]["turn_timeout_ms"]
                ["status"],
            "unknown"
        );

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

        let oversized = browser_operation_call(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &json!({
                "session_ref": session_ref,
                "message": "x".repeat(262_145),
                "expected_generation": 7,
                "idempotency_key": "dispatch-oversized-1"
            }),
        );
        assert_eq!(oversized["code"], "browser_message_invalid");
        assert_eq!(oversized["limit"]["kind"], "max_bytes");
        assert_eq!(oversized["limit"]["max_bytes"], 262_144);
        assert_eq!(oversized["limit"]["actual_bytes"], 262_145);
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
            BrowserDeliveryState::Uncertain
        );

        evidence.result = Some(json!({
            "accepted_user_message_ref": "provider-user-1"
        }));
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

        let mut create_evidence = BrowserPostconditionEvidence::resource_unavailable(7);
        create_evidence.resource_available = true;
        create_evidence.command_accepted = true;
        create_evidence.stable_resource_ref_observed = true;
        create_evidence.lifecycle_observed = true;
        create_evidence.canonical_url_observed = true;
        create_evidence.accepted_message_observed = true;
        create_evidence.result = Some(json!({
            "accepted_user_message_ref": "provider-create-user-1"
        }));
        assert_eq!(
            browser_delivery_state_from_postcondition(
                BrowserOperation::SessionCreate,
                &params,
                7,
                &create_evidence,
            )
            .unwrap(),
            BrowserDeliveryState::Applied,
            "exact provider acceptance must not depend on assistant-start timing"
        );
        create_evidence.result = None;
        assert_eq!(
            browser_delivery_state_from_postcondition(
                BrowserOperation::SessionCreate,
                &params,
                7,
                &create_evidence,
            )
            .unwrap(),
            BrowserDeliveryState::Uncertain,
            "session materialization without exact provider acceptance stays uncertain"
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
    fn browser_operation_result_reports_only_proven_terminal_success() {
        let applied = browser_operation_delivery_result(
            BrowserOperation::MessageAppend,
            BrowserDeliveryState::Applied,
        );
        assert_eq!(applied["ok"], true);
        assert!(applied["code"].is_null());
        assert_eq!(applied["delivery_state"], "applied");

        let stopped = browser_operation_delivery_result(
            BrowserOperation::DispatchStop,
            BrowserDeliveryState::Stopped,
        );
        assert_eq!(stopped["ok"], true);
        assert!(stopped["code"].is_null());
        assert_eq!(stopped["delivery_state"], "stopped");

        let uncertain = browser_operation_delivery_result(
            BrowserOperation::MessageAppend,
            BrowserDeliveryState::Uncertain,
        );
        assert_eq!(uncertain["ok"], false);
        assert_eq!(uncertain["code"], "uncertain");
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
                    capabilities_json: r#"{"operations":["composer.submit","generation.stop"]}"#,
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
            let space = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "space",
                    parent_ref: Some(&account.resource_ref),
                    native_identity: "session-create-space",
                    display_label: Some("Project A"),
                    observation_generation: 7,
                    observed_at: 12,
                })
                .unwrap();
            guard
                .upsert_browser_resource_locator(
                    &space.resource_ref,
                    "https://chatgpt.com/g/g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/project",
                    7,
                    12,
                )
                .unwrap();
            let source = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "session",
                    parent_ref: Some(&space.resource_ref),
                    native_identity: "session-create-source",
                    display_label: Some("Source"),
                    observation_generation: 7,
                    observed_at: 12,
                })
                .unwrap();
            let source_url =
                "https://chatgpt.com/g/g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/c/source-conv";
            guard
                .upsert_browser_resource_locator(&source.resource_ref, source_url, 7, 12)
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
        assert_eq!(status["dispatch"]["execution_state"], "failed");
        assert!(!status.to_string().contains("delivery-session-hidden"));

        struct UncertainThenReconcileActuator {
            reconcile_calls: std::sync::atomic::AtomicUsize,
        }
        impl BrowserActuator for UncertainThenReconcileActuator {
            fn actuate(
                &self,
                _operation: &str,
                _params: &Value,
                expected_generation: i64,
                dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                assert!(dispatch_id.is_some());
                let mut evidence =
                    BrowserPostconditionEvidence::resource_unavailable(expected_generation);
                evidence.command_accepted = true;
                evidence.resource_available = true;
                Ok(evidence)
            }

            fn reconcile_dispatch(
                &self,
                dispatch_id: &str,
                expected_generation: i64,
            ) -> Result<Option<BrowserPostconditionEvidence>, String> {
                assert!(dispatch_id.starts_with("bd_"));
                self.reconcile_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(BrowserPostconditionEvidence {
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
                    result: Some(json!({
                        "accepted_user_message_ref": "provider-user-delayed"
                    })),
                }))
            }
        }
        let reconcile_actuator = UncertainThenReconcileActuator {
            reconcile_calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let reconcile_params = json!({
            "session_ref": session_ref,
            "message": "dispatch with delayed evidence",
            "expected_generation": 7,
            "idempotency_key": "delivery-reconcile-idempotency-key"
        });
        let uncertain = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &reconcile_params,
            true,
            Some(&reconcile_actuator),
        );
        assert_eq!(uncertain["ok"], false);
        assert_eq!(uncertain["code"], "uncertain");
        assert_eq!(uncertain["dispatch"]["delivery_state"], "uncertain");
        assert!(uncertain["dispatch"]["execution_state"].is_null());
        let reconcile_dispatch_id = uncertain["dispatch"]["dispatch_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let reconciled = browser_operation_call_with_grants(
            &store,
            "herdr_mcp.browser_dispatch.status",
            &json!({"dispatch_id": reconcile_dispatch_id}),
            &[],
            Some(&reconcile_actuator),
            None,
        );
        assert_eq!(reconciled["ok"], true);
        assert_eq!(reconciled["reconciled"], true);
        assert_eq!(reconciled["dispatch"]["delivery_state"], "applied");
        assert_eq!(
            reconcile_actuator
                .reconcile_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let settled_status = browser_operation_call_with_grants(
            &store,
            "herdr_mcp.browser_dispatch.status",
            &json!({"dispatch_id": reconcile_dispatch_id}),
            &[],
            Some(&reconcile_actuator),
            None,
        );
        assert_eq!(settled_status["dispatch"]["delivery_state"], "applied");
        assert_eq!(settled_status["reconciled"], false);
        assert_eq!(
            reconcile_actuator
                .reconcile_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "terminal status must not consume or request a second reconciliation"
        );

        struct ConcurrentSettlementActuator {
            store: Arc<Mutex<StateStore>>,
        }
        impl BrowserActuator for ConcurrentSettlementActuator {
            fn actuate(
                &self,
                _operation: &str,
                _params: &Value,
                expected_generation: i64,
                dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                let dispatch_id = dispatch_id.expect("dispatch identity is required");
                self.store
                    .lock()
                    .unwrap()
                    .settle_uncertain_browser_dispatch(BrowserDispatchUpdateInput {
                        dispatch_id,
                        expected_generation,
                        delivery_state: BrowserDeliveryState::Applied,
                        generation_owner: Some(expected_generation),
                        accepted_user_message_ref: Some("provider-user-race"),
                        updated_at: browser_epoch_ms(),
                    })
                    .unwrap();
                let mut evidence =
                    BrowserPostconditionEvidence::resource_unavailable(expected_generation);
                evidence.command_accepted = true;
                evidence.resource_available = true;
                Ok(evidence)
            }
        }
        let settlement_race_params = json!({
            "session_ref": session_ref,
            "message": "status settlement wins before submit timeout writeback",
            "expected_generation": 7,
            "idempotency_key": "delivery-settlement-race-idempotency-key"
        });
        let settlement_race = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &settlement_race_params,
            true,
            Some(&ConcurrentSettlementActuator {
                store: store.clone(),
            }),
        );
        assert_eq!(settlement_race["ok"], true);
        assert!(settlement_race["code"].is_null());
        assert_eq!(settlement_race["dispatch"]["delivery_state"], "applied");

        struct AppliedActuator;
        impl BrowserActuator for AppliedActuator {
            fn actuate(
                &self,
                _operation: &str,
                _params: &Value,
                expected_generation: i64,
                _dispatch_id: Option<&str>,
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
                    result: Some(json!({
                        "accepted_user_message_ref": "provider-user-applied"
                    })),
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

        struct StopActuator {
            calls: std::sync::atomic::AtomicUsize,
        }
        impl BrowserActuator for StopActuator {
            fn actuate(
                &self,
                operation: &str,
                _params: &Value,
                expected_generation: i64,
                dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                assert_eq!(operation, BrowserOperation::DispatchStop.method());
                assert!(dispatch_id.is_some_and(|value| value.starts_with("bd_")));
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(BrowserPostconditionEvidence {
                    observed_generation: expected_generation,
                    command_accepted: true,
                    browser_online: true,
                    resource_available: true,
                    rejected: false,
                    stable_resource_ref_observed: true,
                    lifecycle_observed: true,
                    canonical_url_observed: true,
                    accepted_message_observed: false,
                    message_baseline_advanced: false,
                    reasoning_effort_readback: None,
                    required_apps_readback: Vec::new(),
                    generation_owner: Some(expected_generation),
                    generation_status_observed: true,
                    generation_stopped: true,
                    result: None,
                })
            }
        }
        let stop_actuator = StopActuator {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let stop_params = json!({
            "dispatch_id": applied_dispatch_id,
            "expected_generation": 7,
            "idempotency_key": "stop-applied-dispatch-1"
        });
        let stopped = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.stop",
            &stop_params,
            true,
            Some(&stop_actuator),
        );
        assert_eq!(stopped["ok"], true);
        assert_eq!(stopped["delivery_state"], "stopped");
        assert_eq!(
            stopped["dispatch"]["parent_dispatch_id"],
            applied_dispatch_id
        );
        assert_eq!(stopped["target_dispatch"]["delivery_state"], "stopped");
        assert_eq!(
            stop_actuator
                .calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );

        let stopped_replay = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.stop",
            &stop_params,
            true,
            Some(&stop_actuator),
        );
        assert_eq!(stopped_replay["ok"], true);
        assert_eq!(stopped_replay["replayed"], true);
        assert_eq!(
            stop_actuator
                .calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "replaying a confirmed stop must not click Stop again"
        );

        let second_params = json!({
            "session_ref": session_ref,
            "message": "dispatch before delayed stop",
            "expected_generation": 7,
            "idempotency_key": "delivery-before-delayed-stop",
            "work_chain_id": "wc_dddddddddddddddddddddddddddddddd",
            "lane_id": "lane-browser"
        });
        let second = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &second_params,
            true,
            Some(&AppliedActuator),
        );
        assert_eq!(second["dispatch"]["delivery_state"], "applied");
        let second_dispatch_id = second["dispatch"]["dispatch_id"]
            .as_str()
            .unwrap()
            .to_owned();

        struct DelayedStopActuator {
            actuate_calls: std::sync::atomic::AtomicUsize,
            reconcile_calls: std::sync::atomic::AtomicUsize,
        }
        impl BrowserActuator for DelayedStopActuator {
            fn actuate(
                &self,
                operation: &str,
                _params: &Value,
                expected_generation: i64,
                dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                assert_eq!(operation, BrowserOperation::DispatchStop.method());
                assert!(dispatch_id.is_some_and(|value| value.starts_with("bd_")));
                self.actuate_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(BrowserPostconditionEvidence {
                    observed_generation: expected_generation,
                    command_accepted: true,
                    browser_online: true,
                    resource_available: true,
                    rejected: false,
                    stable_resource_ref_observed: true,
                    lifecycle_observed: true,
                    canonical_url_observed: true,
                    accepted_message_observed: false,
                    message_baseline_advanced: false,
                    reasoning_effort_readback: None,
                    required_apps_readback: Vec::new(),
                    generation_owner: Some(expected_generation),
                    generation_status_observed: true,
                    generation_stopped: false,
                    result: None,
                })
            }

            fn reconcile_dispatch(
                &self,
                dispatch_id: &str,
                expected_generation: i64,
            ) -> Result<Option<BrowserPostconditionEvidence>, String> {
                assert!(dispatch_id.starts_with("bd_"));
                self.reconcile_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(BrowserPostconditionEvidence {
                    observed_generation: expected_generation,
                    command_accepted: true,
                    browser_online: true,
                    resource_available: true,
                    rejected: false,
                    stable_resource_ref_observed: true,
                    lifecycle_observed: true,
                    canonical_url_observed: true,
                    accepted_message_observed: false,
                    message_baseline_advanced: false,
                    reasoning_effort_readback: None,
                    required_apps_readback: Vec::new(),
                    generation_owner: Some(expected_generation),
                    generation_status_observed: true,
                    generation_stopped: true,
                    result: None,
                }))
            }
        }
        let delayed_stop_actuator = DelayedStopActuator {
            actuate_calls: std::sync::atomic::AtomicUsize::new(0),
            reconcile_calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let delayed_stop_params = json!({
            "dispatch_id": second_dispatch_id,
            "expected_generation": 7,
            "idempotency_key": "stop-delayed-dispatch-1"
        });
        let uncertain_stop = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.stop",
            &delayed_stop_params,
            true,
            Some(&delayed_stop_actuator),
        );
        assert_eq!(uncertain_stop["ok"], false);
        assert_eq!(uncertain_stop["delivery_state"], "uncertain");
        let stop_dispatch_id = uncertain_stop["dispatch"]["dispatch_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let uncertain_replay = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.stop",
            &delayed_stop_params,
            true,
            Some(&delayed_stop_actuator),
        );
        assert_eq!(uncertain_replay["replayed"], true);
        assert_eq!(uncertain_replay["delivery_state"], "uncertain");
        assert_eq!(
            delayed_stop_actuator
                .actuate_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "an uncertain stop replay must not send a second Stop command"
        );

        let reconciled_stop = browser_operation_call_with_grants(
            &store,
            "herdr_mcp.browser_dispatch.status",
            &json!({"dispatch_id": stop_dispatch_id}),
            &[],
            Some(&delayed_stop_actuator),
            None,
        );
        assert_eq!(reconciled_stop["reconciled"], true);
        assert_eq!(reconciled_stop["dispatch"]["delivery_state"], "stopped");
        assert_eq!(
            reconciled_stop["target_dispatch"]["delivery_state"],
            "stopped"
        );
        assert_eq!(
            delayed_stop_actuator
                .reconcile_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[test]
    fn browser_evidence_accepted_user_message_ref_is_narrowly_validated() {
        let mut evidence = BrowserPostconditionEvidence::resource_unavailable(7);
        assert_eq!(
            browser_evidence_accepted_user_message_ref(&evidence).unwrap(),
            None
        );
        evidence.result = Some(json!({"accepted_user_message_ref": null}));
        assert_eq!(
            browser_evidence_accepted_user_message_ref(&evidence).unwrap(),
            None
        );
        evidence.result = Some(json!({"accepted_user_message_ref": "user-1"}));
        assert_eq!(
            browser_evidence_accepted_user_message_ref(&evidence)
                .unwrap()
                .as_deref(),
            Some("user-1")
        );
        evidence.result = Some(json!({"accepted_user_message_ref": ""}));
        assert_eq!(
            browser_evidence_accepted_user_message_ref(&evidence).unwrap_err(),
            "browser_evidence_accepted_user_message_invalid"
        );
        evidence.result = Some(json!({"accepted_user_message_ref": 7}));
        assert_eq!(
            browser_evidence_accepted_user_message_ref(&evidence).unwrap_err(),
            "browser_evidence_accepted_user_message_invalid"
        );
    }

    #[test]
    fn browser_dispatch_result_settlement_projects_exact_worker_result() {
        use crate::state_store::{
            BrowserDispatchResultInput, BrowserEndpointConsentInput,
            BrowserEndpointRegistrationInput, BrowserProviderObservationInput,
            BrowserResourceObservationInput, WorkMemoryBindingInput,
        };
        use std::sync::{Arc, Mutex};

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let session_ref = {
            let mut guard = store.lock().unwrap();
            let endpoint = guard
                .register_browser_endpoint(BrowserEndpointRegistrationInput {
                    device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    profile_seed: "result-settlement-profile-seed",
                    browser_family: "chrome",
                    extension_version: "0.1.91",
                    observed_at: 10,
                })
                .unwrap();
            guard
                .observe_browser_provider(BrowserProviderObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    adapter_protocol_version: 1,
                    observation_generation: 7,
                    capabilities_json: r#"{"operations":["composer.submit","generation.status"]}"#,
                    observed_at: 11,
                })
                .unwrap();
            let account = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "account",
                    parent_ref: None,
                    native_identity: "result-settlement-account",
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
                    native_identity: "result-settlement-session",
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
            guard
                .bind_work_memory(WorkMemoryBindingInput {
                    continuity_id: "wm:result-settle",
                    project_ref: "project:result-settle",
                    repo_id: "github.com/whshang/herdr-mcp",
                    work_chain_id: "wc_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                    provider: "chatgpt",
                    account_ref: None,
                    space_ref: None,
                    session_ref: &session.resource_ref,
                    bound_at: 15,
                })
                .unwrap();
            session.resource_ref
        };

        struct AppliedWithIdentityActuator;
        impl BrowserActuator for AppliedWithIdentityActuator {
            fn actuate(
                &self,
                _operation: &str,
                _params: &Value,
                expected_generation: i64,
                _dispatch_id: Option<&str>,
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
                    result: Some(json!({"accepted_user_message_ref": "provider-user-1"})),
                })
            }
        }

        let submitted = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &json!({
                "session_ref": session_ref,
                "message": "bounded worker assignment",
                "expected_generation": 7,
                "idempotency_key": "result-settlement-key",
                "work_chain_id": "wc_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                "lane_id": "lane-result"
            }),
            true,
            Some(&AppliedWithIdentityActuator),
        );
        assert_eq!(submitted["ok"], true);
        assert_eq!(
            submitted["dispatch"]["accepted_user_message_ref"],
            "provider-user-1"
        );
        assert_eq!(submitted["dispatch"]["result"]["settled"], false);
        assert_eq!(submitted["dispatch"]["execution_state"], "submitted");
        let dispatch_id = submitted["dispatch"]["dispatch_id"]
            .as_str()
            .unwrap()
            .to_owned();

        {
            let mut guard = store.lock().unwrap();
            let settled = guard
                .settle_browser_dispatch_result(BrowserDispatchResultInput {
                    provider: "chatgpt",
                    session_ref: &session_ref,
                    expected_generation: 7,
                    accepted_user_message_ref: "provider-user-1",
                    assistant_message_ref: "provider-assistant-1",
                    assistant_text: "final worker answer text",
                    observed_at: 20,
                })
                .unwrap();
            assert!(!settled.replayed);
            assert_eq!(settled.dispatch.dispatch_id, dispatch_id);
        }

        let status = browser_operation_call(
            &store,
            "herdr_mcp.browser_dispatch.status",
            &json!({"dispatch_id": dispatch_id}),
        );
        assert_eq!(status["ok"], true);
        assert_eq!(status["dispatch"]["result"]["settled"], true);
        assert_eq!(status["dispatch"]["execution_state"], "settled");
        assert_eq!(
            status["dispatch"]["result"]["assistant_message_ref"],
            "provider-assistant-1"
        );
        assert!(
            status["dispatch"]["result"]["evidence_id"]
                .as_str()
                .unwrap()
                .starts_with("ev_")
        );
        assert!(
            !status["dispatch"]["result"]["turn_message_id"]
                .as_str()
                .unwrap()
                .is_empty()
        );
        // The worker text lives in Work Memory, not in the planner-facing status.
        assert!(!status.to_string().contains("final worker answer text"));
    }

    #[test]
    fn browser_dispatch_idempotency_survives_store_reopen_without_second_actuation() {
        use crate::state_store::{
            BrowserEndpointConsentInput, BrowserEndpointRegistrationInput,
            BrowserProviderObservationInput, BrowserResourceObservationInput,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        struct CountingActuator {
            calls: Arc<AtomicUsize>,
        }

        impl BrowserActuator for CountingActuator {
            fn actuate(
                &self,
                _operation: &str,
                _params: &Value,
                expected_generation: i64,
                _dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                self.calls.fetch_add(1, Ordering::SeqCst);
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
                    result: Some(json!({
                        "accepted_user_message_ref": "provider-user-restart"
                    })),
                })
            }
        }

        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "herdr-mcp-browser-dispatch-restart-{}-{suffix}.sqlite",
            std::process::id()
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let actuator = CountingActuator {
            calls: calls.clone(),
        };

        let session_ref;
        let first_dispatch_id;
        {
            let store = Arc::new(Mutex::new(StateStore::open(&path).unwrap()));
            session_ref = {
                let mut guard = store.lock().unwrap();
                let endpoint = guard
                    .register_browser_endpoint(BrowserEndpointRegistrationInput {
                        device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                        profile_seed: "restart-idempotency-profile",
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
                        native_identity: "restart-account-hidden",
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
                        native_identity: "restart-session-hidden",
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
                "message": "persisted dispatch before runtime restart",
                "expected_generation": 7,
                "idempotency_key": "runtime-restart-idempotency-key"
            });
            let first = browser_operation_call_with_grant(
                &store,
                "herdr_mcp.browser_dispatch.submit",
                &params,
                true,
                Some(&actuator),
            );
            assert_eq!(first["ok"], true);
            assert_eq!(first["replayed"], false);
            first_dispatch_id = first["dispatch"]["dispatch_id"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }

        {
            let reopened = Arc::new(Mutex::new(StateStore::open(&path).unwrap()));
            let params = json!({
                "session_ref": session_ref,
                "message": "persisted dispatch before runtime restart",
                "expected_generation": 7,
                "idempotency_key": "runtime-restart-idempotency-key"
            });
            let replay = browser_operation_call_with_grant(
                &reopened,
                "herdr_mcp.browser_dispatch.submit",
                &params,
                true,
                Some(&actuator),
            );
            assert_eq!(replay["ok"], true);
            assert_eq!(replay["replayed"], true);
            assert_eq!(replay["dispatch"]["dispatch_id"], first_dispatch_id);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "replaying the durable idempotency key after reopening the runtime store must not invoke browser actuation"
            );

            let control = browser_operation_call_with_grant(
                &reopened,
                "herdr_mcp.browser_dispatch.submit",
                &json!({
                    "session_ref": session_ref,
                    "message": "new dispatch after runtime restart",
                    "expected_generation": 7,
                    "idempotency_key": "runtime-restart-control-key"
                }),
                true,
                Some(&actuator),
            );
            assert_eq!(control["ok"], true);
            assert_eq!(control["replayed"], false);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                2,
                "the reopened runtime still invokes the actuator for a genuinely new idempotency key"
            );
        }

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(path.with_extension("sqlite-wal")).ok();
        std::fs::remove_file(path.with_extension("sqlite-shm")).ok();
    }

    #[test]
    fn beta2_browser_mutation_support_matrix_is_frozen() {
        let cases = [
            (
                "herdr_mcp.browser_space.create",
                json!({
                    "endpoint_ref": "be_alpha4",
                    "provider": "chatgpt",
                    "account_ref": "br_account",
                    "display_label": "Project",
                    "expected_generation": 7,
                    "idempotency_key": "unsupported-space-create"
                }),
            ),
            (
                "herdr_mcp.browser_space.open",
                json!({
                    "space_ref": "br_space",
                    "expected_generation": 7,
                    "idempotency_key": "unsupported-space-open"
                }),
            ),
            (
                "herdr_mcp.browser_message.append",
                json!({
                    "session_ref": "br_session",
                    "message": "append without submit",
                    "expected_generation": 7,
                    "idempotency_key": "unsupported-message-append"
                }),
            ),
            (
                "herdr_mcp.browser_composer.set_reasoning",
                json!({
                    "session_ref": "br_session",
                    "reasoning_effort": "balanced",
                    "expected_generation": 7,
                    "idempotency_key": "unsupported-reasoning"
                }),
            ),
            (
                "herdr_mcp.browser_composer.set_apps",
                json!({
                    "session_ref": "br_session",
                    "required_apps": ["herdr"],
                    "expected_generation": 7,
                    "idempotency_key": "unsupported-apps"
                }),
            ),
            (
                "herdr_mcp.browser_dispatch.submit",
                json!({
                    "session_ref": "br_session",
                    "message": "submit with reasoning",
                    "reasoning_effort": "balanced",
                    "expected_generation": 7,
                    "idempotency_key": "unsupported-submit-reasoning"
                }),
            ),
            (
                "herdr_mcp.browser_dispatch.submit",
                json!({
                    "session_ref": "br_session",
                    "message": "submit with app",
                    "required_apps": ["herdr"],
                    "expected_generation": 7,
                    "idempotency_key": "unsupported-submit-apps"
                }),
            ),
        ];

        for (method, params) in cases {
            let operation = BrowserOperation::parse(method).unwrap();
            assert!(
                !browser_operation_alpha4_supported(operation, &params),
                "method={method} must stay explicit unsupported in Alpha 4"
            );
        }

        assert!(browser_operation_alpha4_supported(
            BrowserOperation::DispatchSubmit,
            &json!({
                "session_ref": "br_session",
                "message": "plain dispatch",
                "expected_generation": 7,
                "idempotency_key": "supported-plain-dispatch"
            })
        ));
        assert!(browser_operation_alpha4_supported(
            BrowserOperation::SessionOpen,
            &json!({
                "session_ref": "br_session",
                "expected_generation": 7,
                "idempotency_key": "supported-session-open"
            })
        ));
        assert!(browser_operation_alpha4_supported(
            BrowserOperation::DispatchStop,
            &json!({
                "dispatch_id": "bd_alpha4",
                "expected_generation": 7,
                "idempotency_key": "supported-stop"
            })
        ));
        assert!(browser_operation_alpha4_supported(
            BrowserOperation::SessionCreate,
            &json!({
                "endpoint_ref": "be_alpha4",
                "provider": "chatgpt",
                "account_ref": "br_account",
                "display_label": "Conversation",
                "message": "first assignment",
                "expected_generation": 7,
                "idempotency_key": "supported-session-create"
            })
        ));
        assert!(browser_operation_alpha4_supported(
            BrowserOperation::SessionCreate,
            &json!({
                "endpoint_ref": "be_alpha4",
                "provider": "chatgpt",
                "account_ref": "br_account",
                "display_label": "Conversation",
                "message": "first assignment with app",
                "required_apps": ["herdr"],
                "expected_generation": 7,
                "idempotency_key": "supported-session-create-app"
            })
        ));
    }

    #[test]
    fn browser_session_open_unique_and_durable_idempotency_regression() {
        use crate::state_store::{
            BrowserEndpointConsentInput, BrowserEndpointRegistrationInput,
            BrowserProviderObservationInput, BrowserResourceObservationInput,
        };
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        };

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (session_ref, other_session_ref) = {
            let mut guard = store.lock().unwrap();
            let endpoint = guard
                .register_browser_endpoint(BrowserEndpointRegistrationInput {
                    device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    profile_seed: "session-open-unique-profile",
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
                    capabilities_json: r#"{"operations":["session.open","session.inspect"]}"#,
                    observed_at: 11,
                })
                .unwrap();
            let account = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "account",
                    parent_ref: None,
                    native_identity: "account-hidden-unique",
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
                    native_identity: "session-hidden-unique",
                    display_label: None,
                    observation_generation: 7,
                    observed_at: 13,
                })
                .unwrap();
            let other_session = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "session",
                    parent_ref: Some(&account.resource_ref),
                    native_identity: "session-hidden-unique-2",
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
            (session.resource_ref, other_session.resource_ref)
        };

        struct SessionOpenActuator {
            calls: AtomicUsize,
            expected_session_ref: String,
        }
        impl BrowserActuator for SessionOpenActuator {
            fn actuate(
                &self,
                operation: &str,
                params: &Value,
                expected_generation: i64,
                dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                assert_eq!(operation, "herdr_mcp.browser_session.open");
                assert_eq!(expected_generation, 7);
                assert!(dispatch_id.is_none());
                // Must never carry a message payload; only opaque refs + generation + idempotency.
                assert!(params.get("message").is_none());
                assert!(params.get("selector").is_none());
                assert_eq!(
                    params.get("session_ref").and_then(Value::as_str).unwrap(),
                    self.expected_session_ref
                );
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(BrowserPostconditionEvidence {
                    observed_generation: 7,
                    command_accepted: true,
                    browser_online: true,
                    resource_available: true,
                    rejected: false,
                    stable_resource_ref_observed: true,
                    lifecycle_observed: true,
                    canonical_url_observed: true,
                    accepted_message_observed: false,
                    message_baseline_advanced: false,
                    reasoning_effort_readback: None,
                    required_apps_readback: Vec::new(),
                    generation_owner: None,
                    generation_status_observed: false,
                    generation_stopped: false,
                    result: None,
                })
            }
        }

        let actuator = SessionOpenActuator {
            calls: AtomicUsize::new(0),
            expected_session_ref: session_ref.clone(),
        };
        let params = json!({
            "session_ref": session_ref,
            "expected_generation": 7,
            "idempotency_key": "session-open-unique-1"
        });
        let first = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.open",
            &params,
            true,
            Some(&actuator),
        );
        assert_eq!(first["ok"], true);
        assert_eq!(first["delivery_state"], "applied");
        assert_eq!(first["operation"], "herdr_mcp.browser_session.open");
        assert_eq!(first["idempotent_replay"], false);
        assert_eq!(actuator.calls.load(Ordering::SeqCst), 1);

        // Same key + same request replays without a second tab activation.
        let replay = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.open",
            &params,
            true,
            Some(&actuator),
        );
        assert_eq!(replay["ok"], true);
        assert_eq!(replay["delivery_state"], "applied");
        assert_eq!(replay["idempotent_replay"], true);
        assert_eq!(replay["op_id"], first["op_id"]);
        assert_eq!(
            actuator.calls.load(Ordering::SeqCst),
            1,
            "idempotent replay must not invoke browser actuation again"
        );

        // Same key + different request conflicts (different session_ref with same key).
        let conflict2 = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.open",
            &json!({
                "session_ref": other_session_ref,
                "expected_generation": 7,
                "idempotency_key": "session-open-unique-1"
            }),
            true,
            Some(&actuator),
        );
        assert_eq!(conflict2["code"], "idempotency_key_conflict");
        assert_eq!(actuator.calls.load(Ordering::SeqCst), 1);

        // Pending/uncertain must not auto-repeat.
        {
            let mut guard = store.lock().unwrap();
            let pending_digest = browser_sha256("session-open-pending-key");
            let pending_request = browser_sha256(&json!({"operation":"herdr_mcp.browser_session.open","session_ref":session_ref,"expected_generation":7}).to_string());
            let pending_op_id = format!("op:browser_session_open:{}", &pending_digest[..32]);
            let now = browser_epoch_ms();
            let expires = now + 10 * 60 * 1000;
            guard
                .reserve_operation(
                    "browser_session.open",
                    &pending_digest,
                    &pending_request,
                    &pending_op_id,
                    now,
                    expires,
                )
                .unwrap();
        }
        let in_flight = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.open",
            &json!({
                "session_ref": session_ref,
                "expected_generation": 7,
                "idempotency_key": "session-open-pending-key"
            }),
            true,
            Some(&actuator),
        );
        assert_eq!(in_flight["code"], "idempotency_in_flight");
        assert_eq!(actuator.calls.load(Ordering::SeqCst), 1);

        // Ensure no message was ever submitted via the session.open path.
        assert_eq!(replay["idempotent_replay"], true);
    }

    #[test]
    fn browser_session_open_fails_closed_on_missing_stale_and_non_chatgpt() {
        use crate::state_store::{
            BrowserEndpointConsentInput, BrowserEndpointRegistrationInput,
            BrowserProviderObservationInput, BrowserResourceObservationInput,
        };
        use std::sync::{Arc, Mutex};

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (endpoint_ref, session_ref) = {
            let mut guard = store.lock().unwrap();
            let endpoint = guard
                .register_browser_endpoint(BrowserEndpointRegistrationInput {
                    device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    profile_seed: "session-open-failclosed-profile",
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
                    capabilities_json: r#"{"operations":["session.open","session.inspect"]}"#,
                    observed_at: 11,
                })
                .unwrap();
            let account = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "account",
                    parent_ref: None,
                    native_identity: "account-failclosed",
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
                    native_identity: "session-failclosed",
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
            (endpoint.endpoint_ref.clone(), session.resource_ref.clone())
        };

        struct NoopActuator;
        impl BrowserActuator for NoopActuator {
            fn actuate(
                &self,
                _operation: &str,
                _params: &Value,
                expected_generation: i64,
                _dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                Ok(BrowserPostconditionEvidence::resource_unavailable(
                    expected_generation,
                ))
            }
        }

        // Missing resource
        let missing = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.open",
            &json!({
                "session_ref": format!("br_{}", "f".repeat(64)),
                "expected_generation": 7,
                "idempotency_key": "missing-session"
            }),
            true,
            Some(&NoopActuator),
        );
        assert_eq!(missing["code"], "browser_resource_not_found");

        // Stale generation
        let stale = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.open",
            &json!({
                "session_ref": session_ref,
                "expected_generation": 999,
                "idempotency_key": "stale-gen"
            }),
            true,
            Some(&NoopActuator),
        );
        assert_eq!(stale["code"], "stale_capability_generation");

        // Non-ChatGPT provider remains unsupported even with same session.open capability.
        let (gemini_session_ref, grants) = {
            let mut guard = store.lock().unwrap();
            guard
                .observe_browser_provider(BrowserProviderObservationInput {
                    endpoint_ref: &endpoint_ref,
                    provider: "gemini",
                    adapter_protocol_version: 1,
                    observation_generation: 7,
                    capabilities_json: r#"{"operations":["session.open","session.inspect"]}"#,
                    observed_at: 15,
                })
                .unwrap();
            let gemini_account = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint_ref,
                    provider: "gemini",
                    kind: "account",
                    parent_ref: None,
                    native_identity: "gemini-account",
                    display_label: None,
                    observation_generation: 7,
                    observed_at: 16,
                })
                .unwrap();
            let gemini_session = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint_ref,
                    provider: "gemini",
                    kind: "session",
                    parent_ref: Some(&gemini_account.resource_ref),
                    native_identity: "gemini-session",
                    display_label: None,
                    observation_generation: 7,
                    observed_at: 17,
                })
                .unwrap();
            // Create a grant for gemini account but request from same endpoint/provider.
            // Directly test the provider gate via a gemini grant.
            let grants = vec![BrowserCallerGrant {
                endpoint_ref: endpoint_ref.clone(),
                provider: "gemini".to_owned(),
                account_ref: gemini_account.resource_ref.clone(),
            }];
            (gemini_session.resource_ref, grants)
        };
        let gemini_result = browser_operation_call_with_grants(
            &store,
            "herdr_mcp.browser_session.open",
            &json!({
                "session_ref": gemini_session_ref,
                "expected_generation": 7,
                "idempotency_key": "gemini-session-open"
            }),
            &grants,
            Some(&NoopActuator),
            None,
        );
        assert_eq!(gemini_result["code"], "unsupported");
    }

    #[test]
    fn browser_session_create_requires_advertised_capability_regression() {
        use crate::state_store::{
            BrowserEndpointConsentInput, BrowserEndpointRegistrationInput,
            BrowserProviderObservationInput, BrowserResourceObservationInput,
        };
        use std::sync::{Arc, Mutex};
        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (endpoint_ref, account_ref) = {
            let mut guard = store.lock().unwrap();
            let endpoint = guard
                .register_browser_endpoint(BrowserEndpointRegistrationInput {
                    device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    profile_seed: "session-create-unsupported-profile",
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
                    capabilities_json: r#"{"operations":["session.open","session.inspect"]}"#,
                    observed_at: 11,
                })
                .unwrap();
            let account = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "account",
                    parent_ref: None,
                    native_identity: "account-create-unsupported",
                    display_label: None,
                    observation_generation: 7,
                    observed_at: 12,
                })
                .unwrap();
            guard
                .set_browser_endpoint_consent(BrowserEndpointConsentInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    expected_revision: 0,
                    webchat_control_allowed: true,
                    tool_bridge_allowed: false,
                    tool_bridge_mutation_allowed: false,
                    observed_at: 13,
                })
                .unwrap();
            (endpoint.endpoint_ref, account.resource_ref)
        };
        let result = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.create",
            &json!({
                "endpoint_ref": endpoint_ref,
                "provider": "chatgpt",
                "account_ref": account_ref,
                "display_label": "Conversation",
                "expected_generation": 7,
                "message": "first assignment",
                "idempotency_key": "create-without-capability"
            }),
            true,
            None,
        );
        assert_eq!(result["code"], "capability_not_allowed");
        assert_eq!(result["actuation_available"], false);
    }

    #[test]
    fn browser_session_create_materializes_once_and_reconciles_uncertain_delivery() {
        use crate::state_store::{
            BrowserEndpointConsentInput, BrowserEndpointRegistrationInput,
            BrowserProviderObservationInput, BrowserResourceObservationInput,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        struct SessionCreateActuator {
            store: Arc<Mutex<StateStore>>,
            calls: AtomicUsize,
            reconcile_calls: AtomicUsize,
            delayed: bool,
            materialized_but_partial: bool,
        }

        impl SessionCreateActuator {
            fn applied_evidence(expected_generation: i64) -> BrowserPostconditionEvidence {
                BrowserPostconditionEvidence {
                    observed_generation: expected_generation,
                    command_accepted: true,
                    browser_online: true,
                    resource_available: true,
                    rejected: false,
                    stable_resource_ref_observed: true,
                    lifecycle_observed: true,
                    canonical_url_observed: true,
                    accepted_message_observed: true,
                    message_baseline_advanced: true,
                    reasoning_effort_readback: None,
                    required_apps_readback: Vec::new(),
                    generation_owner: Some(expected_generation),
                    generation_status_observed: true,
                    generation_stopped: false,
                    result: Some(json!({
                        "accepted_user_message_ref": "provider-created-session-user"
                    })),
                }
            }

            fn materialize(&self, params: &Value, expected_generation: i64) {
                let reservation_ref = params["reservation_ref"].as_str().unwrap();
                let account_ref = params["account_ref"].as_str().unwrap();
                let native_identity = format!("created-{reservation_ref}");
                let canonical_url = format!("https://chatgpt.com/c/{reservation_ref}");
                let mut guard = self.store.lock().unwrap();
                let reservation = guard
                    .browser_session_reservation(reservation_ref)
                    .unwrap()
                    .unwrap();
                let session = guard
                    .observe_browser_resource(BrowserResourceObservationInput {
                        endpoint_ref: &reservation.endpoint_ref,
                        provider: &reservation.provider,
                        kind: "session",
                        parent_ref: reservation.space_ref.as_deref().or(Some(account_ref)),
                        native_identity: &native_identity,
                        display_label: Some(&reservation.display_label),
                        observation_generation: expected_generation,
                        observed_at: 20,
                    })
                    .unwrap();
                guard
                    .upsert_browser_resource_locator(
                        &session.resource_ref,
                        &canonical_url,
                        expected_generation,
                        20,
                    )
                    .unwrap();
                guard
                    .materialize_browser_session_reservation(
                        reservation_ref,
                        &session.resource_ref,
                        20,
                    )
                    .unwrap();
            }
        }

        impl BrowserActuator for SessionCreateActuator {
            fn actuate(
                &self,
                operation: &str,
                params: &Value,
                expected_generation: i64,
                dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                assert_eq!(operation, BrowserOperation::SessionCreate.method());
                assert_eq!(dispatch_id, params["reservation_ref"].as_str());
                assert!(
                    params["launch_url"]
                        .as_str()
                        .unwrap()
                        .starts_with("https://chatgpt.com")
                );
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.materialize(params, expected_generation);
                let mut evidence = Self::applied_evidence(expected_generation);
                if self.materialized_but_partial {
                    evidence.stable_resource_ref_observed = false;
                    evidence.lifecycle_observed = false;
                    evidence.canonical_url_observed = false;
                    evidence.generation_status_observed = false;
                }
                if self.delayed {
                    evidence.generation_status_observed = false;
                    evidence.result = None;
                }
                Ok(evidence)
            }

            fn reconcile_dispatch(
                &self,
                dispatch_id: &str,
                expected_generation: i64,
            ) -> Result<Option<BrowserPostconditionEvidence>, String> {
                assert!(dispatch_id.starts_with("bsr_"));
                self.reconcile_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some(Self::applied_evidence(expected_generation)))
            }
        }

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let (endpoint_ref, account_ref, source_url) = {
            let mut guard = store.lock().unwrap();
            let endpoint = guard
                .register_browser_endpoint(BrowserEndpointRegistrationInput {
                    device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    profile_seed: "session-create-profile-seed",
                    browser_family: "chrome",
                    extension_version: "0.1.91",
                    observed_at: 10,
                })
                .unwrap();
            guard
                .observe_browser_provider(BrowserProviderObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    adapter_protocol_version: 1,
                    observation_generation: 7,
                    capabilities_json: r#"{"operations":["session.create","session.open","composer.submit"]}"#,
                    observed_at: 11,
                })
                .unwrap();
            let account = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "account",
                    parent_ref: None,
                    native_identity: "session-create-account",
                    display_label: None,
                    observation_generation: 7,
                    observed_at: 12,
                })
                .unwrap();
            guard
                .upsert_browser_resource_locator(
                    &account.resource_ref,
                    "https://chatgpt.com",
                    7,
                    12,
                )
                .unwrap();
            let space = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "space",
                    parent_ref: Some(&account.resource_ref),
                    native_identity: "session-create-project",
                    display_label: Some("Project A"),
                    observation_generation: 7,
                    observed_at: 12,
                })
                .unwrap();
            guard
                .upsert_browser_resource_locator(
                    &space.resource_ref,
                    "https://chatgpt.com/g/g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/project",
                    7,
                    12,
                )
                .unwrap();
            let source = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "session",
                    parent_ref: Some(&space.resource_ref),
                    native_identity: "session-create-source",
                    display_label: Some("Source"),
                    observation_generation: 7,
                    observed_at: 12,
                })
                .unwrap();
            let source_url =
                "https://chatgpt.com/g/g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/c/source-conv";
            guard
                .upsert_browser_resource_locator(&source.resource_ref, source_url, 7, 12)
                .unwrap();
            guard
                .set_browser_endpoint_consent(BrowserEndpointConsentInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    expected_revision: 0,
                    webchat_control_allowed: true,
                    tool_bridge_allowed: false,
                    tool_bridge_mutation_allowed: false,
                    observed_at: 13,
                })
                .unwrap();
            (
                endpoint.endpoint_ref,
                account.resource_ref,
                source_url.to_owned(),
            )
        };

        let immediate = SessionCreateActuator {
            store: store.clone(),
            calls: AtomicUsize::new(0),
            reconcile_calls: AtomicUsize::new(0),
            delayed: false,
            materialized_but_partial: false,
        };
        let immediate_params = json!({
            "endpoint_ref": endpoint_ref,
            "provider": "chatgpt",
            "account_ref": account_ref,
            "display_label": "Worker A",
            "message": "do bounded task A",
            "expected_generation": 7,
            "idempotency_key": "session-create-immediate-1"
        });
        let created = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.create",
            &immediate_params,
            true,
            Some(&immediate),
        );
        assert_eq!(created["ok"], true);
        assert!(created["session_ref"].as_str().unwrap().starts_with("br_"));
        assert_eq!(created["dispatch"]["delivery_state"], "applied");
        assert_eq!(immediate.calls.load(Ordering::SeqCst), 1);
        let replay = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.create",
            &immediate_params,
            true,
            Some(&immediate),
        );
        assert_eq!(replay["ok"], true);
        assert_eq!(replay["replayed"], true);
        assert_eq!(immediate.calls.load(Ordering::SeqCst), 1);

        let materialized_but_partial = SessionCreateActuator {
            store: store.clone(),
            calls: AtomicUsize::new(0),
            reconcile_calls: AtomicUsize::new(0),
            delayed: false,
            materialized_but_partial: true,
        };
        let materialized_but_partial_params = json!({
            "endpoint_ref": endpoint_ref,
            "provider": "chatgpt",
            "account_ref": account_ref,
            "display_label": "Worker accepted-before-registration-readback",
            "message": "do bounded task with accepted provider identity",
            "expected_generation": 7,
            "idempotency_key": "session-create-materialized-partial-1"
        });
        let promoted = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.create",
            &materialized_but_partial_params,
            true,
            Some(&materialized_but_partial),
        );
        assert_eq!(promoted["ok"], true);
        assert_eq!(promoted["delivery_state"], "applied");
        assert_eq!(promoted["reconciled"], true);
        assert_eq!(
            promoted["dispatch"]["accepted_user_message_ref"],
            "provider-created-session-user"
        );
        assert_eq!(materialized_but_partial.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            materialized_but_partial
                .reconcile_calls
                .load(Ordering::SeqCst),
            0
        );

        let shortcut_params = json!({
            "source_url": source_url,
            "message": "continue from source URL",
            "idempotency_key": "session-create-source-url-1"
        });
        let shortcut = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.create",
            &shortcut_params,
            true,
            Some(&immediate),
        );
        assert_eq!(shortcut["ok"], true);
        let shortcut_ref = shortcut["session_ref"].as_str().unwrap();
        let shortcut_resource = store
            .lock()
            .unwrap()
            .browser_resource(shortcut_ref)
            .unwrap()
            .unwrap();
        let shortcut_parent = store
            .lock()
            .unwrap()
            .browser_resource(shortcut_resource.parent_ref.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(shortcut_parent.kind, "space");
        assert_eq!(shortcut_parent.display_label.as_deref(), Some("Project A"));
        assert_eq!(immediate.calls.load(Ordering::SeqCst), 2);

        let delayed = SessionCreateActuator {
            store: store.clone(),
            calls: AtomicUsize::new(0),
            reconcile_calls: AtomicUsize::new(0),
            delayed: true,
            materialized_but_partial: false,
        };
        let delayed_params = json!({
            "endpoint_ref": endpoint_ref,
            "provider": "chatgpt",
            "account_ref": account_ref,
            "display_label": "Worker B",
            "message": "do bounded task B",
            "expected_generation": 7,
            "idempotency_key": "session-create-delayed-1"
        });
        let uncertain = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.create",
            &delayed_params,
            true,
            Some(&delayed),
        );
        assert_eq!(uncertain["ok"], false);
        assert_eq!(uncertain["delivery_state"], "uncertain");
        assert_eq!(delayed.calls.load(Ordering::SeqCst), 1);
        let reconciled = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.create",
            &delayed_params,
            true,
            Some(&delayed),
        );
        assert_eq!(reconciled["ok"], true);
        assert_eq!(reconciled["replayed"], true);
        assert_eq!(reconciled["reconciled"], true);
        assert_eq!(delayed.calls.load(Ordering::SeqCst), 1);
        assert_eq!(delayed.reconcile_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn browser_actuation_intersection_fails_closed_with_stable_reasons() {
        use crate::state_store::{
            BrowserEndpointConsentInput, BrowserEndpointRegistrationInput,
            BrowserProviderObservationInput, BrowserResourceObservationInput,
            BrowserResourceRecord,
        };
        use std::sync::{Arc, Mutex, RwLock};

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
            None,
        );
        assert_eq!(inspect["ok"], true);
        assert_eq!(inspect["actuation_available"], true);
        assert!(inspect["actuation_reason"].is_null());
        assert_eq!(inspect["route"]["endpoint_ref"], endpoint_ref);
        assert_eq!(inspect["route"]["provider"], "chatgpt");
        assert_eq!(inspect["route"]["account_ref"], account_ref);
        assert_eq!(inspect["route"]["resource_observation_generation"], 7);
        assert_eq!(inspect["route"]["provider_observation_generation"], 7);
        assert_eq!(inspect["route"]["adapter_protocol_version"], 1);
        assert_eq!(inspect["route"]["status"], "current");
        assert_eq!(
            inspect["route"]["configuration_source"],
            "browser_registry_observation"
        );
        assert_eq!(inspect["capabilities"]["schema_version"], 1);
        assert_eq!(
            inspect["capabilities"]["input_contract"]["message"]["max_bytes"],
            262_144
        );
        assert_eq!(
            inspect["capabilities"]["provider_dynamic_limits"]["turn_timeout_ms"]["status"],
            "unknown"
        );

        struct GateProbeActuator<'a> {
            gate: &'a RwLock<()>,
        }
        impl BrowserActuator for GateProbeActuator<'_> {
            fn actuate(
                &self,
                _operation: &str,
                _params: &Value,
                expected_generation: i64,
                _dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                assert!(
                    self.gate.try_write().is_err(),
                    "the consent/mutation gate must remain held through browser actuation"
                );
                Ok(BrowserPostconditionEvidence::resource_unavailable(
                    expected_generation,
                ))
            }
        }
        struct PanicActuator;
        impl BrowserActuator for PanicActuator {
            fn actuate(
                &self,
                _operation: &str,
                _params: &Value,
                _expected_generation: i64,
                _dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                panic!("explicit unsupported mutations must not reach browser actuation")
            }
        }
        let admission = BrowserMutationAdmission::default();
        let same_account_scope = BrowserMutationScope {
            endpoint_ref: endpoint_ref.clone(),
            provider: "chatgpt".to_owned(),
            account_ref: account_ref.clone(),
        };
        let same_account_permit = admission
            .reserve(&same_account_scope)
            .unwrap()
            .expect("first account mutation reserves its slot");
        let backpressured = browser_operation_call_with_controls(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &dispatch_params,
            &exact_grants,
            Some(&PanicActuator),
            BrowserOperationControls {
                mutation_gate: None,
                mutation_admission: Some(&admission),
                caller_authorization: None,
            },
        );
        assert_eq!(backpressured["code"], "browser_account_backpressure");
        assert_eq!(backpressured["retryable"], true);
        assert_eq!(backpressured["limit"], 1);
        assert_eq!(
            backpressured["retry_after_ms"],
            BROWSER_ACCOUNT_MUTATION_RETRY_AFTER_MS
        );
        assert_eq!(
            backpressured["resource_key"],
            format!("{endpoint_ref}:chatgpt:{account_ref}")
        );
        let other_account_permit = admission
            .reserve(&BrowserMutationScope {
                endpoint_ref: endpoint_ref.clone(),
                provider: "chatgpt".to_owned(),
                account_ref: format!("br_{}", "f".repeat(64)),
            })
            .unwrap()
            .expect("an unrelated account keeps independent admission capacity");
        drop(other_account_permit);
        drop(same_account_permit);

        let mutation_gate = RwLock::new(());
        let gated_attempt = browser_operation_call_with_grants(
            &store,
            "herdr_mcp.browser_dispatch.submit",
            &dispatch_params,
            &exact_grants,
            Some(&GateProbeActuator {
                gate: &mutation_gate,
            }),
            Some(&mutation_gate),
        );
        assert_eq!(gated_attempt["code"], "resource_unavailable");

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
            Some(&PanicActuator),
        );
        assert_eq!(dispatch_reasoning_not_allowed["code"], "unsupported");

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
            Some(&PanicActuator),
        );
        assert_eq!(dispatch_apps_not_allowed["code"], "unsupported");

        let unsupported_reasoning = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_composer.set_reasoning",
            &json!({
                "session_ref": session_ref,
                "reasoning_effort": "balanced",
                "expected_generation": 7,
                "idempotency_key": "actuation-reasoning-1"
            }),
            true,
            Some(&PanicActuator),
        );
        assert_eq!(unsupported_reasoning["code"], "unsupported");

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
        let stale_inspect = browser_operation_call_with_grants(
            &store,
            "herdr_mcp.browser_session.inspect",
            &json!({"session_ref": session_ref}),
            &exact_grants,
            None,
            None,
        );
        assert_eq!(stale_inspect["ok"], true);
        assert_eq!(stale_inspect["actuation_available"], false);
        assert_eq!(
            stale_inspect["actuation_reason"],
            "stale_capability_generation"
        );
        assert_eq!(
            stale_inspect["route"]["status"],
            "stale_capability_generation"
        );
        assert_eq!(stale_inspect["route"]["resource_observation_generation"], 7);
        assert_eq!(stale_inspect["route"]["provider_observation_generation"], 8);
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
    fn browser_adapter_protocol_skew_fails_closed_after_generation_fence() {
        use crate::state_store::{
            BrowserEndpointConsentInput, BrowserEndpointRegistrationInput,
            BrowserProviderObservationInput, BrowserResourceRecord,
        };

        let mut store = StateStore::open(":memory:").unwrap();
        let endpoint = store
            .register_browser_endpoint(BrowserEndpointRegistrationInput {
                device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                profile_seed: "beta1-protocol-skew-profile",
                browser_family: "chrome",
                extension_version: "0.1.90",
                observed_at: 10,
            })
            .unwrap();
        store
            .set_browser_endpoint_consent(BrowserEndpointConsentInput {
                endpoint_ref: &endpoint.endpoint_ref,
                expected_revision: 0,
                webchat_control_allowed: true,
                tool_bridge_allowed: false,
                tool_bridge_mutation_allowed: false,
                observed_at: 11,
            })
            .unwrap();
        store
            .observe_browser_provider(BrowserProviderObservationInput {
                endpoint_ref: &endpoint.endpoint_ref,
                provider: "chatgpt",
                adapter_protocol_version: 2,
                observation_generation: 7,
                capabilities_json: r#"{"operations":["composer.submit"]}"#,
                observed_at: 12,
            })
            .unwrap();
        let account_ref = format!("br_{}", "9".repeat(64));
        let resource = BrowserResourceRecord {
            resource_ref: account_ref.clone(),
            endpoint_ref: endpoint.endpoint_ref.clone(),
            provider: "chatgpt".to_owned(),
            kind: "account".to_owned(),
            parent_ref: None,
            native_identity_sha256: "a".repeat(64),
            display_label: None,
            observation_generation: 7,
            first_observed_at: 12,
            last_observed_at: 12,
        };
        let grants = [BrowserCallerGrant {
            endpoint_ref: endpoint.endpoint_ref.clone(),
            provider: "chatgpt".to_owned(),
            account_ref,
        }];
        assert_eq!(
            browser_resource_actuation_decision(
                &store,
                "composer.submit",
                &resource,
                &grants,
                Some(7),
            )
            .unwrap(),
            (false, Some("browser_adapter_protocol_unsupported"))
        );

        store
            .observe_browser_provider(BrowserProviderObservationInput {
                endpoint_ref: &endpoint.endpoint_ref,
                provider: "chatgpt",
                adapter_protocol_version: 3,
                observation_generation: 8,
                capabilities_json: r#"{"operations":["composer.submit"]}"#,
                observed_at: 13,
            })
            .unwrap();
        assert_eq!(
            browser_resource_actuation_decision(
                &store,
                "composer.submit",
                &resource,
                &grants,
                Some(7),
            )
            .unwrap(),
            (false, Some("stale_capability_generation")),
            "generation mismatch remains the primary pre-dispatch fence"
        );
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

    #[test]
    fn page_assist_requires_exact_caller_grant_and_returns_extension_result() {
        struct PanicActuator;
        impl BrowserActuator for PanicActuator {
            fn actuate(
                &self,
                _operation: &str,
                _params: &Value,
                _expected_generation: i64,
                _dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                panic!("caller grant rejection must happen before browser actuation")
            }
        }
        let params = json!({
            "endpoint_ref": "bep_test",
            "action": "inspect",
            "target_origin": "https://example.com",
            "max_chars": 4096
        });
        let denied = page_assist_call(&params, &[], Some(&PanicActuator));
        assert_eq!(denied["ok"], false);
        assert_eq!(denied["code"], "caller_grant_missing");
        assert_eq!(denied["delivery_state"], "not_delivered");

        struct ResultActuator;
        impl BrowserActuator for ResultActuator {
            fn actuate(
                &self,
                operation: &str,
                params: &Value,
                expected_generation: i64,
                dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                assert_eq!(operation, "herdr_mcp.page_assist");
                assert!(dispatch_id.is_none());
                assert_eq!(params["target_origin"], "https://example.com");
                assert_eq!(params["max_chars"], 4096);
                let mut evidence =
                    BrowserPostconditionEvidence::resource_unavailable(expected_generation);
                evidence.command_accepted = true;
                evidence.browser_online = true;
                evidence.resource_available = true;
                evidence.result = Some(json!({
                    "ok": true,
                    "generation": "pa:test",
                    "text": "visible page text"
                }));
                Ok(evidence)
            }
        }
        let grants = [PageAssistCallerGrant {
            endpoint_ref: "bep_test".to_owned(),
        }];
        let allowed = page_assist_call(&params, &grants, Some(&ResultActuator));
        assert_eq!(allowed["ok"], true);
        assert_eq!(allowed["generation"], "pa:test");
        assert_eq!(allowed["text"], "visible page text");
    }
    #[test]
    fn browser_session_open_uses_local_locator_and_replays_idempotently() {
        use crate::state_store::{
            BrowserEndpointConsentInput, BrowserEndpointRegistrationInput,
            BrowserProviderObservationInput, BrowserResourceObservationInput,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        struct SessionOpenActuator {
            calls: AtomicUsize,
            session_ref: String,
        }

        impl BrowserActuator for SessionOpenActuator {
            fn actuate(
                &self,
                operation: &str,
                params: &Value,
                expected_generation: i64,
                dispatch_id: Option<&str>,
            ) -> Result<BrowserPostconditionEvidence, String> {
                assert_eq!(operation, BrowserOperation::SessionOpen.method());
                assert!(dispatch_id.is_none());
                assert_eq!(params["session_ref"], self.session_ref);
                assert_eq!(params["provider"], "chatgpt");
                assert_eq!(
                    params["canonical_url"],
                    "https://chatgpt.com/c/session-open-1"
                );
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(BrowserPostconditionEvidence {
                    observed_generation: expected_generation,
                    command_accepted: true,
                    browser_online: true,
                    resource_available: true,
                    rejected: false,
                    stable_resource_ref_observed: true,
                    lifecycle_observed: true,
                    canonical_url_observed: true,
                    accepted_message_observed: false,
                    message_baseline_advanced: false,
                    reasoning_effort_readback: None,
                    required_apps_readback: Vec::new(),
                    generation_owner: None,
                    generation_status_observed: false,
                    generation_stopped: false,
                    result: None,
                })
            }
        }

        let store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
        let session_ref = {
            let mut guard = store.lock().unwrap();
            let endpoint = guard
                .register_browser_endpoint(BrowserEndpointRegistrationInput {
                    device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    profile_seed: "session-open-profile-seed",
                    browser_family: "chrome",
                    extension_version: "0.1.91",
                    observed_at: 10,
                })
                .unwrap();
            guard
                .observe_browser_provider(BrowserProviderObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    adapter_protocol_version: 1,
                    observation_generation: 7,
                    capabilities_json: r#"{"operations":["session.open","session.inspect"]}"#,
                    observed_at: 11,
                })
                .unwrap();
            let account = guard
                .observe_browser_resource(BrowserResourceObservationInput {
                    endpoint_ref: &endpoint.endpoint_ref,
                    provider: "chatgpt",
                    kind: "account",
                    parent_ref: None,
                    native_identity: "session-open-account",
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
                    native_identity: "session-open-1",
                    display_label: None,
                    observation_generation: 7,
                    observed_at: 13,
                })
                .unwrap();
            guard
                .upsert_browser_resource_locator(
                    &session.resource_ref,
                    "https://chatgpt.com/c/session-open-1",
                    7,
                    13,
                )
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
        let actuator = SessionOpenActuator {
            calls: AtomicUsize::new(0),
            session_ref: session_ref.clone(),
        };
        let params = json!({
            "session_ref": session_ref,
            "expected_generation": 7,
            "idempotency_key": "session-open-idempotency-1"
        });
        let first = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.open",
            &params,
            true,
            Some(&actuator),
        );
        assert_eq!(first["ok"], true);
        assert_eq!(first["delivery_state"], "applied");
        assert_eq!(first["idempotent_replay"], false);
        assert_eq!(actuator.calls.load(Ordering::SeqCst), 1);

        let replay = browser_operation_call_with_grant(
            &store,
            "herdr_mcp.browser_session.open",
            &params,
            true,
            Some(&actuator),
        );
        assert_eq!(replay["ok"], true);
        assert_eq!(replay["idempotent_replay"], true);
        assert_eq!(actuator.calls.load(Ordering::SeqCst), 1);
    }
}
