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
use crate::utility_exec;
use serde_json::{Value, json};

pub const SDK_WIRE_PROTOCOL: &str = "2025-11-25";
pub const SERVER_INSTRUCTIONS: &str = "Herdr control plane for a WEB planner. Session start: herdr_inspect then herdr_skill once. Prefer deterministic herdr_fs_*/herdr_git/herdr_exec work before agent reasoning. Before unknown native API calls use herdr_methods, then herdr_call. Use explicit workspace/pane IDs and never blind-retry uncertain mutations.";

const SUPPORTED_VERSIONS: [&str; 5] = [
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];

pub struct RuntimeContext<'a> {
    pub client: &'a HerdrClient,
    pub cache: &'a EventCache,
    pub exec: &'a ExecRegistry,
    pub prompt: &'a PromptRegistry,
    pub skill: &'a SkillService,
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

fn initialize_result(request: &Value) -> Value {
    let requested = request
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(SDK_WIRE_PROTOCOL);
    let protocol = if SUPPORTED_VERSIONS.contains(&requested) {
        requested
    } else {
        SDK_WIRE_PROTOCOL
    };
    let identity = contract::identity().ok();
    json!({
        "protocolVersion": protocol,
        "capabilities": {"tools": {"listChanged": true}},
        "serverInfo": {
            "name": "herdr-mcp",
            "version": env!("CARGO_PKG_VERSION")
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
                "version": env!("CARGO_PKG_VERSION")
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
            native_tools::call_with_local(
                context.client,
                context.skill,
                &context.cache.snapshot(),
                method,
                params,
            )
        }
        "herdr_fs_read" => fs_tools::read(&context.cache.snapshot(), &arguments),
        "herdr_fs_list" => fs_tools::list(&context.cache.snapshot(), &arguments),
        "herdr_fs_grep" => fs_tools::grep(&context.cache.snapshot(), &arguments),
        "herdr_fs_image" => {
            return Ok(
                match fs_tools::image(&context.cache.snapshot(), &arguments) {
                    Ok(image) => image_tool_result(image),
                    Err(error) => tool_result(error, false),
                },
            );
        }
        "herdr_fs_edit" => fs_mutation::edit(&context.cache.snapshot(), &arguments),
        "herdr_fs_write" => fs_mutation::write(&context.cache.snapshot(), &arguments),
        "herdr_fs_patch" => fs_patch::apply(&context.cache.snapshot(), &arguments),
        "herdr_git" => git_tools::run(&context.cache.snapshot(), &arguments),
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
    }

    #[test]
    fn unsupported_protocol_negotiates_to_sdk_wire() {
        let result = initialize_result(&json!({
            "params": {"protocolVersion": "2026-07-28"}
        }));
        assert_eq!(result["protocolVersion"], SDK_WIRE_PROTOCOL);
    }

    #[test]
    fn discover_advertises_current_native_identity_and_versions() {
        let result = discover_result();
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["supportedVersions"][0], SDK_WIRE_PROTOCOL);
        assert_eq!(result["_meta"]["herdr_contract_epoch"], 2);
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
}
