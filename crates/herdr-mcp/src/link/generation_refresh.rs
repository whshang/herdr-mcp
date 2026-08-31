//! Keep the production Rust Link aligned with the exact managed runtime generation.
//!
//! A service generation switch updates `runtime/current`. The production Link has
//! two additional generation references: its launchd environment and the live
//! runtime-control document. The running Link watches runtime-control and can
//! switch generations without dropping its Edge WebSocket; the plist is updated
//! only so a later natural Link restart starts with the same generation identity.

use crate::paths::RuntimePaths;

#[cfg(target_os = "macos")]
use plist::{Dictionary, Value as PlistValue};
#[cfg(target_os = "macos")]
use std::env;
#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use super::cutover_execute::{LaunchdOps, RealLaunchd, atomic_write};
#[cfg(target_os = "macos")]
use super::install::{configured_edge_ws_url, inherited_proxy_env};
#[cfg(target_os = "macos")]
use super::migrate_runtime_control::{
    active_rust_generation_id, read_binary_version_hint, reconcile_current_generation,
};
#[cfg(target_os = "macos")]
use super::ownership::{
    LINK_PROD_LABEL, LinkImplementation, assess_agent, program_points_at_managed_runtime,
    read_status_active_generation,
};

#[cfg(target_os = "macos")]
const ACTIVE_WAIT_BUDGET: Duration = Duration::from_secs(8);
#[cfg(target_os = "macos")]
const ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Reconcile production Link generation state after a successful service
/// generation change. Named instances never touch production Link state.
pub(crate) fn reconcile_after_service_generation_change(
    paths: &RuntimePaths,
) -> Result<(), String> {
    if paths.instance.is_named() {
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = paths;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        let home = home_dir()
            .ok_or_else(|| "HOME is required for Link generation reconcile".to_owned())?;
        let launchd = RealLaunchd;
        let loaded = launchd.is_loaded(LINK_PROD_LABEL)?;
        let prod = assess_agent(&home, LINK_PROD_LABEL, loaded);

        // A missing, stopped, Node-owned, or foreign prod Link is outside this
        // lifecycle. Service install must not bootstrap or seize it implicitly.
        if !prod.present
            || !prod.loaded
            || prod.implementation != LinkImplementation::Rust
            || !program_points_at_managed_runtime(&prod.program_arguments, &home)
        {
            return Ok(());
        }

        let generation = active_rust_generation_id(&home)?;
        let runtime_version = read_binary_version_hint(&home);
        // Update runtime-control first. The already-running Link polls this file and
        // hot-switches its local runtime generation while keeping the Edge WebSocket
        // alive. Updating the plist is persistence for the next natural restart; do
        // not bootout/bootstrap link-prod merely to refresh generation metadata.
        reconcile_current_generation(&home, &paths.config_dir)?;
        refresh_prod_plist_generation(
            &home,
            &prod.plist_path,
            &generation,
            runtime_version.as_deref(),
        )?;

        let status_path = prod
            .status_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| paths.config_dir.join("runtime-status-prod.json"));
        wait_for_active_generation(&status_path, &generation)
    }
}

#[cfg(target_os = "macos")]
fn refresh_prod_plist_generation(
    home: &Path,
    plist_path: &Path,
    generation: &str,
    runtime_version: Option<&str>,
) -> Result<bool, String> {
    let original = fs::read(plist_path)
        .map_err(|error| format!("cannot read {}: {error}", plist_path.display()))?;
    let mut plist = PlistValue::from_reader(std::io::Cursor::new(&original))
        .map_err(|error| format!("cannot parse {}: {error}", plist_path.display()))?;
    let dict = plist
        .as_dictionary_mut()
        .ok_or_else(|| "prod Link plist root must be a dict".to_owned())?;
    let label = dict
        .get("Label")
        .and_then(PlistValue::as_string)
        .unwrap_or("");
    if label != LINK_PROD_LABEL {
        return Err(format!(
            "refusing to refresh plist Label={label} (expected {LINK_PROD_LABEL})"
        ));
    }

    let existing_env = dict
        .get("EnvironmentVariables")
        .and_then(PlistValue::as_dictionary)
        .cloned()
        .unwrap_or_default();
    let mut env_out = Dictionary::new();
    for (key, value) in &existing_env {
        let upper = key.to_ascii_uppercase();
        let raw_secret =
            (upper.contains("TOKEN") || upper.contains("PASSWORD") || upper.ends_with("_SECRET"))
                && upper != "HERDR_LINK_KEYCHAIN_SERVICE";
        if !raw_secret {
            env_out.insert(key.clone(), value.clone());
        }
    }

    env_out.insert(
        "HERDR_RUNTIME_GENERATION".to_owned(),
        PlistValue::String(generation.to_owned()),
    );
    match runtime_version {
        Some(version) if !version.trim().is_empty() => {
            env_out.insert(
                "HERDR_RUNTIME_VERSION".to_owned(),
                PlistValue::String(version.trim().to_owned()),
            );
        }
        _ => {
            env_out.remove("HERDR_RUNTIME_VERSION");
        }
    }
    if let Some(edge_url) = configured_edge_ws_url(home) {
        env_out.insert("HERDR_EDGE_URL".to_owned(), PlistValue::String(edge_url));
    }
    for (key, value) in inherited_proxy_env() {
        if !env_out.contains_key(&key) {
            env_out.insert(key, PlistValue::String(value));
        }
    }
    if env_out == existing_env {
        return Ok(false);
    }
    dict.insert(
        "EnvironmentVariables".to_owned(),
        PlistValue::Dictionary(env_out),
    );

    let mut encoded = Vec::new();
    plist
        .to_writer_xml(&mut encoded)
        .map_err(|error| format!("cannot encode prod Link plist: {error}"))?;
    atomic_write(plist_path, &encoded, 0o600)?;
    Ok(true)
}

#[cfg(target_os = "macos")]
fn wait_for_active_generation(status_path: &Path, generation: &str) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if read_status_active_generation(status_path).as_deref() == Some(generation) {
            return Ok(());
        }
        if started.elapsed() >= ACTIVE_WAIT_BUDGET {
            let observed =
                read_status_active_generation(status_path).unwrap_or_else(|| "missing".to_owned());
            return Err(format!(
                "production Link did not activate generation {generation} within {}ms; observed={observed} status={}",
                ACTIVE_WAIT_BUDGET.as_millis(),
                status_path.display()
            ));
        }
        std::thread::sleep(ACTIVE_POLL_INTERVAL);
    }
}

#[cfg(target_os = "macos")]
fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn prod_plist_refresh_pins_generation_and_runtime_version_together() {
        let root = std::env::temp_dir().join(format!(
            "herdr-link-generation-refresh-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let plist_path = root.join("link-prod.plist");
        let mut env = Dictionary::new();
        env.insert(
            "HERDR_RUNTIME_GENERATION".to_owned(),
            PlistValue::String("rust-old".to_owned()),
        );
        env.insert(
            "HERDR_RUNTIME_VERSION".to_owned(),
            PlistValue::String("0.4.2".to_owned()),
        );
        let mut root_dict = Dictionary::new();
        root_dict.insert(
            "Label".to_owned(),
            PlistValue::String(LINK_PROD_LABEL.to_owned()),
        );
        root_dict.insert(
            "EnvironmentVariables".to_owned(),
            PlistValue::Dictionary(env),
        );
        let mut bytes = Vec::new();
        PlistValue::Dictionary(root_dict)
            .to_writer_xml(&mut bytes)
            .unwrap();
        std::fs::write(&plist_path, bytes).unwrap();

        assert!(
            refresh_prod_plist_generation(&root, &plist_path, "rust-new", Some("0.4.3-dev"),)
                .unwrap()
        );
        let updated = PlistValue::from_file(&plist_path).unwrap();
        let env = updated
            .as_dictionary()
            .unwrap()
            .get("EnvironmentVariables")
            .unwrap()
            .as_dictionary()
            .unwrap();
        assert_eq!(
            env.get("HERDR_RUNTIME_GENERATION")
                .and_then(PlistValue::as_string),
            Some("rust-new")
        );
        assert_eq!(
            env.get("HERDR_RUNTIME_VERSION")
                .and_then(PlistValue::as_string),
            Some("0.4.3-dev")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn named_instance_is_always_a_noop() {
        let _guard = crate::test_env::lock();
        let previous = std::env::var_os("HERDR_MCP_INSTANCE");
        unsafe { std::env::set_var("HERDR_MCP_INSTANCE", "uat") };
        let paths = RuntimePaths::discover().unwrap();
        assert!(paths.instance.is_named());
        reconcile_after_service_generation_change(&paths).unwrap();
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HERDR_MCP_INSTANCE", value),
                None => std::env::remove_var("HERDR_MCP_INSTANCE"),
            }
        }
    }
}
