use crate::exec_sessions::ExecRegistry;
use crate::fs_security;
use crate::herdr::HerdrClient;
use crate::mutation;
use crate::projects;
use crate::utility_exec;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

pub fn start(
    client: &HerdrClient,
    snapshot: &Value,
    registry: &ExecRegistry,
    args: &Value,
) -> Value {
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
    let protected_root = crate::macos_permissions::is_protected_user_path(Path::new(root));
    let managed = match if protected_root {
        fs_security::validate_exact_project_root_with_topology(&topology, root)
    } else {
        fs_security::validate_existing_with_topology(&topology, root)
    } {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !protected_root {
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
    }
    let working =
        match mutation::check_with_topology(snapshot, &topology, &managed.root, confirm_busy) {
            Ok(value) => value,
            Err(error) => return error,
        };
    let workspace_id = projects::workspaces_for_root(&topology, &managed.root)
        .into_iter()
        .next();
    let mut result = if protected_root {
        let Some(workspace_id) = workspace_id.as_deref() else {
            return json!({
                "ok": false,
                "reason": "workspace_required_for_protected_exec",
                "root": managed.root.to_string_lossy(),
                "delivery_state": "not_delivered",
            });
        };
        utility_exec::start_reusable_pane_session(
            client,
            snapshot,
            registry,
            workspace_id,
            &managed.real,
            command,
        )
    } else {
        match registry.start_native(&managed.real, command) {
            Ok(value) => value,
            Err(message) => {
                return json!({"ok": false, "reason": "exec_start_failed", "message": message});
            }
        }
    };
    if result.get("ok").and_then(Value::as_bool) != Some(true) {
        return result;
    }
    let Some(object) = result.as_object_mut() else {
        return result;
    };
    object.insert("root".to_owned(), json!(managed.root.to_string_lossy()));
    object.insert(
        "hint".to_owned(),
        json!(
            "poll herdr_exec_read with session_id until phase=completed; herdr_exec_kill when done"
        ),
    );
    if !working.is_empty() {
        object.insert("warnings".to_owned(), json!({"working": working}));
    }
    result
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

const EXEC_WAIT_DEFAULT_TIMEOUT_MS: usize = 10_000;
const EXEC_WAIT_MAX_TIMEOUT_MS: usize = 20_000;
const EXEC_WAIT_POLL_MS: u64 = 200;

pub fn wait(registry: &ExecRegistry, args: &Value) -> Value {
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
    let timeout_ms = match optional_usize(args, "timeout_ms", 1, EXEC_WAIT_MAX_TIMEOUT_MS) {
        Ok(value) => value.unwrap_or(EXEC_WAIT_DEFAULT_TIMEOUT_MS),
        Err(error) => return error,
    };
    let started = Instant::now();
    let timeout = Duration::from_millis(timeout_ms as u64);

    loop {
        let mut view = registry.read(id, stream, offset, limit);
        if wait_view_ready(&view, offset) {
            annotate_wait(&mut view, started.elapsed(), false);
            return view;
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            annotate_wait(&mut view, elapsed, true);
            return view;
        }
        thread::sleep(Duration::from_millis(EXEC_WAIT_POLL_MS).min(timeout - elapsed));
    }
}

fn wait_view_ready(view: &Value, offset: usize) -> bool {
    if view.get("ok").and_then(Value::as_bool) == Some(false) {
        return true;
    }
    if view.get("running").and_then(Value::as_bool) == Some(false)
        || view.get("phase").and_then(Value::as_str) == Some("completed")
    {
        return true;
    }
    view.get("next_offset")
        .and_then(Value::as_u64)
        .is_some_and(|next| next > offset as u64)
}

fn annotate_wait(view: &mut Value, elapsed: Duration, timed_out: bool) {
    if let Some(object) = view.as_object_mut() {
        object.insert(
            "waited_ms".to_owned(),
            json!(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)),
        );
        object.insert("wait_timed_out".to_owned(), json!(timed_out));
    }
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

#[cfg(test)]
mod tests {
    use super::wait_view_ready;
    use serde_json::json;

    #[test]
    fn exec_wait_stops_only_for_output_completion_or_error() {
        assert!(!wait_view_ready(
            &json!({"ok": true, "running": true, "phase": "running", "next_offset": 10}),
            10,
        ));
        assert!(wait_view_ready(
            &json!({"ok": true, "running": true, "phase": "running", "next_offset": 11}),
            10,
        ));
        assert!(wait_view_ready(
            &json!({"ok": true, "running": false, "phase": "completed", "next_offset": 10}),
            10,
        ));
        assert!(wait_view_ready(
            &json!({"ok": false, "code": "missing_session"}),
            10
        ));
    }
}
