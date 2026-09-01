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
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

use super::cutover_execute::LaunchdOps;
#[cfg(target_os = "macos")]
use super::cutover_execute::{RealLaunchd, atomic_write};
#[cfg(target_os = "macos")]
use super::install::{
    configured_edge_device_identity, configured_edge_ws_url, inherited_proxy_env,
};
#[cfg(target_os = "macos")]
use super::migrate_runtime_control::{
    active_rust_generation_id, read_binary_version_hint, reconcile_current_generation,
};
use super::ownership::LINK_PROD_LABEL;
#[cfg(target_os = "macos")]
use super::ownership::{
    LinkImplementation, assess_agent, program_points_at_managed_runtime,
    read_status_active_generation,
};

const ACTIVE_WAIT_BUDGET: Duration = Duration::from_secs(8);
const ACTIVE_RECONCILE_ATTEMPTS: usize = 2;
const POST_KICKSTART_WAIT_BUDGET: Duration = Duration::from_secs(12);
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
        converge_active_generation_with_fallback(
            &launchd,
            &generation,
            || reconcile_current_generation(&home, &paths.config_dir).map(|_| ()),
            || {
                let loaded = launchd.is_loaded(LINK_PROD_LABEL)?;
                let current = assess_agent(&home, LINK_PROD_LABEL, loaded);
                if !current.present
                    || !current.loaded
                    || current.implementation != LinkImplementation::Rust
                    || !program_points_at_managed_runtime(&current.program_arguments, &home)
                {
                    return Err(
                        "production Link ownership changed before bounded kickstart; refusing mutation"
                            .to_owned(),
                    );
                }
                Ok(())
            },
            |budget| wait_for_active_generation(&status_path, &generation, budget),
        )
    }
}

fn converge_active_generation_with_fallback<L, Reconcile, VerifyOwnership, Wait>(
    launchd: &L,
    generation: &str,
    mut reconcile: Reconcile,
    mut verify_ownership: VerifyOwnership,
    mut wait: Wait,
) -> Result<(), String>
where
    L: LaunchdOps,
    Reconcile: FnMut() -> Result<(), String>,
    VerifyOwnership: FnMut() -> Result<(), String>,
    Wait: FnMut(Duration) -> Result<(), String>,
{
    let wait_slice = ACTIVE_WAIT_BUDGET / ACTIVE_RECONCILE_ATTEMPTS as u32;
    let mut last_error = None;
    for attempt in 0..ACTIVE_RECONCILE_ATTEMPTS {
        if attempt > 0 {
            // An already-running Link may have consumed the first revision while
            // the single-port runtime was restarting. Bump desired state once
            // more without interrupting its Edge WebSocket.
            reconcile()?;
        }
        match wait(wait_slice) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }

    // Hot-switching is the normal zero-disconnect path. If it cannot converge
    // within the bounded window, the loaded Link process may still be running
    // older code or may have missed control revisions during the server restart.
    // Restart exactly the still-owned production Link once. Ownership is
    // re-proved immediately before the mutation so a concurrent replacement
    // cannot turn the bounded recovery into a foreign-job restart. Its program
    // path is runtime/current, so the replacement process executes the new
    // binary; the persisted plist remains the source for the next natural
    // bootstrap.
    verify_ownership()?;
    launchd.kickstart_prod(LINK_PROD_LABEL).map_err(|error| {
        format!(
            "production Link hot-switch to {generation} timed out and bounded kickstart failed: {error}; prior={}",
            last_error.as_deref().unwrap_or("unknown")
        )
    })?;
    reconcile()?;
    wait(POST_KICKSTART_WAIT_BUDGET).map_err(|error| {
        format!(
            "production Link did not activate generation {generation} after bounded kickstart: {error}; prior={}",
            last_error.as_deref().unwrap_or("unknown")
        )
    })
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
    if let Some((device_id, keychain_service)) = configured_edge_device_identity(home) {
        env_out.insert(
            "HERDR_WORKSTATION_ID".to_owned(),
            PlistValue::String(device_id),
        );
        env_out.insert(
            "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
            PlistValue::String(keychain_service),
        );
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
fn wait_for_active_generation(
    status_path: &Path,
    generation: &str,
    budget: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if read_status_active_generation(status_path).as_deref() == Some(generation) {
            return Ok(());
        }
        if started.elapsed() >= budget {
            let observed =
                read_status_active_generation(status_path).unwrap_or_else(|| "missing".to_owned());
            return Err(format!(
                "production Link did not activate generation {generation} within {}ms; observed={observed} status={}",
                budget.as_millis(),
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
    use crate::link::cutover_execute::FakeLaunchd;
    use std::path::Path;

    #[cfg(target_os = "macos")]
    #[test]
    fn prod_plist_refresh_pins_generation_and_runtime_version_together() {
        let root = std::env::temp_dir().join(format!(
            "herdr-link-generation-refresh-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let config_dir = root.join(".config/herdr-mcp");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[edge]\npublic_origin = \"https://edge.example\"\ndevice_id = \"dev_01ARZ3NDEKTSV4RRFFQ69G5FAV\"\n",
        )
        .unwrap();
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
        assert_eq!(
            env.get("HERDR_EDGE_URL").and_then(PlistValue::as_string),
            Some("wss://edge.example/ws")
        );
        assert_eq!(
            env.get("HERDR_WORKSTATION_ID")
                .and_then(PlistValue::as_string),
            Some("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV")
        );
        assert_eq!(
            env.get("HERDR_LINK_KEYCHAIN_SERVICE")
                .and_then(PlistValue::as_string),
            Some("herdr-edge-link-dev_01ARZ3NDEKTSV4RRFFQ69G5FAV")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_generation_hot_switch_does_not_restart_link() {
        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, Path::new("/tmp/link-prod.plist"));
        let mut reconciles = 0;
        let mut waits = 0;
        converge_active_generation_with_fallback(
            &launchd,
            "rust-new",
            || {
                reconciles += 1;
                Ok(())
            },
            || Ok(()),
            |_| {
                waits += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(waits, 1);
        assert_eq!(reconciles, 0);
        assert!(launchd.kickstarts().is_empty());
    }

    #[test]
    fn stale_hot_switch_restarts_owned_link_once_then_converges() {
        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, Path::new("/tmp/link-prod.plist"));
        let mut reconciles = 0;
        let mut waits = 0;
        converge_active_generation_with_fallback(
            &launchd,
            "rust-new",
            || {
                reconciles += 1;
                Ok(())
            },
            || Ok(()),
            |_| {
                waits += 1;
                if waits < 3 {
                    Err(format!("synthetic stale observation {waits}"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert_eq!(waits, 3);
        assert_eq!(reconciles, 2);
        assert_eq!(launchd.kickstarts(), vec![LINK_PROD_LABEL.to_owned()]);
    }

    #[test]
    fn ownership_change_before_restart_fails_without_kickstart() {
        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, Path::new("/tmp/link-prod.plist"));
        let error = converge_active_generation_with_fallback(
            &launchd,
            "rust-new",
            || Ok(()),
            || Err("ownership changed".to_owned()),
            |_| Err("still stale".to_owned()),
        )
        .unwrap_err();
        assert_eq!(error, "ownership changed");
        assert!(launchd.kickstarts().is_empty());
    }

    #[test]
    fn stale_after_single_restart_remains_a_transaction_failure() {
        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, Path::new("/tmp/link-prod.plist"));
        let mut reconciles = 0;
        let error = converge_active_generation_with_fallback(
            &launchd,
            "rust-new",
            || {
                reconciles += 1;
                Ok(())
            },
            || Ok(()),
            |_| Err("still stale".to_owned()),
        )
        .unwrap_err();
        assert!(error.contains("after bounded kickstart"));
        assert_eq!(reconciles, 2);
        assert_eq!(launchd.kickstarts(), vec![LINK_PROD_LABEL.to_owned()]);
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
