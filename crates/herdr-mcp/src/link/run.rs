//! Foreground `herdr-mcp link run` entry for a staged Rust Link candidate.
//!
//! Loads credentials with Node `macos-daemon.ts` parity (env override, then
//! macOS Keychain for the link secret, then the MCP server LaunchAgent plist
//! for `HERDR_MCP_TOKEN`). This path never mutates launchd, plists,
//! `runtime/current`, or production Link ownership.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::config::Config;
use crate::paths::RuntimePaths;

use super::daemon::{
    DaemonConfigError, LinkDaemonConfig, read_link_daemon_config, run_link_daemon,
};

/// Default Keychain service for Rust Link soak/candidate (epoch-2 Edge).
///
/// Node canary still uses `herdr-edge-dev-link-secret` against edge-dev. Rust
/// `link run` requires public contract epoch 2, so defaults follow edge-prod.
pub const MACOS_LINK_KEYCHAIN_SERVICE: &str = "herdr-edge-prod-link-secret";
/// Legacy fallback Edge WSS URL for Rust Link when no instance-specific
/// canonical public origin has been configured yet. New installations should
/// persist `[edge].public_origin` during setup instead of relying on this.
pub const MACOS_DEFAULT_EDGE_URL: &str = "wss://herdr-edge-prod.whshang.workers.dev/ws";
/// Default workstation id for foreground `link run` without env overrides.
pub const MACOS_DEFAULT_WORKSTATION_ID: &str = "dev-rust-link-candidate";

const SERVER_PLIST_REL: &str = "Library/LaunchAgents/dev.herdr-mcp.server.plist";

/// CLI entry: load config and run the staged daemon in the foreground.
pub fn run() -> Result<ExitCode, String> {
    let config = load_link_run_config().map_err(|error| error.to_string())?;
    // Reachable incompatible Edges always fail closed. The only deferable case
    // is direct transport unavailability when a validated signed Relay route is
    // actually available; authenticated Edge hello remains the final runtime-
    // contract fence after the Relay establishes transport.
    if let Err(error) = super::edge_contract::probe_edge_contract_for_rust_link(&config.edge_url) {
        let relay_available = validated_relay_route_available(&config)?;
        if should_defer_contract_probe_to_hello(&error, relay_available) {
            eprintln!(
                "[herdr-link] warn direct Edge /health is unreachable; using a validated signed Relay route and deferring the final runtime-contract fence to authenticated Edge hello"
            );
        } else {
            return Err(format!("herdr-mcp link run: {error}"));
        }
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("herdr-mcp link run: tokio runtime: {error}"))?;
    let code = runtime
        .block_on(run_link_daemon(config))
        .map_err(|error| format!("herdr-mcp link run: {error}"))?;
    Ok(ExitCode::from(code as u8))
}

fn should_defer_contract_probe_to_hello(
    error: &super::edge_contract::EdgeContractError,
    relay_available: bool,
) -> bool {
    relay_available && error.is_transport_unavailable()
}

fn validated_relay_route_available(config: &LinkDaemonConfig) -> Result<bool, String> {
    let paths = RuntimePaths::discover()
        .map_err(|error| format!("herdr-mcp link run: runtime paths: {error}"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0);
    let pool = super::relay_manifest::load_cached_pool(&paths, now);
    if pool.source != "cached-remote" || pool.relays.is_empty() {
        return Ok(false);
    }
    let routes = super::ladder::build_ladder_routes(
        &config.edge_url,
        config.public_origin.as_deref(),
        config.link_upstream_origin.as_deref(),
        &config.workstation_id,
        None,
        &pool.relays,
    )
    .map_err(|error| format!("herdr-mcp link run: transport ladder error: {error}"))?;
    Ok(routes
        .iter()
        .any(|route| route.kind == super::ladder::TransportRouteKind::SharedRelay))
}

/// Build daemon config from process environment (+ macOS credential fallbacks).
pub fn load_link_run_config() -> Result<LinkDaemonConfig, DaemonConfigError> {
    let mut env_map = env_map_from_process();
    enrich_macos_credentials(&mut env_map)?;
    read_link_daemon_config(&env_map)
}

fn env_map_from_process() -> HashMap<String, String> {
    env::vars().collect()
}

/// Fill Edge/workstation defaults and resolve link/MCP secrets without printing them.
fn enrich_macos_credentials(
    env_map: &mut HashMap<String, String>,
) -> Result<(), DaemonConfigError> {
    let configured = RuntimePaths::discover()
        .ok()
        .and_then(|paths| Config::load_for_instance(&paths.config_file, &paths.instance).ok());
    enrich_macos_credentials_with_config(env_map, configured.as_ref())
}

fn enrich_macos_credentials_with_config(
    env_map: &mut HashMap<String, String>,
    configured: Option<&Config>,
) -> Result<(), DaemonConfigError> {
    if optional_trimmed(env_map, "HERDR_EDGE_URL").is_none() {
        let edge_url = configured
            .and_then(|config| config.edge_ws_url().ok().flatten())
            .unwrap_or_else(|| MACOS_DEFAULT_EDGE_URL.to_owned());
        env_map.insert("HERDR_EDGE_URL".to_owned(), edge_url);
    }
    if optional_trimmed(env_map, "HERDR_WORKSTATION_ID").is_none() {
        let workstation_id = configured
            .and_then(|config| config.edge_device_id.clone())
            .unwrap_or_else(|| MACOS_DEFAULT_WORKSTATION_ID.to_owned());
        env_map.insert("HERDR_WORKSTATION_ID".to_owned(), workstation_id);
    }
    if optional_trimmed(env_map, "HERDR_DEVICE_NAME").is_none()
        && let Some(name) = crate::device_name::system_device_display_name()
    {
        env_map.insert("HERDR_DEVICE_NAME".to_owned(), name);
    }
    if optional_trimmed(env_map, "HERDR_LINK_KEYCHAIN_SERVICE").is_none()
        && let Some(service) = configured.and_then(Config::edge_link_keychain_service)
    {
        env_map.insert("HERDR_LINK_KEYCHAIN_SERVICE".to_owned(), service);
    }

    if optional_trimmed(env_map, "HERDR_LINK_TOKEN").is_none() {
        let token = load_link_token_from_keychain(env_map)?;
        env_map.insert("HERDR_LINK_TOKEN".to_owned(), token);
    }

    if optional_trimmed(env_map, "HERDR_PUBLIC_ORIGIN").is_none()
        && let Some(origin) = configured.and_then(|config| config.edge_public_origin.clone())
    {
        env_map.insert("HERDR_PUBLIC_ORIGIN".to_owned(), origin);
    }
    if optional_trimmed(env_map, "HERDR_LINK_UPSTREAM_ORIGIN").is_none()
        && let Some(upstream) =
            configured.and_then(|config| config.edge_link_upstream_origin.clone())
    {
        env_map.insert("HERDR_LINK_UPSTREAM_ORIGIN".to_owned(), upstream);
    }

    if optional_trimmed(env_map, "HERDR_MCP_TOKEN").is_none() {
        let token = load_runtime_token_from_server_plist(env_map)?;
        env_map.insert("HERDR_MCP_TOKEN".to_owned(), token);
    }

    Ok(())
}

fn load_link_token_from_keychain(
    env_map: &HashMap<String, String>,
) -> Result<String, DaemonConfigError> {
    #[cfg(target_os = "macos")]
    {
        let username = optional_trimmed(env_map, "USER").unwrap_or_else(current_username);
        let service = optional_trimmed(env_map, "HERDR_LINK_KEYCHAIN_SERVICE")
            .unwrap_or_else(|| MACOS_LINK_KEYCHAIN_SERVICE.to_owned());
        crate::macos_credential_helper::load(&service, &username)
            .map_err(DaemonConfigError::Message)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = env_map;
        Err(DaemonConfigError::Message(
            "HERDR_LINK_TOKEN is required (Keychain load is macOS-only)".to_owned(),
        ))
    }
}

fn load_runtime_token_from_server_plist(
    env_map: &HashMap<String, String>,
) -> Result<String, DaemonConfigError> {
    #[cfg(target_os = "macos")]
    {
        let home = optional_trimmed(env_map, "HOME")
            .map(PathBuf::from)
            .or_else(home_dir)
            .ok_or_else(|| {
                DaemonConfigError::Message(
                    "HOME is required to load local MCP credential".to_owned(),
                )
            })?;
        let plist = server_plist_path(&home);
        command_text(
            "/usr/libexec/PlistBuddy",
            &[
                "-c",
                "Print :EnvironmentVariables:HERDR_MCP_TOKEN",
                plist.to_str().ok_or_else(|| {
                    DaemonConfigError::Message("server plist path is not valid UTF-8".to_owned())
                })?,
            ],
            "local MCP credential",
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = env_map;
        Err(DaemonConfigError::Message(
            "HERDR_MCP_TOKEN is required (LaunchAgent plist load is macOS-only)".to_owned(),
        ))
    }
}

fn server_plist_path(home: &Path) -> PathBuf {
    home.join(SERVER_PLIST_REL)
}

fn command_text(file: &str, args: &[&str], label: &str) -> Result<String, DaemonConfigError> {
    let output = Command::new(file).args(args).output().map_err(|_| {
        DaemonConfigError::Message(format!("herdr-link macOS: unable to load {label}"))
    })?;
    if !output.status.success() {
        return Err(DaemonConfigError::Message(format!(
            "herdr-link macOS: unable to load {label}"
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.is_empty() {
        return Err(DaemonConfigError::Message(format!(
            "herdr-link macOS: unable to load {label}"
        )));
    }
    Ok(text)
}

fn optional_trimmed(env_map: &HashMap<String, String>, name: &str) -> Option<String> {
    env_map
        .get(name)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn current_username() -> String {
    Command::new("id")
        .arg("-un")
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if name.is_empty() { None } else { Some(name) }
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// True when this binary exposes `herdr-mcp link run` (G5 gate `rust_cli_link_run`).
pub const LINK_RUN_WIRED: bool = true;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::daemon::{PUBLIC_CONTRACT_EPOCH, PUBLIC_CONTRACT_HASH};

    fn base_env() -> HashMap<String, String> {
        HashMap::from([
            (
                "HERDR_EDGE_URL".to_owned(),
                "wss://herdr-edge-dev.example/ws".to_owned(),
            ),
            ("HERDR_WORKSTATION_ID".to_owned(), "dev-w1".to_owned()),
            ("HERDR_LINK_TOKEN".to_owned(), "link-secret".to_owned()),
            ("HERDR_MCP_TOKEN".to_owned(), "runtime-secret".to_owned()),
        ])
    }

    #[test]
    fn env_credentials_build_daemon_config_without_keychain() {
        let cfg = read_link_daemon_config(&base_env()).expect("config");
        assert_eq!(cfg.edge_url, "wss://herdr-edge-dev.example/ws");
        assert_eq!(cfg.workstation_id, "dev-w1");
        assert_eq!(cfg.link_token, "link-secret");
        assert_eq!(cfg.runtime_token, "runtime-secret");
        assert_eq!(cfg.contract_epoch, PUBLIC_CONTRACT_EPOCH);
        assert_eq!(cfg.contract_hash, PUBLIC_CONTRACT_HASH);
    }

    #[test]
    fn enrich_applies_macos_defaults_when_env_tokens_present() {
        let mut env_map = HashMap::from([
            ("HERDR_LINK_TOKEN".to_owned(), "link-secret".to_owned()),
            ("HERDR_MCP_TOKEN".to_owned(), "runtime-secret".to_owned()),
        ]);
        enrich_macos_credentials_with_config(&mut env_map, None).expect("enrich");
        assert_eq!(
            env_map.get("HERDR_EDGE_URL").map(String::as_str),
            Some(MACOS_DEFAULT_EDGE_URL)
        );
        assert_eq!(
            env_map.get("HERDR_WORKSTATION_ID").map(String::as_str),
            Some(MACOS_DEFAULT_WORKSTATION_ID)
        );
        let cfg = read_link_daemon_config(&env_map).expect("config");
        assert_eq!(cfg.edge_url, MACOS_DEFAULT_EDGE_URL);
        assert_eq!(cfg.workstation_id, MACOS_DEFAULT_WORKSTATION_ID);
    }

    #[test]
    fn enrich_uses_link_upstream_origin_when_configured() {
        let mut env_map = HashMap::from([
            ("HERDR_LINK_TOKEN".to_owned(), "link-secret".to_owned()),
            ("HERDR_MCP_TOKEN".to_owned(), "runtime-secret".to_owned()),
        ]);
        let config = Config {
            edge_public_origin: Some("https://custom.example.com".to_owned()),
            edge_link_upstream_origin: Some("https://backend.workers.dev".to_owned()),
            edge_device_id: Some("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()),
            ..Config::default()
        };
        enrich_macos_credentials_with_config(&mut env_map, Some(&config)).expect("enrich");
        assert_eq!(
            env_map.get("HERDR_EDGE_URL").map(String::as_str),
            Some("wss://backend.workers.dev/ws")
        );
        let cfg = read_link_daemon_config(&env_map).expect("config");
        assert_eq!(cfg.edge_url, "wss://backend.workers.dev/ws");
    }

    #[test]
    fn credential_errors_never_embed_secret_values() {
        let error = DaemonConfigError::Message(
            "herdr-link macOS: unable to load workstation link credential".to_owned(),
        );
        let text = error.to_string();
        assert!(text.contains("unable to load workstation link credential"));
        assert!(!text.contains("link-secret"));
        assert!(!text.contains("runtime-secret"));
    }

    #[test]
    fn contract_probe_is_deferred_only_for_transport_unavailability_with_relay() {
        let transport = super::super::edge_contract::EdgeContractError::TransportUnavailable(
            "connection reset".to_owned(),
        );
        let mismatch =
            super::super::edge_contract::EdgeContractError::Message("epoch mismatch".to_owned());
        assert!(should_defer_contract_probe_to_hello(&transport, true));
        assert!(!should_defer_contract_probe_to_hello(&transport, false));
        assert!(!should_defer_contract_probe_to_hello(&mismatch, true));
    }

    #[test]
    fn server_plist_path_is_under_launch_agents() {
        let path = server_plist_path(Path::new("/Users/example"));
        assert_eq!(
            path,
            PathBuf::from("/Users/example/Library/LaunchAgents/dev.herdr-mcp.server.plist")
        );
    }
}
