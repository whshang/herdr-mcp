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

/// Ask the authorized stable broker to prove a protected exec root's `cd`
/// target is still the directory the topology declared. Any failure —
/// missing/outdated broker, resolution error, a real path that is not a
/// directory, or a real path that no longer spells the declared root — is
/// `not_delivered` so no pane is created and no command is sent.
fn verify_protected_exec_root(snapshot: &Value, root: &Path) -> Result<std::path::PathBuf, Value> {
    let response = match protected_exec_real_path(snapshot, root) {
        Ok(response) => response,
        Err(message) => {
            return Err(json!({
                "ok": false,
                "reason": "protected_root_unverifiable",
                "root": root.to_string_lossy(),
                "message": message,
                "delivery_state": "not_delivered",
            }));
        }
    };
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(json!({
            "ok": false,
            "reason": "protected_root_unverifiable",
            "root": root.to_string_lossy(),
            "broker": response,
            "delivery_state": "not_delivered",
        }));
    }
    let real = response
        .get("real")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from);
    let is_dir = response.get("is_dir").and_then(Value::as_bool) == Some(true);
    match real {
        Some(real) if is_dir && fs_security::same_declared_spelling(&real, root) => Ok(real),
        _ => Err(json!({
            "ok": false,
            "reason": "protected_root_identity_mismatch",
            "root": root.to_string_lossy(),
            "real": response.get("real").cloned().unwrap_or(Value::Null),
            "delivery_state": "not_delivered",
            "hint": "the protected root resolves somewhere other than the declared directory (a swapped or newly created symlink); refuse to create the exec pane — retry after refreshing herdr_inspect",
        })),
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn protected_exec_real_path(snapshot: &Value, root: &Path) -> Result<Value, String> {
    let config_dir = crate::paths::RuntimePaths::discover()
        .map_err(|error| format!("cannot discover runtime paths: {error}"))?
        .config_dir;
    if !crate::tcc_broker::installed_supports_real_path(&config_dir) {
        return Err(
            "the installed stable TCC broker predates real.path support (compat revision 6); run `herdr-mcp permissions setup --upgrade-broker`"
                .to_owned(),
        );
    }
    crate::tcc_broker::real_path_via_stable_broker(snapshot, root)
}

#[cfg(all(not(target_os = "macos"), not(test)))]
fn protected_exec_real_path(_snapshot: &Value, _root: &Path) -> Result<Value, String> {
    Err("protected exec transport is macOS-only".to_owned())
}

#[cfg(test)]
fn protected_exec_real_path(_snapshot: &Value, root: &Path) -> Result<Value, String> {
    if let Some(response) = TEST_REAL_PATH_RESPONSES
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&root.to_string_lossy().into_owned())
        .cloned()
    {
        return response;
    }
    #[cfg(target_os = "macos")]
    {
        let config_dir = crate::paths::RuntimePaths::discover()
            .map_err(|error| format!("cannot discover runtime paths: {error}"))?
            .config_dir;
        if !crate::tcc_broker::installed_supports_real_path(&config_dir) {
            return Err(
                "the installed stable TCC broker predates real.path support (compat revision 6); run `herdr-mcp permissions setup --upgrade-broker`"
                    .to_owned(),
            );
        }
        return crate::tcc_broker::real_path_via_stable_broker(_snapshot, root);
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(format!(
            "no test real.path response registered for {}",
            root.display()
        ))
    }
}

#[cfg(test)]
fn forced_protected_transport(root: &Path) -> bool {
    TEST_PROTECTED_ROOTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .contains(&root.to_string_lossy().into_owned())
}

#[cfg(not(test))]
fn forced_protected_transport(_root: &Path) -> bool {
    false
}

/// Roots forced through the protected exec transport in tests. Keyed by exact
/// path so a concurrent test's root is never forced — a process-wide env flag
/// would leak the protected classification into parallel tests.
#[cfg(test)]
static TEST_PROTECTED_ROOTS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<String>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

#[cfg(test)]
fn set_test_protected_root(root: &Path) {
    TEST_PROTECTED_ROOTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(root.to_string_lossy().into_owned());
}

#[cfg(test)]
fn clear_test_protected_roots() {
    TEST_PROTECTED_ROOTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
}

#[cfg(test)]
static TEST_REAL_PATH_RESPONSES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Result<Value, String>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
fn set_test_real_path_response(root: &Path, response: Result<Value, String>) {
    TEST_REAL_PATH_RESPONSES
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(root.to_string_lossy().into_owned(), response);
}

#[cfg(test)]
fn clear_test_real_path_responses() {
    TEST_REAL_PATH_RESPONSES
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
}

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
    let topology = projects::derive_routing_cached(snapshot);
    let mut verified_real: Option<std::path::PathBuf> = None;
    let protected_root =
        crate::macos_permissions::project_path_needs_protected_transport(Path::new(root))
            || forced_protected_transport(Path::new(root));
    let managed = match if protected_root {
        fs_security::validate_exact_validated_root_with_topology(&topology, root)
    } else {
        fs_security::validate_existing_validated_with_topology(&topology, root)
    } {
        Ok(value) => value,
        Err(error) => return error,
    };
    if protected_root {
        // The delegated `pane.split`/`cd` below executes in whatever the root
        // resolves to at that moment. `validate_exact_root_with_roots` is
        // lexical — it cannot see a protected root swapped for a symlink after
        // the topology was derived, so the authorized stable broker must
        // re-prove the cd target is the declared directory before any pane is
        // created. Every failure is not_delivered: no pane, no command. The
        // broker's real path is rebound inside the pane script so the
        // executed `cd` must land on the same directory identity — a swap
        // after this check cannot redirect where the command runs.
        verified_real = match verify_protected_exec_root(snapshot, &managed.root) {
            Ok(real) => Some(real),
            Err(error) => return error,
        };
    }
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
        utility_exec::start_private_pane_session(
            client,
            registry,
            workspace_id,
            &managed.real,
            &rendered,
            verified_real.as_deref(),
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
                let mut result = serde_json::Map::new();
                result.insert("ok".to_owned(), json!(false));
                result.insert("reason".to_owned(), json!("exec_start_failed"));
                result.insert("message".to_owned(), json!(message));
                result.insert("backend".to_owned(), json!("native"));
                result.insert("root".to_owned(), json!(managed.root.to_string_lossy()));
                result.insert("command".to_owned(), json!(rendered));
                result.insert("delivery_state".to_owned(), json!("unknown"));
                crate::exec_evidence::insert_uncertain_start(&mut result);
                return Value::Object(result);
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
/// Front-loaded micro-backoff for `exec.wait` polling: sessions that finish
/// within the first few hundred milliseconds surface on the early steps
/// instead of always paying the fixed 200ms poll, while long-running sessions
/// settle at the same 5 Hz steady-state cadence as before.
const EXEC_WAIT_POLL_BACKOFF_MS: [u64; 5] = [10, 20, 50, 100, 200];

/// Sleep duration for the Nth `exec.wait` poll (0-based). The caller still
/// truncates it to the remaining timeout, so timeout semantics are unchanged.
fn exec_wait_poll_delay(attempt: usize) -> Duration {
    Duration::from_millis(
        EXEC_WAIT_POLL_BACKOFF_MS[attempt.min(EXEC_WAIT_POLL_BACKOFF_MS.len() - 1)],
    )
}

pub fn wait(registry: &ExecRegistry, args: &Value) -> Value {
    wait_with_sleep(registry, args, thread::sleep)
}

/// `wait` with the sleep primitive injectable so tests can observe the
/// requested backoff cadence without paying real sleeps.
fn wait_with_sleep(
    registry: &ExecRegistry,
    args: &Value,
    mut sleep: impl FnMut(Duration),
) -> Value {
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
    let mut attempt = 0usize;

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
        sleep(exec_wait_poll_delay(attempt).min(timeout - elapsed));
        attempt = attempt.saturating_add(1);
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
    use super::{
        StartInvocation, clear_test_protected_roots, clear_test_real_path_responses,
        exec_wait_poll_delay, resolve_start_invocation, set_test_protected_root,
        set_test_real_path_response, start, wait, wait_view_ready,
    };
    use crate::exec_sessions::ExecRegistry;
    use crate::herdr::HerdrClient;
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn registry(name: &str) -> ExecRegistry {
        ExecRegistry::new(std::env::temp_dir().join(format!(
            "herdr-mcp-exec-{name}-{}-{}",
            std::process::id(),
            unique_suffix()
        )))
        .unwrap()
    }

    /// `herdr_exec` must run in the exact live vcs-less cwd instead of
    /// borrowing an unrelated managed Git root.
    #[test]
    fn exec_start_accepts_an_exact_vcs_less_operational_root() {
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-exec-operational-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let snapshot = json!({
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}],
            "agents": []
        });
        let registry = registry("operational");
        // The client is only used by the macOS protected-path branch; a
        // vcs-less temp root takes the native branch.
        let client = HerdrClient::new(PathBuf::from("/nonexistent-herdr.sock"));

        let started = start(
            &client,
            &snapshot,
            &registry,
            &json!({"root": root, "program": "pwd"}),
        );
        assert_eq!(started["ok"], true, "{started}");
        assert_eq!(started["root"], json!(root.to_string_lossy()));

        let session_id = started["session_id"].as_str().unwrap().to_owned();
        let view = wait(
            &registry,
            &json!({"session_id": session_id, "stream": "stdout", "timeout_ms": 10_000}),
        );
        let stdout = view["text"]
            .as_str()
            .or_else(|| view["stdout"].as_str())
            .unwrap_or_default();
        assert!(
            stdout.contains(root.file_name().unwrap().to_str().unwrap()),
            "exec must run in the operational root, saw: {view}"
        );

        // A sibling directory is still refused.
        let sibling = std::env::temp_dir().join(format!(
            "herdr-mcp-exec-sibling-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&sibling).unwrap();
        let refused = start(
            &client,
            &snapshot,
            &registry,
            &json!({"root": sibling, "program": "pwd"}),
        );
        assert_eq!(refused["reason"], "outside_managed_roots");

        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&sibling).unwrap();
    }

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
    fn exec_wait_poll_backoff_ramps_then_caps_at_200ms() {
        use std::time::Duration;
        let steps: Vec<Duration> = (0..8).map(exec_wait_poll_delay).collect();
        assert_eq!(
            steps,
            [
                Duration::from_millis(10),
                Duration::from_millis(20),
                Duration::from_millis(50),
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(200),
                Duration::from_millis(200),
                Duration::from_millis(200),
            ]
        );
    }

    #[test]
    fn exec_wait_requests_the_backoff_ladder_until_ready() {
        use std::time::Duration;
        let registry = registry("wait-backoff");
        let started = start(
            &HerdrClient::new(PathBuf::from("/nonexistent-herdr.sock")),
            &json!({"panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": "/tmp"}], "agents": []}),
            &registry,
            &json!({"root": "/tmp", "command": "sleep 0.05; printf done"}),
        );
        assert_eq!(started["ok"], true, "{started}");
        let session_id = started["session_id"].as_str().unwrap().to_owned();

        let mut requested = Vec::new();
        let view = super::wait_with_sleep(
            &registry,
            &json!({"session_id": session_id, "timeout_ms": 10_000}),
            |delay| {
                requested.push(delay);
                // Real 1ms sleeps let the session progress while keeping the
                // recorded sequence deterministic.
                std::thread::sleep(Duration::from_millis(1));
            },
        );
        assert_eq!(view["ok"], true, "{view}");
        assert_eq!(view["wait_timed_out"], false);
        assert!(
            !requested.is_empty(),
            "a ~50ms session must take at least one poll: {view}"
        );
        let ramp: Vec<Duration> = [10, 20, 50, 100]
            .iter()
            .map(|ms| Duration::from_millis(*ms))
            .collect();
        let prefix_len = requested.len().min(ramp.len());
        assert_eq!(
            requested[..prefix_len],
            ramp[..prefix_len],
            "requested delays must follow the ramp: {requested:?}"
        );
        assert!(
            requested[4.min(requested.len())..]
                .iter()
                .all(|delay| *delay == Duration::from_millis(200)),
            "delays past the ramp must stay capped at 200ms: {requested:?}"
        );
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

    /// user exec | Given a protected root swapped for a symlink after the
    /// topology was derived | When herdr_exec.start runs | Then the broker
    /// real.path verdict refuses the pane with not_delivered before any
    /// split.
    #[test]
    fn exec_start_protected_root_swapped_for_symlink_is_not_delivered() {
        let _guard = crate::test_env::lock();
        clear_test_real_path_responses();
        clear_test_protected_roots();
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-exec-protected-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&root).unwrap();
        set_test_protected_root(&root);
        let snapshot = json!({
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}],
            "agents": [],
            "workspaces": [{"workspace_id": "w1", "root": root}],
        });
        // The broker answers the real path of a DIFFERENT directory: the
        // lexical root was swapped for a symlink after the routing topology
        // was derived.
        set_test_real_path_response(
            &root,
            Ok(json!({
                "ok": true,
                "path": root.to_string_lossy(),
                "real": "/Users/example/other",
                "is_dir": true,
            })),
        );
        // No socket behind the client: a split attempt would surface as a
        // transport error, so a not_delivered refusal proves nothing ran.
        let client = HerdrClient::new(PathBuf::from("/nonexistent-herdr.sock"));
        let registry = registry("protected-swap");
        let refused = start(
            &client,
            &snapshot,
            &registry,
            &json!({"root": root, "program": "pwd"}),
        );
        assert_eq!(refused["ok"], false, "{refused}");
        assert_eq!(
            refused["reason"], "protected_root_identity_mismatch",
            "{refused}"
        );
        assert_eq!(refused["delivery_state"], "not_delivered", "{refused}");

        // A broker that cannot answer at all is not_delivered too.
        set_test_real_path_response(
            &root,
            Ok(json!({"ok": false, "reason": "resolve_failed", "os_error": 13})),
        );
        let refused = start(
            &client,
            &snapshot,
            &registry,
            &json!({"root": root, "program": "pwd"}),
        );
        assert_eq!(
            refused["reason"], "protected_root_unverifiable",
            "{refused}"
        );
        assert_eq!(refused["delivery_state"], "not_delivered", "{refused}");

        // When the broker proves the cd target is still the declared root
        // (the /System/Volumes/Data firmlink spelling counts as the same
        // directory), verification passes and execution proceeds to the next
        // delegated step — which fails closed on the unreachable client
        // instead of an identity refusal.
        set_test_real_path_response(
            &root,
            Ok(json!({
                "ok": true,
                "path": root.to_string_lossy(),
                "real": format!(
                    "/System/Volumes/Data{}",
                    root.to_string_lossy()
                ),
                "is_dir": true,
            })),
        );
        let proceeded = start(
            &client,
            &snapshot,
            &registry,
            &json!({"root": root, "program": "pwd"}),
        );
        assert_ne!(
            proceeded["reason"], "protected_root_identity_mismatch",
            "{proceeded}"
        );
        assert_ne!(
            proceeded["reason"], "protected_root_unverifiable",
            "{proceeded}"
        );

        clear_test_real_path_responses();
        clear_test_protected_roots();
        std::fs::remove_dir_all(&root).unwrap();
    }
}
