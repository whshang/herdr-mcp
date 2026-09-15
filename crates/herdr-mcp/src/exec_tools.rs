use crate::exec_sessions::{ExecRegistry, render_exec_argv};
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

const MAX_TYPED_ARGS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
enum StartInvocation {
    Command(String),
    Program { program: String, args: Vec<String> },
}

impl StartInvocation {
    fn rendered(&self) -> String {
        match self {
            Self::Command(command) => command.clone(),
            Self::Program { program, args } => render_exec_argv(program, args),
        }
    }
}

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
    let invocation = match resolve_start_invocation(args) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let confirm_busy = match optional_bool(args, "confirm_busy") {
        Ok(value) => value.unwrap_or(false),
        Err(error) => return error,
    };
    let topology = projects::derive_routing(snapshot);
    let protected_root =
        crate::macos_permissions::project_path_needs_protected_transport(Path::new(root));
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
    let rendered = invocation.rendered();
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
            &rendered,
        )
    } else {
        let started = match &invocation {
            StartInvocation::Command(command) => registry.start_native(&managed.real, command),
            StartInvocation::Program { program, args } => {
                registry.start_native_program(&managed.real, program, args)
            }
        };
        match started {
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
            "for long sessions, prefer private herdr_mcp.exec.wait via herdr_call with a 10-20s bounded wait when advertised; use herdr_exec_read for immediate/delta output or when exec.wait is unavailable; herdr_exec_kill when done"
        ),
    );
    if !working.is_empty() {
        object.insert("warnings".to_owned(), json!({"working": working}));
    }
    result
}

fn resolve_start_invocation(args: &Value) -> Result<StartInvocation, Value> {
    let command = strict_optional_str(args, "command")?;
    let program = strict_optional_str(args, "program")?;
    let raw_args = args.get("args");

    if command.is_some() && (program.is_some() || raw_args.is_some()) {
        return Err(invalid(
            "command and program/args modes are mutually exclusive",
        ));
    }
    if let Some(command) = command {
        if command.is_empty() {
            return Err(invalid("command must not be empty"));
        }
        return Ok(StartInvocation::Command(command.to_owned()));
    }

    if let Some(program) = program {
        if program.is_empty() || program.contains('\0') {
            return Err(invalid("program must be non-empty and contain no NUL"));
        }
        let args = parse_typed_args(raw_args)?;
        return Ok(StartInvocation::Program {
            program: program.to_owned(),
            args,
        });
    }

    if raw_args.is_some() {
        return Err(invalid("program is required when args is provided"));
    }
    Err(invalid("exactly one of command or program is required"))
}

fn parse_typed_args(value: Option<&Value>) -> Result<Vec<String>, Value> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(invalid("args must be an array of strings"));
    };
    if values.len() > MAX_TYPED_ARGS {
        return Err(invalid(&format!(
            "args must contain at most {MAX_TYPED_ARGS} strings"
        )));
    }
    let mut args = Vec::with_capacity(values.len());
    for value in values {
        let Some(value) = value.as_str() else {
            return Err(invalid("args must be an array of strings"));
        };
        if value.contains('\0') {
            return Err(invalid("args must contain only NUL-free strings"));
        }
        args.push(value.to_owned());
    }
    Ok(args)
}

fn strict_optional_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, Value> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(invalid(&format!("{key} must be a string"))),
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
    use super::{StartInvocation, resolve_start_invocation, wait_view_ready};
    use serde_json::json;

    #[test]
    fn exec_start_keeps_legacy_command_and_defaults_typed_args() {
        assert_eq!(
            resolve_start_invocation(&json!({"command": "printf legacy"})).unwrap(),
            StartInvocation::Command("printf legacy".to_owned())
        );
        let typed = resolve_start_invocation(&json!({"program": "printf"})).unwrap();
        assert_eq!(
            typed,
            StartInvocation::Program {
                program: "printf".to_owned(),
                args: Vec::new(),
            }
        );
        assert_eq!(typed.rendered(), "'printf'");
    }

    #[test]
    fn exec_start_typed_mode_is_literal_and_mutually_exclusive() {
        let typed = resolve_start_invocation(&json!({
            "program": "printf",
            "args": ["%s\\n", "a; $(uname) *", "it's literal"]
        }))
        .unwrap();
        assert_eq!(
            typed.rendered(),
            "'printf' '%s\\n' 'a; $(uname) *' 'it'\\''s literal'"
        );

        for invalid_args in [
            json!({"command": "printf legacy", "program": "printf"}),
            json!({"command": "printf legacy", "args": []}),
            json!({"args": ["orphan"]}),
            json!({"program": "printf", "args": "not-an-array"}),
        ] {
            assert_eq!(
                resolve_start_invocation(&invalid_args).unwrap_err()["code"],
                "invalid_params"
            );
        }
    }

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
