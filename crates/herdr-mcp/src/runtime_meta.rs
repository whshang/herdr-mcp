use crate::contract;
use crate::exec_sessions::ExecRegistry;
use crate::state_cache::EventCache;
use serde_json::{Map, Value, json};
use std::env;
use std::sync::OnceLock;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const MIGRATED_TOOLS: [&str; 18] = [
    "herdr_methods",
    "herdr_inspect",
    "herdr_call",
    "herdr_since",
    "herdr_fs_read",
    "herdr_fs_list",
    "herdr_fs_grep",
    "herdr_fs_image",
    "herdr_fs_edit",
    "herdr_fs_write",
    "herdr_fs_patch",
    "herdr_git",
    "herdr_exec_start",
    "herdr_exec_read",
    "herdr_exec_kill",
    "herdr_exec",
    "herdr_prompt",
    "herdr_skill",
];

static STARTED_AT: OnceLock<String> = OnceLock::new();

pub fn runtime_channel() -> &'static str {
    match option_env!("HERDR_MCP_BUILD_CHANNEL") {
        Some("dev") => "dev",
        _ => "prod",
    }
}

pub fn runtime_version() -> &'static str {
    match option_env!("HERDR_MCP_BUILD_VERSION") {
        Some(value) if !value.is_empty() => value,
        _ => env!("CARGO_PKG_VERSION"),
    }
}

pub fn compiled_source_commit() -> Option<&'static str> {
    match option_env!("HERDR_MCP_BUILD_COMMIT") {
        Some(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

pub fn compiled_source_dirty() -> bool {
    matches!(option_env!("HERDR_MCP_BUILD_DIRTY"), Some("1" | "true"))
}

pub fn build_info() -> Value {
    let started_at = started_at();
    let built_at = env::var("HERDR_MCP_BUILT_AT").unwrap_or_else(|_| started_at.clone());
    let commit = compiled_source_commit()
        .map(str::to_owned)
        .or_else(|| env::var("HERDR_MCP_BUILD_COMMIT").ok())
        .unwrap_or_else(|| "unknown".to_owned());
    json!({
        "commit": commit,
        "built_at": built_at,
        "started_at": started_at,
        "pid": std::process::id(),
        "server_version": runtime_version(),
        "package_version": env!("CARGO_PKG_VERSION"),
        "channel": runtime_channel(),
        "source_commit": compiled_source_commit(),
        "source_dirty": compiled_source_dirty(),
        "runtime": "rust",
        "stale": false,
    })
}

pub fn migration_status() -> Value {
    let all = contract::tool_names();
    let pending = all
        .iter()
        .copied()
        .filter(|name| !MIGRATED_TOOLS.contains(name))
        .collect::<Vec<_>>();
    let sealed = env::var_os("HOME")
        .map(|home| {
            crate::link::seal::production_ready_from_seal(
                &std::path::PathBuf::from(home)
                    .join(".config")
                    .join("herdr-mcp"),
            )
        })
        .unwrap_or(false);
    json!({
        "phase": if sealed { "production" } else { "candidate" },
        "native_parity_ready": pending.is_empty(),
        "production_ready": sealed,
        "link_cutover": crate::link::production_ready_gate_catalog(),
        "contract_epoch": contract::identity().ok().map(|identity| identity.epoch),
        "tool_count": all.len(),
        "migrated_tool_count": MIGRATED_TOOLS.len(),
        "pending_tool_count": pending.len(),
        "migrated_tools": MIGRATED_TOOLS,
        "pending_tools": pending,
    })
}

pub fn augment_inspect(view: &mut Value, cache: Option<&EventCache>, exec: Option<&ExecRegistry>) {
    let Some(object) = view.as_object_mut() else {
        return;
    };
    object.insert("build".to_owned(), build_info());
    object.insert("native_migration".to_owned(), migration_status());
    if let Some(workstation) = object
        .get_mut("workstation_info")
        .and_then(Value::as_object_mut)
    {
        workstation.insert(
            "boot_id".to_owned(),
            cache
                .map(|cache| json!(cache.boot_id()))
                .unwrap_or(Value::Null),
        );
        workstation.insert(
            "exec_sessions".to_owned(),
            exec.map(|registry| Value::Array(compact_exec_session_views(registry.list_views())))
                .unwrap_or_else(|| Value::Array(vec![])),
        );
        workstation.insert("exec_sessions_source".to_owned(), json!("rust-native"));
        workstation.insert("exec_sessions_ready".to_owned(), json!(exec.is_some()));
        workstation.insert(
            "exec_sessions_diagnostics".to_owned(),
            exec.map(ExecRegistry::diagnostics).unwrap_or(Value::Null),
        );
        workstation.insert("native_migration".to_owned(), migration_status());
        // Non-sensitive web-artifact metadata only. Never includes token, cookie,
        // or download URL material. On cache discovery/read failure we surface a
        // terse readiness code and an empty list, never the raw error path.
        let artifact_ready = crate::paths::RuntimePaths::discover().is_ok();
        let artifacts = if artifact_ready {
            let config_dir = crate::paths::RuntimePaths::discover()
                .map(|paths| paths.config_dir)
                .ok();
            config_dir
                .and_then(|dir| crate::web_artifact_cache::list(&dir).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        workstation.insert(
            "web_artifacts".to_owned(),
            json!({
                "enabled": artifact_ready,
                "entry_count": artifacts.len(),
                "artifacts": artifacts,
            }),
        );
    }
}

fn compact_exec_session_views(views: Vec<Value>) -> Vec<Value> {
    const RECENT_CLOSED: usize = 3;
    let mut selected = views
        .iter()
        .filter(|view| view.get("running").and_then(Value::as_bool) == Some(true))
        .cloned()
        .collect::<Vec<_>>();
    selected.extend(
        views
            .iter()
            .rev()
            .filter(|view| view.get("running").and_then(Value::as_bool) != Some(true))
            .take(RECENT_CLOSED)
            .cloned(),
    );
    selected.sort_by(|left, right| {
        left.get("started_at")
            .and_then(Value::as_str)
            .cmp(&right.get("started_at").and_then(Value::as_str))
    });
    selected
        .into_iter()
        .map(|mut view| {
            if let Some(command) = view.get("command").and_then(Value::as_str) {
                view["command"] = json!(redact_command_summary(command));
            }
            view
        })
        .collect()
}

fn redact_command_summary(command: &str) -> String {
    const SECRET_FLAGS: &[&str] = &[
        "--api-key",
        "--token",
        "--access-token",
        "--auth-token",
        "--refresh-token",
        "--password",
        "--secret",
        "--client-secret",
        "--credential",
        "--client-id",
        "--app-id",
        "--release-id",
        "--profile",
        "--tenant-key",
    ];
    let mut output = Vec::new();
    let mut redact_next = false;
    for token in command.split_whitespace() {
        if redact_next {
            output.push("<redacted>".to_owned());
            redact_next = false;
            continue;
        }
        if SECRET_FLAGS.contains(&token) {
            output.push(token.to_owned());
            redact_next = true;
            continue;
        }
        if let Some(flag) = SECRET_FLAGS
            .iter()
            .find(|flag| token.starts_with(&format!("{flag}=")))
        {
            output.push(format!("{flag}=<redacted>"));
            continue;
        }
        if let Some((key, _)) = token.split_once('=') {
            let key = key.to_ascii_uppercase();
            if key.ends_with("_TOKEN")
                || key.ends_with("_SECRET")
                || key.ends_with("_PASSWORD")
                || key.ends_with("_API_KEY")
                || key.ends_with("_ACCESS_TOKEN")
                || key.ends_with("_AUTH_TOKEN")
                || key.ends_with("_REFRESH_TOKEN")
                || key.ends_with("_CLIENT_SECRET")
                || key.ends_with("_CREDENTIAL")
                || key.ends_with("_CLIENT_ID")
                || key.ends_with("_APP_ID")
                || key.ends_with("_RELEASE_ID")
                || key.ends_with("_PROFILE")
                || key.ends_with("_TENANT_KEY")
            {
                output.push(format!("{key}=<redacted>"));
                continue;
            }
        }
        output.push(token.to_owned());
    }
    let summary = output.join(" ");
    if summary.chars().count() <= 160 {
        summary
    } else {
        format!("{}…", summary.chars().take(159).collect::<String>())
    }
}

pub fn health_fields(cache: &EventCache, exec: Option<&ExecRegistry>) -> Map<String, Value> {
    let diagnostics = cache.diagnostics();
    let sealed = env::var_os("HOME")
        .map(|home| {
            crate::link::seal::production_ready_from_seal(
                &std::path::PathBuf::from(home)
                    .join(".config")
                    .join("herdr-mcp"),
            )
        })
        .unwrap_or(false);
    let mut output = Map::new();
    output.insert(
        "runtime".to_owned(),
        json!(if sealed { "rust" } else { "rust-candidate" }),
    );
    output.insert("version".to_owned(), json!(runtime_version()));
    output.insert("channel".to_owned(), json!(runtime_channel()));
    if let Some(generation) = env::var_os("HERDR_MCP_RUNTIME_GENERATION")
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
    {
        output.insert("runtime_generation".to_owned(), json!(generation));
    }
    output.insert("build".to_owned(), build_info());
    output.insert("native_migration".to_owned(), migration_status());
    output.insert(
        "exec_sessions".to_owned(),
        exec.map(ExecRegistry::health_diagnostics)
            .unwrap_or(Value::Null),
    );
    output.insert(
        "event_cache".to_owned(),
        json!({
            "boot_id": cache.boot_id(),
            "event_count": diagnostics.event_count,
            "last_event_at": diagnostics.last_event_at,
            "needs_reconcile": diagnostics.needs_reconcile,
        }),
    );
    output
}

fn started_at() -> String {
    STARTED_AT
        .get_or_init(|| {
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn with_empty_home<F>(run: F)
    where
        F: FnOnce(),
    {
        let _env_guard = crate::test_env::lock();
        let root = std::env::temp_dir().join(format!(
            "herdr-runtime-meta-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", &root);
        }
        run();
        unsafe {
            match previous_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn migration_status_is_derived_from_epoch2_catalog() {
        with_empty_home(|| {
            let status = migration_status();
            assert_eq!(status["tool_count"], 18);
            assert_eq!(status["migrated_tool_count"], 18);
            assert_eq!(status["pending_tool_count"], 0);
            assert_eq!(status["native_parity_ready"], true);
            assert_eq!(status["production_ready"], false);
            assert_eq!(status["link_cutover"]["production_ready"], false);
            assert!(
                status["link_cutover"]["requires_all"]
                    .as_array()
                    .is_some_and(|gates| !gates.is_empty())
            );
            assert_eq!(status["pending_tools"], json!([]));
            for name in MIGRATED_TOOLS {
                assert!(contract::tool_names().contains(&name));
            }
        });
    }

    #[test]
    fn build_info_is_stable_within_process() {
        let first = build_info();
        let second = build_info();
        assert_eq!(first["started_at"], second["started_at"]);
        assert_eq!(first["server_version"], runtime_version());
        assert_eq!(first["channel"], runtime_channel());
        assert_eq!(first["package_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(first["runtime"], "rust");
    }

    #[test]
    fn inspect_augmentation_is_explicit_about_pending_exec_registry() {
        let mut view = json!({"ok": true, "workstation_info": {"server_name": "herdr-mcp"}});
        augment_inspect(&mut view, None, None);
        assert_eq!(view["build"]["runtime"], "rust");
        assert_eq!(view["native_migration"]["migrated_tool_count"], 18);
        assert_eq!(view["workstation_info"]["boot_id"], Value::Null);
        assert_eq!(view["workstation_info"]["exec_sessions"], json!([]));
        assert_eq!(
            view["workstation_info"]["exec_sessions_source"],
            "rust-native"
        );
        assert_eq!(view["workstation_info"]["exec_sessions_ready"], false);
        // Non-sensitive web-artifact readiness block must never leak secrets.
        let artifacts = &view["workstation_info"]["web_artifacts"];
        assert!(artifacts.get("enabled").is_some());
        assert_eq!(artifacts["entry_count"], 0);
        for secret in [
            "bearer",
            "cookie",
            "accesstoken",
            "authorization",
            "download_url",
        ] {
            let serialized = serde_json::to_string(&view).unwrap().to_ascii_lowercase();
            assert!(
                !serialized.contains(secret),
                "serialized inspect must not contain {secret}"
            );
        }
    }

    #[test]
    fn exec_session_summary_keeps_running_plus_recent_and_redacts_commands() {
        let views = vec![
            json!({"session_id":"old","running":false,"started_at":"2026-01-01T00:00:00Z","command":"echo old"}),
            json!({"session_id":"recent-a","running":false,"started_at":"2026-01-02T00:00:00Z","command":"API_TOKEN=secret tool --api-key hidden"}),
            json!({"session_id":"recent-b","running":false,"started_at":"2026-01-03T00:00:00Z","command":"tool --app-id app_example --release-id release_example --profile profile_example"}),
            json!({"session_id":"recent-c","running":false,"started_at":"2026-01-04T00:00:00Z","command":"echo c"}),
            json!({"session_id":"running","running":true,"started_at":"2026-01-01T12:00:00Z","command":"tool --token=live-secret"}),
        ];
        let compact = compact_exec_session_views(views);
        assert_eq!(compact.len(), 4);
        assert!(compact.iter().any(|view| view["session_id"] == "running"));
        assert!(!compact.iter().any(|view| view["session_id"] == "old"));
        let text = serde_json::to_string(&compact).unwrap();
        assert!(!text.contains("live-secret"));
        assert!(!text.contains("hidden"));
        assert!(!text.contains("API_TOKEN=secret"));
        assert!(!text.contains("app_example"));
        assert!(!text.contains("release_example"));
        assert!(!text.contains("profile_example"));
    }
}
