//! #1094 P0 — same-machine stdio shim (`herdr-mcp mcp-stdio`).
//!
//! Resident process bridging a caller's newline-delimited stdio JSON-RPC to
//! the loopback runtime `/mcp`. The runtime and device credentials are loaded
//! from the managed credential stores inside this process and are never
//! printed, logged, or echoed. No network listener is created.
//!
//! Routing (design `docs/design/1.3-direct-connect.md` §6):
//! - Everything ordinary forwards verbatim to `127.0.0.1:<port>/mcp` over one
//!   keep-alive `reqwest::blocking::Client` (connection reuse is the point of
//!   the direct path).
//! - `herdr_devices` forwards to Edge `GET /devices` with the enrolled device
//!   credential — never served from a local cache.
//! - Fleet-control and Edge-local `herdr_call` methods forward verbatim to
//!   Edge `/mcp` (device credential; the P0-b fallback mints a bound
//!   `device:<id>` principal there).
//! - The four write tools (`herdr_fs_write`/`herdr_fs_edit`/`herdr_fs_patch`/
//!   `herdr_prompt`) forward to Edge until the P0-a durable dedupe capability
//!   is negotiated — this build has no runtime capability advertisement for
//!   it yet, so they always take the Edge path.
//! - A mutating `herdr_call` without `client_request_id`, or any `herdr_call`
//!   carrying `idempotency_key`, is refused at the shim: the direct path has
//!   no Edge replay ledger to dedupe it.
//! - TUI backend only: a Core-backend config fails closed before any I/O.

use std::io::{BufRead, Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::Config;
use crate::paths::RuntimePaths;
use crate::runtime_adapter::RuntimeBackend;

/// Loopback calls may carry up to the runtime's own 60s prompt wait budget.
const LOOPBACK_TIMEOUT: Duration = Duration::from_secs(65);
const EDGE_TIMEOUT: Duration = Duration::from_secs(30);
/// One stdio frame or one forwarded response body.
const MAX_FRAME_BYTES: u64 = 16 * 1024 * 1024;
/// Stdin lines above the frame cap are rejected, not truncated.
const MAX_STDIN_LINE_BYTES: usize = MAX_FRAME_BYTES as usize;

/// The four write tools that must reach Edge until the P0-a durable dedupe
/// capability is merged AND negotiated on discover.
const WRITE_TOOLS_FORWARDED_TO_EDGE: [&str; 4] = [
    "herdr_fs_write",
    "herdr_fs_edit",
    "herdr_fs_patch",
    "herdr_prompt",
];

/// `herdr_call` inner methods that only exist on Edge (fleet control plus the
/// Edge-local fleet-administration surface). Forwarded verbatim; the Edge
/// device-credential path scopes the resulting principal.
const EDGE_FORWARD_PREFIXES: [&str; 3] = [
    "herdr_mcp.work_chain.",
    "herdr_mcp.planner_lease.",
    "herdr_mcp.execution_lane.",
];
const EDGE_FORWARD_EXACT: [&str; 4] = [
    "herdr_mcp.device.pair",
    "herdr_mcp.device.revoke",
    "herdr_mcp.connector.approve",
    "herdr_mcp.connector.revoke",
];

struct ShimConfig {
    endpoint: String,
    runtime_token: String,
    device_id: String,
    edge_origin: Option<String>,
    device_secret: Option<String>,
    contract_epoch: u64,
    contract_hash: String,
}

pub fn run(print_config: bool) -> Result<ExitCode, String> {
    let paths = RuntimePaths::discover()?;
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    if config.runtime_backend != RuntimeBackend::Tui {
        return Err(
            "mcp-stdio supports the TUI runtime backend only; the Core backend stays on the Edge path"
                .to_owned(),
        );
    }
    let device_id = config
        .edge_device_id
        .clone()
        .ok_or_else(|| "mcp-stdio requires an enrolled device (edge.device_id)".to_owned())?;
    let runtime_token = crate::service_manager::doctor_runtime_token()?
        .ok_or_else(|| "local Herdr-MCP runtime credential is unavailable".to_owned())?;
    let identity = crate::contract::identity()?;
    let edge_origin = config
        .edge_public_origin
        .as_deref()
        .map(|origin| origin.trim_end_matches('/').to_owned());
    // The device credential stays optional: without it the shim still serves
    // the loopback path, and Edge-bound calls report
    // `edge_credential_unsupported` instead of failing startup.
    let device_secret = edge_origin.as_deref().and_then(|origin| {
        crate::worker::stored_bootstrap_credential(&paths, origin, &device_id)
            .ok()
            .map(|credential| credential.device_secret)
    });
    let shim = ShimConfig {
        endpoint: format!("http://127.0.0.1:{}/mcp", config.runtime_port),
        runtime_token,
        device_id,
        edge_origin,
        device_secret,
        contract_epoch: u64::from(identity.epoch),
        contract_hash: identity.hash,
    };
    if print_config {
        return print_client_config(&shim);
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(LOOPBACK_TIMEOUT)
        .build()
        .map_err(|error| format!("cannot create shim HTTP client: {error}"))?;
    verify_loopback_identity(&client, &shim)?;

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!("herdr-mcp mcp-stdio: stdin read failed: {error}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_STDIN_LINE_BYTES {
            write_response(
                &mut out,
                &jsonrpc_error(
                    Value::Null,
                    -32600,
                    "request frame exceeds the shim size cap",
                ),
            );
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                write_response(&mut out, &jsonrpc_error(Value::Null, -32700, "Parse error"));
                continue;
            }
        };
        if let Some(response) = dispatch(&client, &shim, &request) {
            write_response(&mut out, &response);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn print_client_config(shim: &ShimConfig) -> Result<ExitCode, String> {
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "endpoint": shim.endpoint,
            "device_id": shim.device_id,
            "contract_epoch": shim.contract_epoch,
            "contract_hash": shim.contract_hash,
            "edge_origin": shim.edge_origin,
            "edge_credential": if shim.device_secret.is_some() { "enrolled" } else { "unavailable" },
            "credential": "runtime credential is read from the managed credential store or HERDR_MCP_TOKEN; it is never printed",
        }))
        .map_err(|error| format!("cannot encode client config: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

/// Boot-time discover handshake: the loopback endpoint must prove it is this
/// workstation's enrolled runtime on the shim's own contract. A different
/// device id or contract generation means the caller would silently talk to
/// the wrong surface, so the shim refuses to start and points back at Edge.
fn verify_loopback_identity(
    client: &reqwest::blocking::Client,
    shim: &ShimConfig,
) -> Result<(), String> {
    let body = post_json(
        client,
        &shim.endpoint,
        &[(
            "authorization".to_owned(),
            format!("Bearer {}", shim.runtime_token),
        )],
        &json!({"jsonrpc": "2.0", "id": "mcp-stdio-discover", "method": "server/discover", "params": {}}),
    )?;
    let meta = body
        .get("result")
        .and_then(|result| result.get("_meta"))
        .cloned()
        .unwrap_or(Value::Null);
    let advertised_device = meta.get("herdr_device_id").and_then(Value::as_str);
    if advertised_device != Some(shim.device_id.as_str()) {
        return Err(
            "loopback runtime does not advertise the enrolled herdr_device_id; use the Edge path instead"
                .to_owned(),
        );
    }
    let epoch_ok =
        meta.get("herdr_contract_epoch").and_then(Value::as_u64) == Some(shim.contract_epoch);
    let hash_ok = meta.get("herdr_contract_hash").and_then(Value::as_str)
        == Some(shim.contract_hash.as_str());
    if !epoch_ok || !hash_ok {
        return Err(
            "loopback runtime contract does not match this shim build; use the Edge path instead"
                .to_owned(),
        );
    }
    Ok(())
}

/// Returns the JSON-RPC response to write back, or `None` for notifications.
fn dispatch(
    client: &reqwest::blocking::Client,
    shim: &ShimConfig,
    request: &Value,
) -> Option<Value> {
    let is_notification = request.get("id").is_none();
    let request_id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str);

    let route = if method == Some("tools/call") {
        classify_tool_call(request)
    } else {
        Route::Loopback
    };
    let response = match route {
        Route::Loopback => forward_loopback(client, shim, request),
        Route::EdgeMcp => match edge_headers(shim) {
            Some(headers) => forward_edge_mcp(client, shim, &headers, request),
            None => Ok(tool_error_envelope(
                "edge_credential_unsupported",
                "the enrolled device credential is unavailable; fleet-control calls must use the Edge path",
            )),
        },
        Route::EdgeDevices => match edge_headers(shim) {
            Some(headers) => forward_edge_devices(client, shim, &headers, request_id.clone()),
            None => Ok(tool_error_envelope(
                "edge_credential_unsupported",
                "the enrolled device credential is unavailable; herdr_devices must use the Edge path",
            )),
        },
        Route::Reject(payload) => Ok(tool_error_value(&payload)),
    };

    if is_notification {
        return None;
    }
    Some(match response {
        Ok(ResponseBody::JsonRpc(value)) => value,
        Ok(ResponseBody::ToolResult(value)) => {
            json!({"jsonrpc": "2.0", "id": request_id, "result": value})
        }
        Err(message) => jsonrpc_error(request_id, -32603, &message),
    })
}

enum Route {
    Loopback,
    EdgeMcp,
    EdgeDevices,
    Reject(Value),
}

/// Response kinds: a verbatim upstream JSON-RPC document, or a tool result
/// payload the shim wraps into a `tools/call` response.
enum ResponseBody {
    JsonRpc(Value),
    ToolResult(Value),
}

fn classify_tool_call(request: &Value) -> Route {
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let raw_name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let name = crate::mcp::canonical_tool_name(raw_name);
    if name == "herdr_devices" {
        return Route::EdgeDevices;
    }
    if WRITE_TOOLS_FORWARDED_TO_EDGE.contains(&name) {
        return Route::EdgeMcp;
    }
    if name != "herdr_call" {
        return Route::Loopback;
    }
    let arguments = match params.get("arguments") {
        Some(Value::String(text)) => serde_json::from_str::<Value>(text).unwrap_or(Value::Null),
        other => other.cloned().unwrap_or(Value::Null),
    };
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    if arguments.contains_key("idempotency_key") {
        return Route::Reject(json!({
            "ok": false,
            "code": "direct_no_edge_replay_ledger",
            "retryable": false,
            "delivery_state": "not_delivered",
            "message": "herdr_call idempotency_key has no Edge replay ledger on the direct path; submit client_request_id instead",
        }));
    }
    let inner_method = arguments
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("");
    let edge_bound = EDGE_FORWARD_EXACT.contains(&inner_method)
        || EDGE_FORWARD_PREFIXES
            .iter()
            .any(|prefix| inner_method.starts_with(prefix));
    if edge_bound {
        return Route::EdgeMcp;
    }
    let mutating = !inner_method.is_empty()
        && !crate::durable_request_ledger::is_read_only_client_request_method(inner_method);
    if mutating && arguments.get("client_request_id").is_none() {
        return Route::Reject(json!({
            "ok": false,
            "code": "client_request_id_required",
            "retryable": false,
            "delivery_state": "not_delivered",
            "message": "mutating herdr_call on the direct path requires client_request_id for durable dedupe",
        }));
    }
    Route::Loopback
}

fn edge_headers(shim: &ShimConfig) -> Option<Vec<(String, String)>> {
    shim.edge_origin.as_ref()?;
    let secret = shim.device_secret.as_ref()?;
    Some(vec![
        ("authorization".to_owned(), format!("Bearer {secret}")),
        ("x-herdr-workstation".to_owned(), shim.device_id.clone()),
    ])
}

fn forward_loopback(
    client: &reqwest::blocking::Client,
    shim: &ShimConfig,
    request: &Value,
) -> Result<ResponseBody, String> {
    post_json(
        client,
        &shim.endpoint,
        &[(
            "authorization".to_owned(),
            format!("Bearer {}", shim.runtime_token),
        )],
        request,
    )
    .map(ResponseBody::JsonRpc)
}

fn forward_edge_mcp(
    client: &reqwest::blocking::Client,
    shim: &ShimConfig,
    headers: &[(String, String)],
    request: &Value,
) -> Result<ResponseBody, String> {
    let origin = shim.edge_origin.as_deref().unwrap_or_default();
    post_json(client, &format!("{origin}/mcp"), headers, request).map(ResponseBody::JsonRpc)
}

/// `herdr_devices` on the direct path reads Edge `GET /devices` (fleet
/// inventory; no local cache) and wraps the body as a tool text result.
fn forward_edge_devices(
    client: &reqwest::blocking::Client,
    shim: &ShimConfig,
    headers: &[(String, String)],
    _request_id: Value,
) -> Result<ResponseBody, String> {
    let origin = shim.edge_origin.as_deref().unwrap_or_default();
    let mut call = client
        .get(format!("{origin}/devices"))
        .timeout(EDGE_TIMEOUT)
        .header(reqwest::header::ACCEPT, "application/json");
    for (name, value) in headers {
        call = call.header(name.as_str(), value.as_str());
    }
    let response = call
        .send()
        .map_err(|error| format!("edge /devices unreachable: {error}"))?;
    let body = read_capped(response)?;
    let payload: Value = serde_json::from_slice(&body)
        .map_err(|_| "edge /devices returned a non-JSON body".to_owned())?;
    let is_error = payload.get("ok") != Some(&Value::Bool(true));
    Ok(ResponseBody::ToolResult(tool_result(payload, is_error)))
}

/// One bounded JSON POST. Returns the decoded body; transport failures and
/// non-JSON bodies surface as transport errors (never credential material).
fn post_json(
    client: &reqwest::blocking::Client,
    url: &str,
    headers: &[(String, String)],
    body: &Value,
) -> Result<Value, String> {
    let mut call = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .json(body);
    for (name, value) in headers {
        call = call.header(name.as_str(), value.as_str());
    }
    let response = call
        .send()
        .map_err(|error| format!("upstream request failed: {error}"))?;
    let status = response.status();
    let body = read_capped(response)?;
    let parsed: Value = serde_json::from_slice(&body)
        .map_err(|_| format!("upstream returned a non-JSON body (HTTP {status})"))?;
    if !status.is_success() {
        // Preserve an upstream JSON-RPC error verbatim when present so the
        // caller sees the real rejection code.
        if parsed.get("error").is_some() || parsed.get("result").is_some() {
            return Ok(parsed);
        }
        return Err(format!("upstream returned HTTP {status}"));
    }
    Ok(parsed)
}

fn read_capped(mut response: reqwest::blocking::Response) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    response
        .by_ref()
        .take(MAX_FRAME_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|error| format!("cannot read upstream body: {error}"))?;
    if body.len() as u64 > MAX_FRAME_BYTES {
        return Err("upstream response exceeds the shim frame cap".to_owned());
    }
    Ok(body)
}

fn tool_result(payload: Value, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": payload.to_string()}],
        "isError": is_error,
    })
}

fn tool_error_envelope(code: &str, message: &str) -> ResponseBody {
    ResponseBody::ToolResult(tool_result(
        json!({
            "ok": false,
            "code": code,
            "retryable": false,
            "delivery_state": "not_delivered",
            "message": message,
        }),
        true,
    ))
}

fn tool_error_value(payload: &Value) -> ResponseBody {
    ResponseBody::ToolResult(tool_result(payload.clone(), true))
}

fn jsonrpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn write_response(out: &mut impl Write, response: &Value) {
    if let Ok(text) = serde_json::to_string(response) {
        let _ = out.write_all(text.as_bytes());
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        })
    }

    #[test]
    fn write_tools_route_to_edge() {
        for tool in WRITE_TOOLS_FORWARDED_TO_EDGE {
            assert!(matches!(
                classify_tool_call(&call(tool, json!({}))),
                Route::EdgeMcp
            ));
        }
    }

    #[test]
    fn herdr_devices_routes_to_edge_rest() {
        assert!(matches!(
            classify_tool_call(&call("herdr_devices", json!({}))),
            Route::EdgeDevices
        ));
        assert!(matches!(
            classify_tool_call(&call("mcp__herdr-mcp__herdr_devices", json!({}))),
            Route::EdgeDevices
        ));
    }

    #[test]
    fn fleet_control_and_edge_local_methods_route_to_edge() {
        for method in [
            "herdr_mcp.work_chain.create",
            "herdr_mcp.planner_lease.takeover",
            "herdr_mcp.execution_lane.update",
            "herdr_mcp.device.pair",
            "herdr_mcp.connector.revoke",
        ] {
            assert!(
                matches!(
                    classify_tool_call(&call("herdr_call", json!({"method": method}))),
                    Route::EdgeMcp
                ),
                "{method} must forward to Edge"
            );
        }
    }

    #[test]
    fn herdr_call_with_idempotency_key_is_refused() {
        let route = classify_tool_call(&call(
            "herdr_call",
            json!({"method": "server.status", "idempotency_key": "k"}),
        ));
        let Route::Reject(payload) = route else {
            panic!("idempotency_key must be refused");
        };
        assert_eq!(payload["code"], "direct_no_edge_replay_ledger");
    }

    #[test]
    fn mutating_herdr_call_requires_client_request_id() {
        let route = classify_tool_call(&call(
            "herdr_call",
            json!({"method": "herdr_mcp.agent.task.dispatch"}),
        ));
        let Route::Reject(payload) = route else {
            panic!("mutating herdr_call without client_request_id must be refused");
        };
        assert_eq!(payload["code"], "client_request_id_required");

        let with_key = call(
            "herdr_call",
            json!({"method": "herdr_mcp.agent.task.dispatch", "client_request_id": "cr_1"}),
        );
        assert!(matches!(classify_tool_call(&with_key), Route::Loopback));
    }

    #[test]
    fn read_only_and_ordinary_calls_stay_loopback() {
        for method in [
            "server.status",
            "herdr_mcp.request.status",
            "herdr_mcp.skill.list",
        ] {
            assert!(matches!(
                classify_tool_call(&call("herdr_call", json!({"method": method}))),
                Route::Loopback
            ));
        }
        assert!(matches!(
            classify_tool_call(&call("herdr_fs_read", json!({"path": "/tmp/x"}))),
            Route::Loopback
        ));
        assert!(matches!(
            classify_tool_call(&call("herdr_exec", json!({"cmd": "ls"}))),
            Route::Loopback
        ));
    }

    #[test]
    fn stringified_arguments_are_parsed_for_policy() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "herdr_call",
                "arguments": "{\"method\":\"herdr_mcp.work_chain.create\",\"idempotency_key\":\"k\"}",
            },
        });
        // idempotency_key is checked before the Edge route, so this rejects
        // rather than forwarding a ledger-less mutation.
        assert!(matches!(classify_tool_call(&request), Route::Reject(_)));
    }
}
