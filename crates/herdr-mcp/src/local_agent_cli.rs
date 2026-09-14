use crate::cli::{ContinuityCommand, MemoryCommand, WebChatCommand};
use crate::link::local_mcp::{
    LinkRuntimeTransport, LocalMcpConfig, LocalMcpTransport, RuntimeToolResult,
};
use crate::link::request_core::RuntimeRequest;
use crate::paths::RuntimePaths;
use serde_json::{Map, Value, json};
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

pub(crate) fn run_continuity(command: ContinuityCommand) -> Result<ExitCode, String> {
    match command {
        ContinuityCommand::Search {
            query,
            project_id,
            project_path,
            workspace_id,
            limit,
        } => {
            let mut params = Map::new();
            params.insert("query".to_owned(), json!(query));
            insert_optional(&mut params, "project_id", project_id);
            if let Some(project_path) = project_path {
                params.insert(
                    "repo_id".to_owned(),
                    json!(repo_id_for_project_path(Path::new(&project_path))?),
                );
            }
            insert_optional(&mut params, "workspace_id", workspace_id);
            params.insert("limit".to_owned(), json!(limit));
            print_private_result(call_private(
                "continuity.search",
                Value::Object(params),
                None,
            )?)
        }
        ContinuityCommand::Resume { continuity_id } => print_private_result(call_private(
            "continuity.resume",
            json!({"continuity_id": continuity_id}),
            None,
        )?),
    }
}

pub(crate) fn run_memory(command: MemoryCommand) -> Result<ExitCode, String> {
    let (method, params) = match command {
        MemoryCommand::Resume {
            project_ref,
            repo_id,
            work_chain_id,
            max_turns,
        } => (
            "work_memory.resume",
            json!({
                "project_ref": project_ref,
                "repo_id": repo_id,
                "work_chain_id": work_chain_id,
                "max_turns": max_turns,
            }),
        ),
        MemoryCommand::Search {
            project_ref,
            repo_id,
            work_chain_id,
            query,
            limit,
        } => (
            "work_memory.search",
            json!({
                "project_ref": project_ref,
                "repo_id": repo_id,
                "work_chain_id": work_chain_id,
                "query": query,
                "limit": limit,
            }),
        ),
    };
    print_private_result(call_private(method, params, None)?)
}

pub(crate) fn run_webchat(command: WebChatCommand) -> Result<ExitCode, String> {
    match command {
        WebChatCommand::Endpoints { limit } => print_private_result(call_private(
            "herdr_mcp.browser_endpoint.list",
            json!({"limit": limit}),
            None,
        )?),
        WebChatCommand::Resources {
            endpoint_ref,
            provider,
            kind,
            parent_ref,
            limit,
        } => {
            let mut params = Map::new();
            insert_optional(&mut params, "endpoint_ref", endpoint_ref);
            insert_optional(&mut params, "provider", provider);
            insert_optional(&mut params, "kind", kind);
            insert_optional(&mut params, "parent_ref", parent_ref);
            params.insert("limit".to_owned(), json!(limit));
            print_private_result(call_private(
                "herdr_mcp.browser_resource.list",
                Value::Object(params),
                None,
            )?)
        }
        WebChatCommand::Inspect { resource_ref } => {
            let grant = grant_for_resource(&resource_ref)?;
            print_private_result(call_private(
                "herdr_mcp.browser_resource.inspect",
                json!({"resource_ref": resource_ref}),
                Some(&grant),
            )?)
        }
        WebChatCommand::Create {
            endpoint_ref,
            provider,
            account_ref,
            space_ref,
            display_label,
            message,
            expected_generation,
            idempotency_key,
            work_chain_id,
        } => {
            let grant = BrowserGrant {
                endpoint_ref: endpoint_ref.clone(),
                provider: provider.clone(),
                account_ref: account_ref.clone(),
            };
            let mut params = Map::new();
            params.insert("endpoint_ref".to_owned(), json!(endpoint_ref));
            params.insert("provider".to_owned(), json!(provider));
            params.insert("account_ref".to_owned(), json!(account_ref));
            insert_optional(&mut params, "space_ref", space_ref);
            params.insert("display_label".to_owned(), json!(display_label));
            params.insert("message".to_owned(), json!(message));
            params.insert("expected_generation".to_owned(), json!(expected_generation));
            params.insert("idempotency_key".to_owned(), json!(idempotency_key));
            insert_optional(&mut params, "work_chain_id", work_chain_id);
            print_private_result(call_private(
                "herdr_mcp.browser_session.create",
                Value::Object(params),
                Some(&grant),
            )?)
        }
        WebChatCommand::Send {
            session_ref,
            message,
            expected_generation,
            idempotency_key,
            work_chain_id,
        } => {
            let grant = grant_for_resource(&session_ref)?;
            let mut params = Map::new();
            params.insert("session_ref".to_owned(), json!(session_ref));
            params.insert("message".to_owned(), json!(message));
            params.insert("expected_generation".to_owned(), json!(expected_generation));
            params.insert("idempotency_key".to_owned(), json!(idempotency_key));
            insert_optional(&mut params, "work_chain_id", work_chain_id);
            print_private_result(call_private(
                "herdr_mcp.browser_dispatch.submit",
                Value::Object(params),
                Some(&grant),
            )?)
        }
        WebChatCommand::DispatchStatus { dispatch_id } => print_private_result(call_private(
            "herdr_mcp.browser_dispatch.status",
            json!({"dispatch_id": dispatch_id}),
            None,
        )?),
        WebChatCommand::Archive {
            session_ref,
            expected_generation,
            idempotency_key,
        } => {
            let grant = grant_for_resource(&session_ref)?;
            print_private_result(call_private(
                "herdr_mcp.browser_session.archive",
                json!({
                    "session_ref": session_ref,
                    "expected_generation": expected_generation,
                    "idempotency_key": idempotency_key,
                }),
                Some(&grant),
            )?)
        }
        WebChatCommand::Handoff {
            continuity_id,
            source_url,
            objective,
            work_chain_id,
            handoff_id,
            idempotency_key,
            prepare_only,
        } => print_private_result(webchat_handoff(
            WebChatHandoffRequest {
                continuity_id,
                source_url,
                objective,
                work_chain_id,
                handoff_id,
                idempotency_key,
                prepare_only,
            },
            call_private,
        )?),
    }
}

const BROWSER_HANDOFF_PREPARE_METHOD: &str = "herdr_mcp.browser_handoff.prepare";
const BROWSER_SESSION_CREATE_METHOD: &str = "herdr_mcp.browser_session.create";

/// The delivery states the browser runtime can report for a mutation.
const BROWSER_DELIVERY_STATES: &[&str] = &[
    "applied",
    "not_applied",
    "uncertain",
    "rejected",
    "browser_offline",
    "resource_unavailable",
    "stopped",
];

struct WebChatHandoffRequest {
    continuity_id: String,
    source_url: String,
    objective: Option<String>,
    work_chain_id: Option<String>,
    handoff_id: Option<String>,
    idempotency_key: Option<String>,
    prepare_only: bool,
}

/// Canonical handoff for a local agent.
///
/// This never builds a handoff message itself: it asks the runtime for the canonical
/// packet (`herdr_mcp.browser_handoff.prepare`), hands that packet's own
/// `automatic_delivery.params` to the existing source-anchored browser session create,
/// and reports the delivery evidence. A prepared packet is not a completed handoff, and
/// a delivery whose outcome is uncertain is never retried here.
fn webchat_handoff<F>(request: WebChatHandoffRequest, mut call: F) -> Result<Value, String>
where
    F: FnMut(&str, Value, Option<&BrowserGrant>) -> Result<Value, String>,
{
    let mut prepare_params = Map::new();
    prepare_params.insert("continuity_id".to_owned(), json!(request.continuity_id));
    prepare_params.insert("source_url".to_owned(), json!(request.source_url));
    insert_optional(&mut prepare_params, "objective", request.objective);
    insert_optional(&mut prepare_params, "work_chain_id", request.work_chain_id);
    insert_optional(&mut prepare_params, "handoff_id", request.handoff_id);
    let prepared = call(
        BROWSER_HANDOFF_PREPARE_METHOD,
        Value::Object(prepare_params),
        None,
    )?;
    if prepared.get("ok").and_then(Value::as_bool) != Some(true) {
        return Ok(prepared);
    }

    let canonical_params = prepared
        .pointer("/automatic_delivery/params")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| "canonical handoff packet has no automatic_delivery params".to_owned())?;
    let canonical_message = canonical_params
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| "canonical handoff packet has no delivery message".to_owned())?
        .to_owned();
    let canonical_source_url = canonical_params
        .get("source_url")
        .and_then(Value::as_str)
        .ok_or_else(|| "canonical handoff packet has no source_url".to_owned())?
        .to_owned();
    let copy_prompt = prepared
        .pointer("/manual_delivery/copy_prompt")
        .and_then(Value::as_str)
        .ok_or_else(|| "canonical handoff packet has no manual delivery prompt".to_owned())?;
    if copy_prompt != canonical_message {
        return Err(
            "canonical handoff packet returned divergent automatic and manual messages".to_owned(),
        );
    }

    let route = prepared
        .get("source_route")
        .filter(|value| !value.is_null());
    let delivery = if request.prepare_only {
        json!({
            "attempted": false,
            "completed": false,
            "reason": "prepare_only",
            "delivery_state": Value::Null,
            "replayed": false,
            "session_ref": Value::Null,
            "dispatch_id": Value::Null,
            "result": Value::Null,
        })
    } else if let Some(route) = route {
        let grant = BrowserGrant {
            endpoint_ref: handoff_route_string(route, "endpoint_ref")?,
            provider: handoff_route_string(route, "provider")?,
            account_ref: handoff_route_string(route, "account_ref")?,
        };
        let idempotency_key = match request.idempotency_key.clone() {
            Some(value) => value,
            None => prepared
                .pointer("/handoff/handoff_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "canonical handoff packet has no handoff_id".to_owned())?
                .to_owned(),
        };
        let mut create_params = Map::new();
        create_params.insert("source_url".to_owned(), json!(canonical_source_url));
        create_params.insert("message".to_owned(), json!(canonical_message));
        if let Some(work_chain_id) = canonical_params.get("work_chain_id").cloned()
            && !work_chain_id.is_null()
        {
            create_params.insert("work_chain_id".to_owned(), work_chain_id);
        }
        create_params.insert("idempotency_key".to_owned(), json!(idempotency_key));
        let result = call(
            BROWSER_SESSION_CREATE_METHOD,
            Value::Object(create_params),
            Some(&grant),
        )?;
        handoff_delivery_result(&result)
    } else {
        let reason = prepared
            .get("source_route_error")
            .and_then(Value::as_str)
            .unwrap_or("browser_source_route_unavailable");
        json!({
            "attempted": false,
            "completed": false,
            "reason": reason,
            "delivery_state": Value::Null,
            "replayed": false,
            "session_ref": Value::Null,
            "dispatch_id": Value::Null,
            "result": Value::Null,
        })
    };

    let delivery_state = delivery
        .get("delivery_state")
        .and_then(Value::as_str)
        .unwrap_or("not_attempted")
        .to_owned();
    let completed = delivery.get("completed").and_then(Value::as_bool) == Some(true);
    let attempted = delivery.get("attempted").and_then(Value::as_bool) == Some(true);

    let mut output = prepared;
    if let Some(object) = output.as_object_mut() {
        object.insert("action".to_owned(), json!("webchat_handoff"));
        if let Some(automatic) = object
            .get_mut("automatic_delivery")
            .and_then(Value::as_object_mut)
        {
            for (key, value) in delivery.as_object().into_iter().flatten() {
                automatic.insert(key.clone(), value.clone());
            }
        }
        object.insert(
            "instruction".to_owned(),
            json!(handoff_instruction(attempted, completed, &delivery_state)),
        );
    }
    Ok(output)
}

fn handoff_delivery_result(result: &Value) -> Value {
    let code = result.get("code").and_then(Value::as_str);
    let delivery_state = result
        .get("delivery_state")
        .and_then(Value::as_str)
        .or_else(|| code.filter(|code| BROWSER_DELIVERY_STATES.contains(code)));
    let reason = code.filter(|code| !BROWSER_DELIVERY_STATES.contains(code));
    json!({
        "attempted": true,
        "completed": delivery_state == Some("applied"),
        "reason": reason,
        "delivery_state": delivery_state,
        "replayed": result.get("replayed").cloned().unwrap_or(json!(false)),
        "session_ref": result.get("session_ref").cloned().unwrap_or(Value::Null),
        "dispatch_id": result.pointer("/dispatch/dispatch_id").cloned().unwrap_or(Value::Null),
        "result": result,
    })
}

fn handoff_route_string(route: &Value, field: &str) -> Result<String, String> {
    route
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("canonical handoff packet has no source route {field}"))
}

fn handoff_instruction(attempted: bool, completed: bool, delivery_state: &str) -> &'static str {
    if completed {
        "Canonical handoff prepared and automatically delivered: the target conversation was created and its first step is continuity.resume for the same continuity_id. Re-check live workspace / Git / runtime state on the target side, and do not create a second conversation for the same logical handoff."
    } else if attempted {
        "Canonical handoff prepared, but automatic browser delivery did not complete. Do not blindly re-create it: read the returned delivery_state, session_ref, dispatch_id and result first, and use webchat dispatch-status for an existing dispatch. The same canonical message is available as manual_delivery.copy_prompt. A retry of the same logical handoff must reuse the same idempotency key."
    } else if delivery_state == "prepare_only" {
        "Canonical handoff prepared only; automatic delivery was not attempted. Nothing was created: hand manual_delivery.copy_prompt to the user for a WebChat conversation, and do not report this as a completed handoff."
    } else {
        "Canonical handoff prepared, but automatic delivery was not attempted because the source conversation route is unavailable. Nothing was created: re-observe the browser resources, or hand manual_delivery.copy_prompt to the user for a WebChat conversation. This is a prepared packet, not a completed handoff."
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserGrant {
    endpoint_ref: String,
    provider: String,
    account_ref: String,
}

fn grant_for_resource(resource_ref: &str) -> Result<BrowserGrant, String> {
    let mut current_ref = resource_ref.to_owned();
    let mut expected_endpoint: Option<String> = None;
    let mut expected_provider: Option<String> = None;
    for _ in 0..4 {
        let result = call_private(
            "herdr_mcp.browser_resource.inspect",
            json!({"resource_ref": current_ref}),
            None,
        )?;
        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(format!(
                "cannot resolve WebChat grant scope: {}",
                compact_json(&result)
            ));
        }
        let resource = result
            .get("resource")
            .and_then(Value::as_object)
            .ok_or_else(|| "browser resource inspection returned no resource".to_owned())?;
        let endpoint_ref = required_json_string(resource, "endpoint_ref")?;
        let provider = required_json_string(resource, "provider")?;
        if expected_endpoint
            .as_deref()
            .is_some_and(|value| value != endpoint_ref)
            || expected_provider
                .as_deref()
                .is_some_and(|value| value != provider)
        {
            return Err("browser resource parent scope changed during grant resolution".to_owned());
        }
        expected_endpoint.get_or_insert_with(|| endpoint_ref.to_owned());
        expected_provider.get_or_insert_with(|| provider.to_owned());
        let kind = required_json_string(resource, "kind")?;
        if kind == "account" {
            return Ok(BrowserGrant {
                endpoint_ref: endpoint_ref.to_owned(),
                provider: provider.to_owned(),
                account_ref: required_json_string(resource, "resource_ref")?.to_owned(),
            });
        }
        current_ref = resource
            .get("parent_ref")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "browser resource has no account parent".to_owned())?
            .to_owned();
    }
    Err("browser resource parent chain exceeds supported depth".to_owned())
}

fn call_private(
    method: &str,
    params: Value,
    webchat_grant: Option<&BrowserGrant>,
) -> Result<Value, String> {
    if !params.is_object() {
        return Err("local private call params must be an object".to_owned());
    }
    let paths = RuntimePaths::discover()?;
    let token = crate::service_manager::doctor_runtime_token()?
        .ok_or_else(|| "local Herdr-MCP runtime credential is unavailable".to_owned())?;
    let contract = crate::contract::identity()?;
    let config_file =
        crate::config::Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let mut config = LocalMcpConfig::new(token, contract.hash.clone());
    config.endpoint = format!("http://127.0.0.1:{}/mcp", config_file.runtime_port);
    config.contract_epoch = u64::from(contract.epoch);
    config.runtime_generation = active_runtime_generation(&paths.config_dir)?;
    let transport = LocalMcpTransport::new(config)
        .map_err(|error| format!("cannot initialize local MCP transport: {error:?}"))?;
    let arguments = json!({
        "method": method,
        "params": serde_json::to_string(&params)
            .map_err(|error| format!("cannot encode local private call params: {error}"))?,
    })
    .as_object()
    .cloned()
    .ok_or_else(|| "cannot construct local private call".to_owned())?;
    let trace = webchat_grant.map(|grant| {
        json!({
            "webchat_control_grants": [{
                "endpoint_ref": grant.endpoint_ref,
                "provider": grant.provider,
                "account_ref": grant.account_ref,
            }]
        })
        .as_object()
        .cloned()
        .expect("grant trace is an object")
    });
    let request = RuntimeRequest {
        workstation_id: "local-agent-cli".to_owned(),
        request_id: local_request_id(),
        operation: "herdr_call".to_owned(),
        arguments: Some(arguments),
        timeout_ms: Some(30_000u64.into()),
        contract_epoch: Some(u64::from(contract.epoch).into()),
        contract_hash: Some(contract.hash),
        idempotency_key: params
            .get("idempotency_key")
            .and_then(Value::as_str)
            .map(str::to_owned),
        trace,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot create local CLI runtime: {error}"))?;
    match runtime.block_on(transport.dispatch_request(request)) {
        RuntimeToolResult::Success { result } => decode_tool_result(result),
        RuntimeToolResult::Failure {
            code,
            retryable,
            message,
            details,
        } => Err(format!(
            "local MCP call failed: code={code} retryable={retryable} message={message}{}",
            details
                .as_ref()
                .map(|value| format!(" details={}", compact_json(value)))
                .unwrap_or_default()
        )),
    }
}

fn decode_tool_result(result: Option<Value>) -> Result<Value, String> {
    let value = result.ok_or_else(|| "local MCP call returned no result".to_owned())?;
    let text = value
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .ok_or_else(|| "local MCP call returned an unexpected tool result".to_owned())?;
    serde_json::from_str(text)
        .map_err(|error| format!("local MCP tool result is invalid JSON: {error}"))
}

fn active_runtime_generation(config_dir: &std::path::Path) -> Result<Option<String>, String> {
    if let Ok(value) = std::env::var("HERDR_MCP_RUNTIME_GENERATION")
        && !value.trim().is_empty()
    {
        return Ok(Some(value));
    }
    let current = config_dir.join("runtime/current");
    #[cfg(target_os = "windows")]
    {
        if current.is_dir() {
            return Ok(fs::read_to_string(current.join("generation"))
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()));
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(target) = fs::read_link(&current) {
            return Ok(target
                .file_name()
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .map(str::to_owned));
        }
    }
    Ok(None)
}

fn insert_optional(map: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        map.insert(key.to_owned(), json!(value));
    }
}

fn repo_id_for_project_path(path: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|error| {
            format!(
                "cannot inspect Git origin for project path {}: {error}",
                path.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "project path {} has no readable Git origin",
            path.display()
        ));
    }
    let remote = String::from_utf8(output.stdout)
        .map_err(|_| "project Git origin is not valid UTF-8".to_owned())?;
    canonical_repo_id_from_remote(remote.trim()).ok_or_else(|| {
        format!(
            "project Git origin for {} cannot be mapped to a canonical Work Memory repo_id",
            path.display()
        )
    })
}

fn canonical_repo_id_from_remote(remote: &str) -> Option<String> {
    let remote = remote.trim().trim_end_matches('/');
    if remote.is_empty() {
        return None;
    }
    let (host, raw_path) = if remote.contains("://") {
        let url = Url::parse(remote).ok()?;
        let host = url.host_str()?.to_ascii_lowercase();
        (host, url.path().trim_matches('/').to_owned())
    } else {
        let (authority, path) = remote.split_once(':')?;
        let host = authority.rsplit('@').next()?.to_ascii_lowercase();
        (host, path.trim_matches('/').to_owned())
    };
    let raw_path = raw_path.strip_suffix(".git").unwrap_or(&raw_path);
    let mut parts = raw_path
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }
    if host == "github.com" {
        parts
            .iter_mut()
            .for_each(|part| part.make_ascii_lowercase());
    }
    let repo_id = format!("{host}/{}", parts.join("/"));
    crate::state_store::valid_canonical_work_memory_repo_id(&repo_id).then_some(repo_id)
}

fn print_private_result(value: Value) -> Result<ExitCode, String> {
    println!(
        "{}",
        serde_json::to_string_pretty(&value)
            .map_err(|error| format!("cannot encode local CLI result: {error}"))?
    );
    Ok(if value.get("ok").and_then(Value::as_bool) == Some(true) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn required_json_string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("browser resource is missing {key}"))
}

fn local_request_id() -> String {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros())
        .unwrap_or(0);
    format!("local-agent-cli-{}-{micros}", std::process::id())
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_trace_contains_only_browser_scope() {
        let grant = BrowserGrant {
            endpoint_ref: "ep_test".to_owned(),
            provider: "chatgpt".to_owned(),
            account_ref: "br_test".to_owned(),
        };
        let trace = json!({
            "webchat_control_grants": [{
                "endpoint_ref": grant.endpoint_ref,
                "provider": grant.provider,
                "account_ref": grant.account_ref,
            }]
        });
        assert_eq!(trace["webchat_control_grants"][0]["provider"], "chatgpt");
        assert!(trace.get("bearer_token").is_none());
    }

    const HANDOFF_SOURCE_URL: &str =
        "https://chatgpt.com/g/g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/c/source-conv";
    const HANDOFF_ID: &str = "hh_0123456789abcdef0123456789abcdef";

    fn canonical_handoff_message() -> String {
        format!(
            "继续 continuity_id hc:canonical。第一步调用 continuity.resume 恢复权威 journal。恢复后重新检查\
             目标设备上的实时 workspace / Git / runtime 状态。原会话：{HANDOFF_SOURCE_URL} \
             [HERDR_CONTINUITY_REF id={HANDOFF_ID} continuity_id=hc:canonical] \
             continuity_id: hc:canonical [END_HERDR_CONTINUITY_REF]"
        )
    }

    fn canonical_prepare_response(source_route: Value, source_route_error: Value) -> Value {
        let message = canonical_handoff_message();
        json!({
            "ok": true,
            "source_route": source_route,
            "source_route_error": source_route_error,
            "handoff": {
                "handoff_id": HANDOFF_ID,
                "continuity_id": "hc:canonical",
                "source_url": HANDOFF_SOURCE_URL,
                "message": message,
                "work_chain_id": Value::Null,
                "target_context": {"provider": "chatgpt", "project_id": "g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
            },
            "automatic_delivery": {
                "method": BROWSER_SESSION_CREATE_METHOD,
                "params": {
                    "source_url": HANDOFF_SOURCE_URL,
                    "message": message,
                    "work_chain_id": Value::Null,
                },
            },
            "manual_delivery": {"copy_prompt": message},
            "safety": {
                "pre_delivery_retry_limit": 1,
                "retry_requires_no_execution_evidence": true,
                "preserve_mutation_idempotency_key": true,
                "rewrite_rejected_payload": false,
            },
        })
    }

    fn canonical_source_route() -> Value {
        json!({
            "session_ref": "br_source",
            "provider": "chatgpt",
            "endpoint_ref": "bep_endpoint",
            "account_ref": "br_account",
            "space_ref": "br_space",
            "display_label": "herdr-mcp",
            "expected_generation": 7,
        })
    }

    fn applied_create_response(replayed: bool) -> Value {
        json!({
            "ok": true,
            "code": Value::Null,
            "operation": BROWSER_SESSION_CREATE_METHOD,
            "reservation_ref": "bsr_target",
            "reservation_state": "materialized",
            "session_ref": "br_target",
            "delivery_state": "applied",
            "dispatch": {
                "dispatch_id": "bd_target",
                "target_session_ref": "br_target",
                "delivery_state": "applied",
            },
            "replayed": replayed,
            "reconciled": false,
        })
    }

    struct FakeRuntime {
        prepare: Value,
        create: Value,
        calls: Vec<(String, Value, Option<BrowserGrant>)>,
    }

    impl FakeRuntime {
        fn new(prepare: Value, create: Value) -> Self {
            Self {
                prepare,
                create,
                calls: Vec::new(),
            }
        }

        fn call(
            &mut self,
            method: &str,
            params: Value,
            grant: Option<&BrowserGrant>,
        ) -> Result<Value, String> {
            self.calls.push((method.to_owned(), params, grant.cloned()));
            match method {
                BROWSER_HANDOFF_PREPARE_METHOD => Ok(self.prepare.clone()),
                BROWSER_SESSION_CREATE_METHOD => Ok(self.create.clone()),
                other => Err(format!("unexpected private method {other}")),
            }
        }

        fn create_calls(&self) -> Vec<&Value> {
            self.calls
                .iter()
                .filter(|(method, _, _)| method == BROWSER_SESSION_CREATE_METHOD)
                .map(|(_, params, _)| params)
                .collect()
        }
    }

    fn handoff_request(idempotency_key: Option<&str>, prepare_only: bool) -> WebChatHandoffRequest {
        WebChatHandoffRequest {
            continuity_id: "hc:canonical".to_owned(),
            source_url: HANDOFF_SOURCE_URL.to_owned(),
            objective: None,
            work_chain_id: None,
            handoff_id: None,
            idempotency_key: idempotency_key.map(str::to_owned),
            prepare_only,
        }
    }

    #[test]
    fn webchat_handoff_reuses_the_canonical_message_and_is_idempotent() {
        let mut runtime = FakeRuntime::new(
            canonical_prepare_response(canonical_source_route(), Value::Null),
            applied_create_response(false),
        );
        let first = webchat_handoff(handoff_request(None, false), |method, params, grant| {
            runtime.call(method, params, grant)
        })
        .unwrap();
        let message = canonical_handoff_message();
        assert_eq!(runtime.calls.len(), 2);
        assert_eq!(runtime.calls[0].0, BROWSER_HANDOFF_PREPARE_METHOD);
        assert!(runtime.calls[0].2.is_none());
        let create = runtime.create_calls()[0].clone();
        assert_eq!(create["source_url"], HANDOFF_SOURCE_URL);
        // The CLI must hand the canonical string through unchanged.
        assert_eq!(create["message"], message);
        assert_eq!(
            first["automatic_delivery"]["params"]["message"],
            first["manual_delivery"]["copy_prompt"]
        );
        assert_eq!(first["manual_delivery"]["copy_prompt"], message);
        // One logical handoff keeps one idempotency key, so a re-run cannot fork a conversation.
        assert_eq!(create["idempotency_key"], HANDOFF_ID);
        assert_eq!(create.get("work_chain_id"), None);
        let grant = runtime.calls[1].2.clone().unwrap();
        assert_eq!(grant.endpoint_ref, "bep_endpoint");
        assert_eq!(grant.account_ref, "br_account");
        assert_eq!(grant.provider, "chatgpt");
        assert_eq!(first["automatic_delivery"]["attempted"], true);
        assert_eq!(first["automatic_delivery"]["completed"], true);
        assert_eq!(first["automatic_delivery"]["delivery_state"], "applied");
        assert_eq!(first["automatic_delivery"]["session_ref"], "br_target");
        assert_eq!(first["automatic_delivery"]["dispatch_id"], "bd_target");
        assert_eq!(first["handoff"]["continuity_id"], "hc:canonical");
        assert!(
            first["instruction"]
                .as_str()
                .unwrap()
                .contains("automatically delivered")
        );

        runtime.create = applied_create_response(true);
        let replay = webchat_handoff(handoff_request(None, false), |method, params, grant| {
            runtime.call(method, params, grant)
        })
        .unwrap();
        let calls = runtime.create_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], calls[1]);
        assert_eq!(replay["automatic_delivery"]["replayed"], true);
        assert_eq!(replay["automatic_delivery"]["completed"], true);
    }

    #[test]
    fn webchat_handoff_keeps_the_copy_prompt_when_the_route_is_unavailable() {
        let mut runtime = FakeRuntime::new(
            canonical_prepare_response(Value::Null, json!("browser_source_session_not_found")),
            applied_create_response(false),
        );
        let output = webchat_handoff(handoff_request(None, false), |method, params, grant| {
            runtime.call(method, params, grant)
        })
        .unwrap();
        assert_eq!(runtime.create_calls().len(), 0);
        assert_eq!(output["ok"], true);
        assert_eq!(output["automatic_delivery"]["attempted"], false);
        assert_eq!(output["automatic_delivery"]["completed"], false);
        assert_eq!(
            output["automatic_delivery"]["reason"],
            "browser_source_session_not_found"
        );
        assert_eq!(
            output["manual_delivery"]["copy_prompt"],
            canonical_handoff_message()
        );
        assert!(
            output["instruction"]
                .as_str()
                .unwrap()
                .contains("not a completed handoff")
        );
    }

    #[test]
    fn webchat_handoff_does_not_retry_an_uncertain_delivery() {
        let mut runtime = FakeRuntime::new(
            canonical_prepare_response(canonical_source_route(), Value::Null),
            json!({
                "ok": false,
                "code": "uncertain",
                "reservation_ref": "bsr_target",
                "reservation_state": "reserved",
                "delivery_state": "uncertain",
                "replayed": false,
                "reconciled": false,
            }),
        );
        let output = webchat_handoff(handoff_request(None, false), |method, params, grant| {
            runtime.call(method, params, grant)
        })
        .unwrap();
        assert_eq!(runtime.create_calls().len(), 1);
        assert_eq!(output["automatic_delivery"]["attempted"], true);
        assert_eq!(output["automatic_delivery"]["completed"], false);
        assert_eq!(output["automatic_delivery"]["delivery_state"], "uncertain");
        assert_eq!(output["automatic_delivery"]["session_ref"], Value::Null);
        assert_eq!(
            output["automatic_delivery"]["result"]["reservation_ref"],
            "bsr_target"
        );
        assert!(output["manual_delivery"]["copy_prompt"].is_string());
        assert!(
            output["instruction"]
                .as_str()
                .unwrap()
                .contains("did not complete")
        );
    }

    #[test]
    fn webchat_handoff_prepare_only_and_explicit_keys_change_nothing_canonical() {
        let mut runtime = FakeRuntime::new(
            canonical_prepare_response(canonical_source_route(), Value::Null),
            applied_create_response(false),
        );
        let prepared = webchat_handoff(handoff_request(Some("ignored"), true), |m, p, g| {
            runtime.call(m, p, g)
        })
        .unwrap();
        assert_eq!(runtime.create_calls().len(), 0);
        assert_eq!(prepared["automatic_delivery"]["attempted"], false);
        assert_eq!(prepared["automatic_delivery"]["reason"], "prepare_only");
        assert_eq!(
            prepared["manual_delivery"]["copy_prompt"],
            canonical_handoff_message()
        );

        let mut runtime = FakeRuntime::new(
            canonical_prepare_response(canonical_source_route(), Value::Null),
            applied_create_response(false),
        );
        webchat_handoff(handoff_request(Some("explicit-key"), false), |m, p, g| {
            runtime.call(m, p, g)
        })
        .unwrap();
        assert_eq!(runtime.create_calls()[0]["idempotency_key"], "explicit-key");
        assert_eq!(
            runtime.create_calls()[0]["message"],
            canonical_handoff_message()
        );
    }

    #[test]
    fn webchat_handoff_refuses_a_divergent_or_unprepared_packet() {
        let mut divergent = canonical_prepare_response(canonical_source_route(), Value::Null);
        divergent["manual_delivery"]["copy_prompt"] = json!("a different handoff message");
        let mut runtime = FakeRuntime::new(divergent, applied_create_response(false));
        let error = webchat_handoff(handoff_request(None, false), |m, p, g| {
            runtime.call(m, p, g)
        })
        .unwrap_err();
        assert!(error.contains("divergent"), "unexpected error: {error}");
        assert_eq!(runtime.create_calls().len(), 0);

        let mut runtime = FakeRuntime::new(
            json!({"ok": false, "code": "continuity_not_found"}),
            applied_create_response(false),
        );
        let output = webchat_handoff(handoff_request(None, false), |m, p, g| {
            runtime.call(m, p, g)
        })
        .unwrap();
        assert_eq!(output["code"], "continuity_not_found");
        assert_eq!(runtime.create_calls().len(), 0);
    }

    #[test]
    fn canonical_repo_id_accepts_common_git_remote_forms() {
        for remote in [
            "git@github.com:WhShang/Herdr-MCP.git",
            "ssh://git@github.com/WhShang/Herdr-MCP.git",
            "https://github.com/WhShang/Herdr-MCP.git",
        ] {
            assert_eq!(
                canonical_repo_id_from_remote(remote).as_deref(),
                Some("github.com/whshang/herdr-mcp")
            );
        }
        assert_eq!(
            canonical_repo_id_from_remote("https://git.example.com/Team/Repo.git").as_deref(),
            Some("git.example.com/Team/Repo")
        );
        assert!(canonical_repo_id_from_remote("/tmp/repo").is_none());
    }
}
