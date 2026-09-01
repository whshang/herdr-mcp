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
    // Fail closed before opening a WebSocket when Edge still publishes epoch 1.
    let _edge = super::edge_contract::probe_edge_contract_for_rust_link(&config.edge_url)
        .map_err(|error| format!("herdr-mcp link run: {error}"))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("herdr-mcp link run: tokio runtime: {error}"))?;
    let code = runtime
        .block_on(run_link_daemon(config))
        .map_err(|error| format!("herdr-mcp link run: {error}"))?;
    Ok(ExitCode::from(code as u8))
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
    if optional_trimmed(env_map, "HERDR_EDGE_URL").is_none() {
        let edge_url = configured
            .as_ref()
            .and_then(|config| config.edge_ws_url().ok().flatten())
            .unwrap_or_else(|| MACOS_DEFAULT_EDGE_URL.to_owned());
        env_map.insert("HERDR_EDGE_URL".to_owned(), edge_url);
    }
    if optional_trimmed(env_map, "HERDR_WORKSTATION_ID").is_none() {
        let workstation_id = configured
            .as_ref()
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
        && let Some(service) = configured
            .as_ref()
            .and_then(Config::edge_link_keychain_service)
    {
        env_map.insert("HERDR_LINK_KEYCHAIN_SERVICE".to_owned(), service);
    }

    if optional_trimmed(env_map, "HERDR_LINK_TOKEN").is_none() {
        let token = load_link_token_from_keychain(env_map)?;
        env_map.insert("HERDR_LINK_TOKEN".to_owned(), token);
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
        crate::macos_keychain::load_generic_secret(&service, &username)
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
        enrich_macos_credentials(&mut env_map).expect("enrich");
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
    fn server_plist_path_is_under_launch_agents() {
        let path = server_plist_path(Path::new("/Users/example"));
        assert_eq!(
            path,
            PathBuf::from("/Users/example/Library/LaunchAgents/dev.herdr-mcp.server.plist")
        );
    }
}
