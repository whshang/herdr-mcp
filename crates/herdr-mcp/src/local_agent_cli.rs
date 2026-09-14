use crate::cli::{ContinuityCommand, MemoryCommand, WebChatCommand};
use crate::link::local_mcp::{
    LinkRuntimeTransport, LocalMcpConfig, LocalMcpTransport, RuntimeToolResult,
};
use crate::link::request_core::RuntimeRequest;
use crate::paths::RuntimePaths;
use serde_json::{Map, Value, json};
use std::fs;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn run_continuity(command: ContinuityCommand) -> Result<ExitCode, String> {
    match command {
        ContinuityCommand::Search {
            query,
            project_id,
            workspace_id,
            limit,
        } => {
            let mut params = Map::new();
            params.insert("query".to_owned(), json!(query));
            insert_optional(&mut params, "project_id", project_id);
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
}
