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

pub fn build_info() -> Value {
    let started_at = started_at();
    let built_at = env::var("HERDR_MCP_BUILT_AT").unwrap_or_else(|_| started_at.clone());
    json!({
        "commit": env::var("HERDR_MCP_BUILD_COMMIT").unwrap_or_else(|_| "dev".to_owned()),
        "built_at": built_at,
        "started_at": started_at,
        "pid": std::process::id(),
        "server_version": env!("CARGO_PKG_VERSION"),
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
            exec.map(|registry| Value::Array(registry.list_views()))
                .unwrap_or_else(|| Value::Array(vec![])),
        );
        workstation.insert("exec_sessions_source".to_owned(), json!("rust-native"));
        workstation.insert("exec_sessions_ready".to_owned(), json!(exec.is_some()));
        workstation.insert(
            "exec_sessions_diagnostics".to_owned(),
            exec.map(ExecRegistry::diagnostics).unwrap_or(Value::Null),
        );
        workstation.insert("native_migration".to_owned(), migration_status());
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
    output.insert("version".to_owned(), json!(env!("CARGO_PKG_VERSION")));
    output.insert("build".to_owned(), build_info());
    output.insert("native_migration".to_owned(), migration_status());
    output.insert(
        "exec_sessions".to_owned(),
        exec.map(ExecRegistry::diagnostics).unwrap_or(Value::Null),
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
        assert_eq!(first["server_version"], env!("CARGO_PKG_VERSION"));
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
    }
}
