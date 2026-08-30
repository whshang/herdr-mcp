use crate::exec_sessions::ExecRegistry;
use crate::fs_security;
use crate::mutation;
use crate::projects;
use serde_json::{Value, json};
use std::fs;

pub fn start(snapshot: &Value, registry: &ExecRegistry, args: &Value) -> Value {
    let root = match required_str(args, "root") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let command = match required_str(args, "command") {
        Ok("") => return invalid("command must not be empty"),
        Ok(value) => value,
        Err(error) => return error,
    };
    let confirm_busy = match optional_bool(args, "confirm_busy") {
        Ok(value) => value.unwrap_or(false),
        Err(error) => return error,
    };
    let topology = projects::derive_routing(snapshot);
    let managed = match fs_security::validate_existing_with_topology(&topology, root) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !managed.real.is_dir() {
        return json!({"ok": false, "reason": "not_a_directory", "root": managed.resolved.to_string_lossy()});
    }
    let expected = fs::canonicalize(&managed.root).unwrap_or_else(|_| managed.root.clone());
    if managed.real != expected {
        return json!({
            "ok": false,
            "reason": "root_not_project_root",
            "root": managed.resolved.to_string_lossy(),
            "project_root": managed.root.to_string_lossy(),
        });
    }
    let working =
        match mutation::check_with_topology(snapshot, &topology, &managed.root, confirm_busy) {
            Ok(value) => value,
            Err(error) => return error,
        };
    let workspace_id = projects::workspaces_for_root(&topology, &managed.root)
        .into_iter()
        .next();
    match registry.start_in_workspace(&managed.real, command, workspace_id.as_deref()) {
        Ok(mut result) => {
            if let Some(object) = result.as_object_mut() {
                object.insert("root".to_owned(), json!(managed.root.to_string_lossy()));
                object.insert(
                    "hint".to_owned(),
                    json!("poll herdr_exec_read with session_id until phase=completed; herdr_exec_kill when done"),
                );
                if !working.is_empty() {
                    object.insert("warnings".to_owned(), json!({"working": working}));
                }
            }
            result
        }
        Err(message) => json!({"ok": false, "reason": "exec_start_failed", "message": message}),
    }
}

pub fn read(registry: &ExecRegistry, args: &Value) -> Value {
    let id = match required_str(args, "session_id") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let stream = match optional_str(args, "stream") {
        Ok(value) => value.unwrap_or("both"),
        Err(error) => return error,
    };
    let offset = match optional_usize(args, "offset", 0, 9_007_199_254_740_991usize) {
        Ok(value) => value.unwrap_or(0),
        Err(error) => return error,
    };
    let limit = match optional_usize(args, "limit", 1, 262_144) {
        Ok(value) => value.unwrap_or(65_536),
        Err(error) => return error,
    };
    registry.read(id, stream, offset, limit)
}

pub fn kill(registry: &ExecRegistry, args: &Value) -> Value {
    let id = match required_str(args, "session_id") {
        Ok(value) => value,
        Err(error) => return error,
    };
    registry.kill(id)
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("{key} must be a string")))
}

fn optional_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(invalid(&format!("{key} must be a string"))),
    }
}

fn optional_bool(args: &Value, key: &str) -> Result<Option<bool>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(invalid(&format!("{key} must be a boolean"))),
    }
}

fn optional_usize(args: &Value, key: &str, min: usize, max: usize) -> Result<Option<usize>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value >= min && *value <= max)
        {
            Some(value) => Ok(Some(value)),
            None => Err(invalid(&format!(
                "{key} must be an integer in {min}..={max}"
            ))),
        },
    }
}

fn invalid(message: &str) -> Value {
    json!({"ok": false, "code": "invalid_params", "message": message})
}
