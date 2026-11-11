//! Foreground `herdr-mcp link run` entry for a staged Rust Link candidate.
//!
//! Loads credentials with Node `macos-daemon.ts` parity (env override, then
//! the platform device-credential store for the link secret, then the managed
//! local service credential for `HERDR_MCP_TOKEN`). This path never mutates
//! service ownership, `runtime/current`, or production Link ownership.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::config::Config;
use crate::instance::{DEFAULT_RUNTIME_PORT, InstanceId};
use crate::paths::RuntimePaths;

use super::daemon::{
    DaemonConfigError, LinkDaemonConfig, read_link_daemon_config, run_link_daemon,
};
use super::local_mcp::{default_endpoint_for_port, endpoint_targets_port};

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
/// Historical DEV Edge Worker host. The default Rust Link credential is the
/// production item, so that credential must never be loaded for this host.
const LEGACY_DEV_EDGE_HOST: &str = "herdr-edge-dev.whshang.workers.dev";

const SERVER_PLIST_REL: &str = "Library/LaunchAgents/dev.herdr-mcp.server.plist";

/// CLI entry: load config and run the staged daemon in the foreground.
pub fn run() -> Result<ExitCode, String> {
    let config = load_link_run_config().map_err(|error| error.to_string())?;
    // Reachable incompatible Edges always fail closed. The only deferable case
    // is direct transport unavailability when a qualified Relay route is
    // actually available. Qualified means either the compiled mainland-UAT
    // baseline or a validated signed remote pool. Authenticated Edge hello
    // remains the final runtime-contract fence after Relay transport succeeds.
    if let Err(error) = super::edge_contract::probe_edge_contract_for_rust_link(&config.edge_url) {
        let relay_available = validated_relay_route_available(&config)?;
        if should_defer_contract_probe_to_hello(&error, relay_available) {
            eprintln!(
                "[herdr-link] warn direct Edge /health is unreachable; using a qualified Relay route and deferring the final runtime-contract fence to authenticated Edge hello"
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
    if !relay_pool_can_bridge_direct_outage(&pool) {
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

fn relay_pool_can_bridge_direct_outage(pool: &super::relay_manifest::RelayPoolLoad) -> bool {
    !pool.relays.is_empty() && matches!(pool.source, "embedded" | "cached-remote")
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

/// Resolve the target instance from `HERDR_MCP_INSTANCE` in `env_map` (falling
/// back to the default production instance when unset/empty). Invalid instance
/// names fail closed instead of silently falling back to default.
pub(crate) fn resolve_link_instance(
    env_map: &HashMap<String, String>,
) -> Result<InstanceId, DaemonConfigError> {
    match optional_trimmed(env_map, "HERDR_MCP_INSTANCE") {
        Some(raw) => InstanceId::parse(&raw).map_err(DaemonConfigError::Message),
        None => Ok(InstanceId::default_instance()),
    }
}

pub(crate) fn instance_config_dir(
    env_map: &HashMap<String, String>,
    instance: &InstanceId,
) -> Option<PathBuf> {
    if let Some(dir) = optional_trimmed(env_map, "HERDR_MCP_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Some(xdg) = optional_trimmed(env_map, "XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join(instance.config_leaf()));
    }
    optional_trimmed(env_map, "HOME")
        .or_else(|| optional_trimmed(env_map, "USERPROFILE"))
        .map(PathBuf::from)
        .or_else(home_dir)
        .map(|home| home.join(".config").join(instance.config_leaf()))
}

/// Fill Edge/workstation defaults and resolve link/MCP secrets without printing them.
fn enrich_macos_credentials(
    env_map: &mut HashMap<String, String>,
) -> Result<(), DaemonConfigError> {
    let instance = resolve_link_instance(env_map)?;
    let configured = match instance_config_dir(env_map, &instance) {
        Some(dir) => {
            let config_file = dir.join("config.json");
            match Config::load_for_instance(&config_file, &instance) {
                Ok(config) => Some(config),
                Err(error) if instance.is_named() => {
                    return Err(DaemonConfigError::Message(error));
                }
                Err(_) => None,
            }
        }
        None => None,
    };
    enrich_macos_credentials_for_instance(env_map, &instance, configured.as_ref())
}

fn enrich_macos_credentials_with_config(
    env_map: &mut HashMap<String, String>,
    configured: Option<&Config>,
) -> Result<(), DaemonConfigError> {
    let instance = resolve_link_instance(env_map)?;
    enrich_macos_credentials_for_instance(env_map, &instance, configured)
}

fn enrich_macos_credentials_for_instance(
    env_map: &mut HashMap<String, String>,
    instance: &InstanceId,
    configured: Option<&Config>,
) -> Result<(), DaemonConfigError> {
    let runtime_port = configured
        .map(|config| config.runtime_port)
        .unwrap_or_else(|| instance.default_port());
    if let Some(instance_name) = instance.name() {
        if runtime_port == DEFAULT_RUNTIME_PORT {
            return Err(DaemonConfigError::Message(format!(
                "refusing to run Link for named instance '{instance_name}' on production runtime port {DEFAULT_RUNTIME_PORT}"
            )));
        }
        if let Some(existing_endpoint) = optional_trimmed(env_map, "HERDR_MCP_ENDPOINT") {
            if endpoint_targets_port(&existing_endpoint, DEFAULT_RUNTIME_PORT) {
                return Err(DaemonConfigError::Message(format!(
                    "refusing to point named instance '{instance_name}' Link at production runtime port {DEFAULT_RUNTIME_PORT} ({existing_endpoint})"
                )));
            }
        } else {
            env_map.insert(
                "HERDR_MCP_ENDPOINT".to_owned(),
                default_endpoint_for_port(runtime_port),
            );
        }

        let Some(device_id) = configured.and_then(|config| config.edge_device_id.as_deref()) else {
            return Err(DaemonConfigError::Message(format!(
                "refusing to run Link for named instance '{instance_name}': missing edge.device_id; enroll a dedicated device identity for this instance and never fall back to the production link credential ({MACOS_LINK_KEYCHAIN_SERVICE})"
            )));
        };
        if let Some(existing_workstation) = optional_trimmed(env_map, "HERDR_WORKSTATION_ID") {
            if existing_workstation != device_id {
                return Err(DaemonConfigError::Message(format!(
                    "refusing to run Link for named instance '{instance_name}' with HERDR_WORKSTATION_ID='{existing_workstation}' (expected enrolled edge.device_id '{device_id}')"
                )));
            }
        } else {
            env_map.insert("HERDR_WORKSTATION_ID".to_owned(), device_id.to_owned());
        }

        if optional_trimmed(env_map, "HERDR_EDGE_URL").is_none() {
            let Some(edge_url) = configured.and_then(|config| config.edge_ws_url().ok().flatten())
            else {
                return Err(DaemonConfigError::Message(format!(
                    "refusing to run Link for named instance '{instance_name}' without a configured Edge origin; set edge.public_origin or HERDR_EDGE_URL instead of falling back to the production Edge ({MACOS_DEFAULT_EDGE_URL})"
                )));
            };
            env_map.insert("HERDR_EDGE_URL".to_owned(), edge_url);
        }

        let expected_service = format!("herdr-edge-link-{device_id}");
        if let Some(existing_service) = optional_trimmed(env_map, "HERDR_LINK_KEYCHAIN_SERVICE") {
            if existing_service == MACOS_LINK_KEYCHAIN_SERVICE {
                return Err(DaemonConfigError::Message(format!(
                    "refusing to load the legacy production link credential ({MACOS_LINK_KEYCHAIN_SERVICE}) for named instance '{instance_name}'; use the instance's device-specific HERDR_LINK_KEYCHAIN_SERVICE ({expected_service})"
                )));
            }
            if existing_service != expected_service {
                return Err(DaemonConfigError::Message(format!(
                    "refusing to run Link for named instance '{instance_name}' with mismatched HERDR_LINK_KEYCHAIN_SERVICE='{existing_service}' (expected '{expected_service}')"
                )));
            }
        } else {
            env_map.insert("HERDR_LINK_KEYCHAIN_SERVICE".to_owned(), expected_service);
        }
    } else {
        if optional_trimmed(env_map, "HERDR_MCP_ENDPOINT").is_none()
            && runtime_port != DEFAULT_RUNTIME_PORT
        {
            env_map.insert(
                "HERDR_MCP_ENDPOINT".to_owned(),
                default_endpoint_for_port(runtime_port),
            );
        }
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
        if optional_trimmed(env_map, "HERDR_LINK_KEYCHAIN_SERVICE").is_none()
            && let Some(service) = configured.and_then(Config::edge_link_keychain_service)
        {
            env_map.insert("HERDR_LINK_KEYCHAIN_SERVICE".to_owned(), service);
        }
    }

    if optional_trimmed(env_map, "HERDR_DEVICE_NAME").is_none()
        && let Some(name) = crate::device_name::system_device_display_name()
    {
        env_map.insert("HERDR_DEVICE_NAME".to_owned(), name);
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
        let token = load_runtime_token_from_server_plist(env_map, instance)?;
        env_map.insert("HERDR_MCP_TOKEN".to_owned(), token);
    }

    Ok(())
}

/// Resolve the Keychain service name to read for the Link token.
///
/// Fails closed before any device-credential read when:
/// - the run targets the historical DEV Edge with the legacy production service
///   (implicit or explicit `#867` fence); or
/// - neither an enrolled `edge.device_id` nor an explicit
///   `HERDR_LINK_KEYCHAIN_SERVICE` is present (`#870`: never fall back to
///   `MACOS_LINK_KEYCHAIN_SERVICE` when device identity is missing).
fn resolved_link_keychain_service(
    env_map: &HashMap<String, String>,
) -> Result<String, DaemonConfigError> {
    let candidate = optional_trimmed(env_map, "HERDR_LINK_KEYCHAIN_SERVICE");
    let effective = candidate.as_deref().unwrap_or(MACOS_LINK_KEYCHAIN_SERVICE);
    fence_legacy_dev_edge_prod_credential(env_map, effective)?;
    match candidate {
        Some(service) => Ok(service),
        None => Err(DaemonConfigError::Message(format!(
            "refusing to fall back to the legacy production link credential ({MACOS_LINK_KEYCHAIN_SERVICE}) when edge.device_id is missing; configure edge.device_id or set a device-specific HERDR_LINK_KEYCHAIN_SERVICE"
        ))),
    }
}

/// True only for the exact legacy DEV Edge host: parsed as a URL, host-only,
/// lowercased, one trailing dot removed. A hostile superdomain, a lookalike
/// suffix, or a host mentioned in a path/query never matches.
fn edge_url_targets_legacy_dev_edge(edge_url: &str) -> bool {
    url::Url::parse(edge_url.trim())
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host.trim_end_matches('.') == LEGACY_DEV_EDGE_HOST)
}

/// Fail closed before any device-credential read when the run is aimed at the
/// historical DEV Edge but would use the legacy production Link credential.
fn fence_legacy_dev_edge_prod_credential(
    env_map: &HashMap<String, String>,
    service: &str,
) -> Result<(), DaemonConfigError> {
    if service != MACOS_LINK_KEYCHAIN_SERVICE {
        return Ok(());
    }
    let Some(edge_url) = optional_trimmed(env_map, "HERDR_EDGE_URL") else {
        return Ok(());
    };
    if !edge_url_targets_legacy_dev_edge(&edge_url) {
        return Ok(());
    }
    Err(DaemonConfigError::Message(format!(
        "refusing to load the legacy production link credential ({MACOS_LINK_KEYCHAIN_SERVICE}) for the DEV Edge host {LEGACY_DEV_EDGE_HOST}; use a device-specific HERDR_LINK_KEYCHAIN_SERVICE for DEV and never copy the production secret into HERDR_LINK_TOKEN"
    )))
}

fn load_link_token_from_keychain(
    env_map: &HashMap<String, String>,
) -> Result<String, DaemonConfigError> {
    let service = resolved_link_keychain_service(env_map)?;
    #[cfg(target_os = "macos")]
    {
        let username = optional_trimmed(env_map, "USER").unwrap_or_else(current_username);
        crate::credential_store::load(&service, &username).map_err(DaemonConfigError::Message)
    }
    #[cfg(not(target_os = "macos"))]
    {
        #[cfg(target_os = "linux")]
        {
            let username = optional_trimmed(env_map, "USER").unwrap_or_else(current_username);
            crate::credential_store::load(&service, &username).map_err(DaemonConfigError::Message)
        }

        #[cfg(target_os = "windows")]
        {
            let username = optional_trimmed(env_map, "USERNAME").unwrap_or_else(current_username);
            crate::credential_store::load(&service, &username).map_err(DaemonConfigError::Message)
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            Err(DaemonConfigError::Message(
                "HERDR_LINK_TOKEN is required on this platform".to_owned(),
            ))
        }
    }
}

fn load_runtime_token_from_server_plist(
    env_map: &HashMap<String, String>,
    instance: &InstanceId,
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
        let plist = server_plist_path_for_instance(&home, instance);
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
        let _ = instance;
        #[cfg(target_os = "linux")]
        {
            let _ = env_map;
            crate::linux_service_manager::runtime_token_for_link()
                .map_err(DaemonConfigError::Message)
        }

        #[cfg(target_os = "windows")]
        {
            let _ = env_map;
            crate::windows_service_manager::runtime_token_for_link()
                .map_err(DaemonConfigError::Message)
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            let _ = env_map;
            Err(DaemonConfigError::Message(
                "HERDR_MCP_TOKEN is required on this platform".to_owned(),
            ))
        }
    }
}

fn server_plist_path(home: &Path) -> PathBuf {
    server_plist_path_for_instance(home, &InstanceId::default_instance())
}

fn server_plist_path_for_instance(home: &Path, instance: &InstanceId) -> PathBuf {
    if instance.is_default() {
        home.join(SERVER_PLIST_REL)
    } else {
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", instance.service_label()))
    }
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
    #[cfg(target_os = "windows")]
    {
        return env::var("USERNAME")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "unknown".to_owned());
    }
    #[cfg(not(target_os = "windows"))]
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
    fn direct_outage_bridge_accepts_qualified_embedded_or_signed_remote_pool_only() {
        use crate::link::ladder::default_embedded_relays;
        use crate::link::relay_manifest::RelayPoolLoad;

        let embedded = RelayPoolLoad {
            relays: default_embedded_relays(),
            source: "embedded",
            metadata: None,
            freshness: "missing",
            error_class: None,
            revision_floor: None,
        };
        assert!(relay_pool_can_bridge_direct_outage(&embedded));

        let mut remote = embedded.clone();
        remote.source = "cached-remote";
        remote.freshness = "fresh";
        assert!(relay_pool_can_bridge_direct_outage(&remote));

        let mut empty = embedded.clone();
        empty.relays.clear();
        assert!(!relay_pool_can_bridge_direct_outage(&empty));

        let mut unknown = embedded;
        unknown.source = "unknown";
        assert!(!relay_pool_can_bridge_direct_outage(&unknown));
    }

    const DEV_EDGE_WS: &str = "wss://herdr-edge-dev.whshang.workers.dev/ws";

    /// Env for the fence cases. `USER` cannot own a real credential item, so a
    /// regression that drops the fence fails on the store error instead of
    /// reading the production secret.
    fn fence_env(edge_url: &str) -> HashMap<String, String> {
        HashMap::from([
            ("HERDR_EDGE_URL".to_owned(), edge_url.to_owned()),
            (
                "USER".to_owned(),
                "herdr-dev-prod-fence-nonexistent-account".to_owned(),
            ),
        ])
    }

    #[test]
    fn dev_edge_host_with_legacy_prod_credential_fails_before_keychain() {
        // Implicit default (production) service, with no HERDR_LINK_TOKEN, so
        // `enrich` would otherwise read the device store here.
        let mut env_map = fence_env(DEV_EDGE_WS);
        let error = enrich_macos_credentials_with_config(&mut env_map, None)
            .expect_err("legacy dev Edge + production link credential must fail closed");
        let text = error.to_string();
        assert!(text.contains("refusing to load the legacy production link credential"));
        assert!(text.contains(LEGACY_DEV_EDGE_HOST));
        assert!(!text.contains("unable to load workstation link credential"));
        assert_eq!(env_map.get("HERDR_LINK_TOKEN"), None);
        assert_eq!(env_map.get("HERDR_MCP_TOKEN"), None);

        // The same production service set explicitly is the same rejection, and
        // host matching stays exact: a trailing dot is that host, a hostile
        // superdomain is not.
        let explicit = fence_env(DEV_EDGE_WS);
        assert!(
            fence_legacy_dev_edge_prod_credential(&explicit, MACOS_LINK_KEYCHAIN_SERVICE).is_err()
        );
        assert!(edge_url_targets_legacy_dev_edge(
            "wss://Herdr-Edge-Dev.whshang.workers.dev./ws"
        ));
        assert!(!edge_url_targets_legacy_dev_edge(
            "wss://herdr-edge-dev.whshang.workers.dev.evil.example/ws"
        ));
    }

    /// The exact shape of the live `dev.herdr-mcp.link-rust-candidate` job:
    /// a DEV Edge URL with the production Link service pinned explicitly.
    #[test]
    fn dev_edge_host_with_explicit_prod_keychain_service_fails_before_keychain() {
        let mut env_map = fence_env(DEV_EDGE_WS);
        env_map.insert(
            "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
            MACOS_LINK_KEYCHAIN_SERVICE.to_owned(),
        );
        let error = enrich_macos_credentials_with_config(&mut env_map, None)
            .expect_err("explicit production service + DEV Edge must fail closed");
        let text = error.to_string();
        assert!(text.contains("refusing to load the legacy production link credential"));
        assert!(text.contains(LEGACY_DEV_EDGE_HOST));
        assert!(!text.contains("unable to load workstation link credential"));
        assert_eq!(env_map.get("HERDR_LINK_TOKEN"), None);
        assert_eq!(env_map.get("HERDR_MCP_TOKEN"), None);
    }

    #[test]
    fn fence_allows_prod_host_and_dev_device_credentials() {
        let prod = fence_env(MACOS_DEFAULT_EDGE_URL);
        assert!(
            fence_legacy_dev_edge_prod_credential(&prod, MACOS_LINK_KEYCHAIN_SERVICE).is_ok(),
            "healthy production Link must stay allowed"
        );
        let dev_device = "herdr-edge-link-dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        assert!(
            fence_legacy_dev_edge_prod_credential(&fence_env(DEV_EDGE_WS), dev_device).is_ok(),
            "dev device-specific credential must stay allowed"
        );
    }

    #[test]
    fn server_plist_path_is_under_launch_agents() {
        let path = server_plist_path(Path::new("/Users/example"));
        assert_eq!(
            path,
            PathBuf::from("/Users/example/Library/LaunchAgents/dev.herdr-mcp.server.plist")
        );
        let named = InstanceId::parse("dev698").unwrap();
        assert_eq!(
            server_plist_path_for_instance(Path::new("/Users/example"), &named),
            PathBuf::from("/Users/example/Library/LaunchAgents/dev.herdr-mcp.dev698.server.plist")
        );
    }

    #[test]
    fn missing_device_id_without_explicit_service_refuses_prod_fallback() {
        let mut env_map = fence_env("wss://edge.example/ws");
        let config = Config::default();
        let error = enrich_macos_credentials_with_config(&mut env_map, Some(&config))
            .expect_err("missing edge.device_id must not fall back to prod keychain service");
        let text = error.to_string();
        assert!(text.contains("refusing to fall back to the legacy production link credential"));
        assert!(text.contains(MACOS_LINK_KEYCHAIN_SERVICE));
        assert!(!text.contains("unable to load workstation link credential"));
        assert_eq!(env_map.get("HERDR_LINK_TOKEN"), None);
        assert_eq!(env_map.get("HERDR_MCP_TOKEN"), None);
    }

    #[test]
    fn named_instance_without_device_id_fails_before_keychain_and_plist() {
        let named = InstanceId::parse("dev698").unwrap();
        let config = Config {
            runtime_port: named.default_port(),
            edge_public_origin: Some("https://herdr-edge-dev.whshang.workers.dev".to_owned()),
            edge_device_id: None,
            ..Config::default()
        };
        let mut env_map = HashMap::from([
            ("HERDR_MCP_INSTANCE".to_owned(), "dev698".to_owned()),
            (
                "USER".to_owned(),
                "herdr-dev-prod-fence-nonexistent-account".to_owned(),
            ),
        ]);
        let error = enrich_macos_credentials_with_config(&mut env_map, Some(&config))
            .expect_err("named instance without edge.device_id must fail closed");
        let text = error.to_string();
        assert!(text.contains("named instance 'dev698'"));
        assert!(text.contains("missing edge.device_id"));
        assert!(text.contains(MACOS_LINK_KEYCHAIN_SERVICE));
        assert_eq!(env_map.get("HERDR_LINK_TOKEN"), None);
        assert_eq!(env_map.get("HERDR_MCP_TOKEN"), None);
    }

    #[test]
    fn named_instance_refuses_prod_or_mismatched_keychain_service_and_port_8772() {
        let named = InstanceId::parse("dev698").unwrap();
        let device_id = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let config = Config {
            runtime_port: named.default_port(),
            edge_public_origin: Some("https://herdr-edge-dev.whshang.workers.dev".to_owned()),
            edge_device_id: Some(device_id.to_owned()),
            ..Config::default()
        };

        // Explicit production Keychain service on a named instance must fail closed.
        let mut prod_service_env = HashMap::from([
            ("HERDR_MCP_INSTANCE".to_owned(), "dev698".to_owned()),
            (
                "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
                MACOS_LINK_KEYCHAIN_SERVICE.to_owned(),
            ),
            (
                "USER".to_owned(),
                "herdr-dev-prod-fence-nonexistent-account".to_owned(),
            ),
        ]);
        let err = enrich_macos_credentials_with_config(&mut prod_service_env, Some(&config))
            .expect_err("named instance with prod keychain service must fail");
        assert!(err.to_string().contains(MACOS_LINK_KEYCHAIN_SERVICE));
        assert_eq!(prod_service_env.get("HERDR_LINK_TOKEN"), None);

        // Mismatched device keychain service must fail closed.
        let mut wrong_service_env = HashMap::from([
            ("HERDR_MCP_INSTANCE".to_owned(), "dev698".to_owned()),
            (
                "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
                "herdr-edge-link-dev_01ARZ3NDEKTSV4RRFFQ69G5FAW".to_owned(),
            ),
            (
                "USER".to_owned(),
                "herdr-dev-prod-fence-nonexistent-account".to_owned(),
            ),
        ]);
        let err = enrich_macos_credentials_with_config(&mut wrong_service_env, Some(&config))
            .expect_err("named instance with mismatched service must fail");
        assert!(
            err.to_string()
                .contains("mismatched HERDR_LINK_KEYCHAIN_SERVICE")
        );

        // Explicit production port 8772 endpoint on a named instance must fail closed.
        let mut prod_port_env = HashMap::from([
            ("HERDR_MCP_INSTANCE".to_owned(), "dev698".to_owned()),
            (
                "HERDR_MCP_ENDPOINT".to_owned(),
                "http://127.0.0.1:8772/mcp".to_owned(),
            ),
            ("HERDR_LINK_TOKEN".to_owned(), "dev-link-secret".to_owned()),
            (
                "HERDR_MCP_TOKEN".to_owned(),
                "dev-runtime-secret".to_owned(),
            ),
        ]);
        let err = enrich_macos_credentials_with_config(&mut prod_port_env, Some(&config))
            .expect_err("named instance targeting port 8772 must fail");
        assert!(err.to_string().contains("production runtime port 8772"));
    }

    #[test]
    fn named_instance_isolates_endpoint_plist_credentials_and_control_dir_from_prod() {
        let named = InstanceId::parse("dev698").unwrap();
        let expected_port = named.default_port();
        assert_ne!(expected_port, DEFAULT_RUNTIME_PORT);
        let device_id = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let config = Config {
            runtime_port: expected_port,
            edge_public_origin: Some("https://herdr-edge-dev.whshang.workers.dev".to_owned()),
            edge_device_id: Some(device_id.to_owned()),
            ..Config::default()
        };

        let temp_home = std::env::temp_dir().join(format!(
            "herdr-mcp-link-named-isolation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let launch_agents = temp_home.join("Library/LaunchAgents");
        std::fs::create_dir_all(&launch_agents).unwrap();

        #[cfg(target_os = "macos")]
        {
            // Write both a production server plist and a named-instance server plist
            // with distinct tokens so any fallback to production would be caught.
            let write_plist = |label: &str, token: &str| {
                let mut env_dict = plist::Dictionary::new();
                env_dict.insert(
                    "HERDR_MCP_TOKEN".to_owned(),
                    plist::Value::String(token.to_owned()),
                );
                let mut root = plist::Dictionary::new();
                root.insert("Label".to_owned(), plist::Value::String(label.to_owned()));
                root.insert(
                    "EnvironmentVariables".to_owned(),
                    plist::Value::Dictionary(env_dict),
                );
                let path = launch_agents.join(format!("{label}.plist"));
                plist::Value::Dictionary(root).to_file_xml(&path).unwrap();
                path
            };
            write_plist("dev.herdr-mcp.server", "prod-runtime-secret-must-not-leak");
            let named_plist =
                write_plist("dev.herdr-mcp.dev698.server", "dev698-isolated-mcp-token");

            let mut env_map = HashMap::from([
                ("HERDR_MCP_INSTANCE".to_owned(), "dev698".to_owned()),
                ("HOME".to_owned(), temp_home.to_string_lossy().into_owned()),
                (
                    "HERDR_LINK_TOKEN".to_owned(),
                    "dev698-isolated-link-secret".to_owned(),
                ),
            ]);
            enrich_macos_credentials_with_config(&mut env_map, Some(&config))
                .expect("named instance enrich");
            assert_eq!(
                env_map.get("HERDR_MCP_TOKEN").map(String::as_str),
                Some("dev698-isolated-mcp-token"),
                "named instance must read its own server plist token and never the production token"
            );

            // Removing the named instance plist while leaving the production plist in
            // place must fail closed instead of falling back to production.
            std::fs::remove_file(&named_plist).unwrap();
            let mut missing_named_plist_env = HashMap::from([
                ("HERDR_MCP_INSTANCE".to_owned(), "dev698".to_owned()),
                ("HOME".to_owned(), temp_home.to_string_lossy().into_owned()),
                (
                    "HERDR_LINK_TOKEN".to_owned(),
                    "dev698-isolated-link-secret".to_owned(),
                ),
            ]);
            assert!(
                enrich_macos_credentials_with_config(&mut missing_named_plist_env, Some(&config))
                    .is_err(),
                "missing named-instance server plist must not fall back to dev.herdr-mcp.server.plist"
            );
        }

        let mut env_map = HashMap::from([
            ("HERDR_MCP_INSTANCE".to_owned(), "dev698".to_owned()),
            ("HOME".to_owned(), temp_home.to_string_lossy().into_owned()),
            (
                "HERDR_LINK_TOKEN".to_owned(),
                "dev698-isolated-link-secret".to_owned(),
            ),
            (
                "HERDR_MCP_TOKEN".to_owned(),
                "dev698-isolated-mcp-token".to_owned(),
            ),
        ]);
        enrich_macos_credentials_with_config(&mut env_map, Some(&config))
            .expect("named instance enrich");
        assert_eq!(
            env_map.get("HERDR_WORKSTATION_ID").map(String::as_str),
            Some(device_id)
        );
        assert_eq!(
            env_map
                .get("HERDR_LINK_KEYCHAIN_SERVICE")
                .map(String::as_str),
            Some("herdr-edge-link-dev_01ARZ3NDEKTSV4RRFFQ69G5FAV")
        );
        assert_eq!(
            env_map.get("HERDR_MCP_ENDPOINT").map(String::as_str),
            Some(format!("http://127.0.0.1:{expected_port}/mcp").as_str())
        );

        let daemon_cfg = read_link_daemon_config(&env_map).expect("daemon config");
        assert_eq!(
            daemon_cfg.runtime_endpoint,
            format!("http://127.0.0.1:{expected_port}/mcp")
        );
        assert_eq!(
            daemon_cfg.runtime_control_path,
            temp_home.join(".config/herdr-mcp-dev698/runtime-control.json")
        );
        assert_eq!(
            daemon_cfg.runtime_status_path,
            temp_home.join(".config/herdr-mcp-dev698/runtime-status.json")
        );

        // Pointing runtime-control at the production config dir on a named instance must fail.
        let mut bad_control_env = env_map.clone();
        bad_control_env.insert(
            "HERDR_RUNTIME_CONTROL_PATH".to_owned(),
            temp_home
                .join(".config/herdr-mcp/runtime-control.json")
                .to_string_lossy()
                .into_owned(),
        );
        assert!(read_link_daemon_config(&bad_control_env).is_err());

        let _ = std::fs::remove_dir_all(&temp_home);
    }
}
