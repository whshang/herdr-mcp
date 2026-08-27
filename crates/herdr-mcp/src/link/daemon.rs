//! Staged candidate-only workstation Link daemon assembly.
//!
//! This layer mirrors Node `src/link/daemon.ts`: env config, runtime-generation
//! manager, runtime-control loop, and the staged I/O loop. CLI `link run` loads
//! credentials via `link::run` and calls `run_link_daemon`. This module still
//! does not own launchd, `runtime/current`, or production cutover. Production
//! Link remains on the Node path until later cutover gates are complete.

use std::collections::HashMap;
use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

use super::backoff::{BackoffOptions, ExponentialBackoff};
use super::io_loop::{LinkIoConfig, LinkIoError, LinkIoLoop};
use super::local_mcp::{
    LOCAL_MCP_CONTRACT_EPOCH, LOCAL_MCP_DEFAULT_ENDPOINT, LinkRuntimeTransport,
};
use super::policy::LinkExitKind;
use super::runner::{LinkRunnerCore, RunnerConfig};
use super::runtime_control::{RuntimeControlLoop, RuntimeControlLoopOptions};
use super::runtime_generation::{
    RuntimeGenerationManager, RuntimeGenerationManagerOptions, RuntimeGenerationSpec,
};
use super::socket_driver::LINK_SUBPROTOCOL;
use super::transport::{
    LINK_DEFAULT_HANDSHAKE_TIMEOUT_MS, LINK_DEFAULT_MAX_FRAME_BYTES, LinkTransportCore,
    TransportConfig,
};

/// Public epoch-2 contract identity shared with Node `daemon.ts`.
pub const PUBLIC_CONTRACT_EPOCH: u64 = 2;
pub const PUBLIC_CONTRACT_HASH: &str =
    "sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8";
pub const LEGACY_EPOCH1_CONTRACT_HASH: &str =
    "sha256:3f23083ae31b977dad21b1ec9d6919c49e1067a27f7b7eea7bdd021b54770c0d";

const DAEMON_HEARTBEAT_MS: i64 = 15_000;
const DAEMON_MAX_SILENCE_MS: i64 = 60_000;
const DAEMON_REQUEST_TIMEOUT_MS: u64 = 60_000;
const DAEMON_DRAIN_MS: u64 = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkDaemonConfig {
    pub edge_url: String,
    pub workstation_id: String,
    pub link_token: String,
    pub runtime_token: String,
    pub runtime_endpoint: String,
    pub runtime_generation: String,
    pub contract_epoch: u64,
    pub contract_hash: String,
    pub runtime_control_path: PathBuf,
    pub runtime_status_path: PathBuf,
    pub runtime_control_poll_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonConfigError {
    Missing(String),
    Message(String),
}

impl std::fmt::Display for DaemonConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(name) => write!(f, "herdr-link daemon: {name} is required"),
            Self::Message(message) => write!(f, "herdr-link daemon: {message}"),
        }
    }
}

impl std::error::Error for DaemonConfigError {}

/// Read daemon config from an env map. Keys match Node `readLinkDaemonConfig`.
pub fn read_link_daemon_config(
    env_map: &HashMap<String, String>,
) -> Result<LinkDaemonConfig, DaemonConfigError> {
    let edge_url = required(env_map, "HERDR_EDGE_URL")?;
    let workstation_id = required(env_map, "HERDR_WORKSTATION_ID")?;
    let link_token = required(env_map, "HERDR_LINK_TOKEN")?;
    let runtime_token = required(env_map, "HERDR_MCP_TOKEN")?;
    let runtime_endpoint = optional_trimmed(env_map, "HERDR_MCP_ENDPOINT")
        .unwrap_or_else(|| LOCAL_MCP_DEFAULT_ENDPOINT.to_owned());
    let runtime_generation = optional_trimmed(env_map, "HERDR_RUNTIME_GENERATION")
        .unwrap_or_else(|| "local-mcp-active".to_owned());
    let contract_hash = optional_trimmed(env_map, "HERDR_CONTRACT_HASH")
        .unwrap_or_else(|| PUBLIC_CONTRACT_HASH.to_owned());
    let contract_epoch = match optional_trimmed(env_map, "HERDR_CONTRACT_EPOCH") {
        Some(raw) => raw.parse::<u64>().map_err(|_| {
            DaemonConfigError::Message("HERDR_CONTRACT_EPOCH must be an integer".to_owned())
        })?,
        None if contract_hash == LEGACY_EPOCH1_CONTRACT_HASH => 1,
        None => PUBLIC_CONTRACT_EPOCH,
    };

    let runtime_control_dir = optional_trimmed(env_map, "HERDR_RUNTIME_CONTROL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_runtime_control_dir);
    let runtime_control_path = optional_trimmed(env_map, "HERDR_RUNTIME_CONTROL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime_control_dir.join("runtime-control.json"));
    let runtime_status_path = optional_trimmed(env_map, "HERDR_RUNTIME_STATUS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            runtime_control_path
                .parent()
                .map(|parent| parent.join("runtime-status.json"))
                .unwrap_or_else(|| runtime_control_dir.join("runtime-status.json"))
        });
    let poll_raw = optional_trimmed(env_map, "HERDR_RUNTIME_CONTROL_POLL_MS")
        .and_then(|raw| raw.parse::<u64>().ok());
    let runtime_control_poll_ms = match poll_raw {
        Some(value) if (100..=60_000).contains(&value) => value,
        _ => 1_000,
    };

    let edge = url::Url::parse(&edge_url).map_err(|_| {
        DaemonConfigError::Message("HERDR_EDGE_URL must use wss:// or ws://".to_owned())
    })?;
    if edge.scheme() != "wss" && edge.scheme() != "ws" {
        return Err(DaemonConfigError::Message(
            "HERDR_EDGE_URL must use wss:// or ws://".to_owned(),
        ));
    }
    if !valid_id(&workstation_id) {
        return Err(DaemonConfigError::Message(
            "HERDR_WORKSTATION_ID is invalid".to_owned(),
        ));
    }
    if !valid_id(&runtime_generation) {
        return Err(DaemonConfigError::Message(
            "HERDR_RUNTIME_GENERATION is invalid".to_owned(),
        ));
    }
    let valid_contract = (contract_epoch == PUBLIC_CONTRACT_EPOCH
        && contract_hash == PUBLIC_CONTRACT_HASH)
        || (contract_epoch == 1 && contract_hash == LEGACY_EPOCH1_CONTRACT_HASH);
    if !valid_contract {
        return Err(DaemonConfigError::Message(
            "contract epoch/hash pair is not a supported public or rollback contract".to_owned(),
        ));
    }

    Ok(LinkDaemonConfig {
        edge_url,
        workstation_id,
        link_token,
        runtime_token,
        runtime_endpoint,
        runtime_generation,
        contract_epoch,
        contract_hash,
        runtime_control_path,
        runtime_status_path,
        runtime_control_poll_ms,
    })
}

/// Process exit status mirroring Node daemon deliberate-stop handling.
pub fn exit_status_for_kind(kind: LinkExitKind) -> i32 {
    match kind {
        LinkExitKind::Stopped
        | LinkExitKind::Superseded
        | LinkExitKind::AuthRejected
        | LinkExitKind::ContractRejected => 0,
        LinkExitKind::MaxReconnect | LinkExitKind::FatalError => 1,
    }
}

/// Assemble and run the staged candidate-only Link daemon.
///
/// Requires epoch 2 because the staged Rust `RuntimeGenerationManager` only
/// accepts `LOCAL_MCP_CONTRACT_EPOCH`. Epoch-1 config remains readable for
/// Node parity, but this Rust run path fails closed instead of cutting over.
pub async fn run_link_daemon(config: LinkDaemonConfig) -> Result<i32, String> {
    if config.contract_epoch != LOCAL_MCP_CONTRACT_EPOCH {
        return Err(format!(
            "herdr-link daemon: staged Rust assembly requires contract epoch {LOCAL_MCP_CONTRACT_EPOCH}"
        ));
    }

    let base = RuntimeGenerationSpec {
        generation: config.runtime_generation.clone(),
        endpoint: config.runtime_endpoint.clone(),
        expected_runtime_version: None,
        runtime_commit: None,
    };
    let mut manager_options = RuntimeGenerationManagerOptions::new(
        base.clone(),
        config.runtime_token.clone(),
        config.contract_hash.clone(),
    );
    manager_options.contract_epoch = config.contract_epoch;
    manager_options.default_timeout_ms = 30_000;
    manager_options.max_timeout_ms = 60_000;
    manager_options.observation_checks = 3;
    manager_options.observation_interval_ms = 500;

    let manager = Arc::new(
        RuntimeGenerationManager::new(manager_options)
            .map_err(|error| format!("herdr-link daemon: manager config: {error:?}"))?,
    );
    let runtime_control = RuntimeControlLoop::new(RuntimeControlLoopOptions {
        manager: Arc::clone(&manager),
        base: base.clone(),
        control_path: config.runtime_control_path.clone(),
        status_path: config.runtime_status_path.clone(),
        poll_interval_ms: Some(config.runtime_control_poll_ms),
        now_ms: None,
    });
    runtime_control
        .initialize()
        .await
        .map_err(|error| format!("herdr-link daemon: runtime-control initialize: {error}"))?;
    runtime_control.start();

    let initial_health = manager.get_health().await;
    if !initial_health.healthy {
        let _ = writeln!(
            io::stderr(),
            "[herdr-link-daemon] warn local runtime health probe failed at startup"
        );
    }

    let boot_id = new_boot_id();
    let started_at_ms = system_now_ms();
    let mut runner_config =
        RunnerConfig::new(config.workstation_id.clone(), boot_id, started_at_ms);
    runner_config.request_timeout_ms = DAEMON_REQUEST_TIMEOUT_MS;
    let runner = LinkRunnerCore::new(runner_config, Arc::clone(&manager), base.generation);
    let transport = LinkTransportCore::new(
        config.workstation_id.clone(),
        None,
        ExponentialBackoff::new(BackoffOptions::default()),
        TransportConfig {
            heartbeat_ms: DAEMON_HEARTBEAT_MS,
            handshake_timeout_ms: LINK_DEFAULT_HANDSHAKE_TIMEOUT_MS,
            max_frame_bytes: LINK_DEFAULT_MAX_FRAME_BYTES,
            max_silence_ms: DAEMON_MAX_SILENCE_MS,
        },
    );
    let io_config = LinkIoConfig {
        edge_url: config.edge_url.clone(),
        application_protocol: LINK_SUBPROTOCOL.to_owned(),
        link_token: config.link_token.clone(),
        socket: Default::default(),
        drain_ms: DAEMON_DRAIN_MS,
        now_ms: Arc::new(system_now_ms),
        rng_sample: Arc::new(system_rng_sample),
    };
    let io = LinkIoLoop::production(io_config, transport, runner);

    let (stop_tx, stop_rx) = watch::channel(false);
    let signal_task = tokio::spawn(async move {
        wait_for_stop_signal().await;
        let _ = stop_tx.send(true);
    });

    let exit = match io.run(stop_rx).await {
        Ok(kind) => kind,
        Err(LinkIoError::Runner(error)) => {
            runtime_control.close();
            signal_task.abort();
            return Err(format!("herdr-link daemon: runner error: {error:?}"));
        }
        Err(error) => {
            runtime_control.close();
            signal_task.abort();
            return Err(format!("herdr-link daemon: io error: {error:?}"));
        }
    };

    let _ = writeln!(
        io::stderr(),
        "[herdr-link-daemon] info exit kind={}",
        exit.as_str()
    );
    runtime_control.close();
    signal_task.abort();
    Ok(exit_status_for_kind(exit))
}

fn required(env_map: &HashMap<String, String>, name: &str) -> Result<String, DaemonConfigError> {
    match env_map.get(name) {
        Some(value) if !value.trim().is_empty() => Ok(value.trim().to_owned()),
        _ => Err(DaemonConfigError::Missing(name.to_owned())),
    }
}

fn optional_trimmed(env_map: &HashMap<String, String>, name: &str) -> Option<String> {
    env_map
        .get(name)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn default_runtime_control_dir() -> PathBuf {
    if let Ok(path) = env::var("HERDR_MCP_CONFIG_DIR") {
        return PathBuf::from(path);
    }
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("herdr-mcp");
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("herdr-mcp")
}

fn new_boot_id() -> String {
    format!("boot-{}-{}", std::process::id(), system_now_ms())
}

fn system_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn system_rng_sample() -> f64 {
    // Non-crypto sample for reconnect jitter only. Node uses Math.random().
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    (f64::from(nanos % 10_000) / 10_000.0).clamp(0.0, 1.0)
}

async fn wait_for_stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(overrides: &[(&str, &str)]) -> HashMap<String, String> {
        let mut map = HashMap::from([
            (
                "HERDR_EDGE_URL".to_owned(),
                "wss://herdr-edge-dev.example/ws".to_owned(),
            ),
            ("HERDR_WORKSTATION_ID".to_owned(), "dev-w1".to_owned()),
            ("HERDR_LINK_TOKEN".to_owned(), "link-secret".to_owned()),
            ("HERDR_MCP_TOKEN".to_owned(), "runtime-secret".to_owned()),
        ]);
        for (key, value) in overrides {
            map.insert((*key).to_owned(), (*value).to_owned());
        }
        map
    }

    #[test]
    fn daemon_config_uses_public_epoch2_identity_and_loopback_mcp_default() {
        let cfg = read_link_daemon_config(&env(&[])).expect("config");
        assert_eq!(cfg.contract_epoch, PUBLIC_CONTRACT_EPOCH);
        assert_eq!(cfg.contract_hash, PUBLIC_CONTRACT_HASH);
        assert_eq!(cfg.runtime_endpoint, LOCAL_MCP_DEFAULT_ENDPOINT);
        assert_eq!(cfg.edge_url, "wss://herdr-edge-dev.example/ws");
        assert_eq!(cfg.workstation_id, "dev-w1");
    }

    #[test]
    fn daemon_config_fails_closed_on_missing_credentials() {
        for key in ["HERDR_LINK_TOKEN", "HERDR_MCP_TOKEN"] {
            let mut input = env(&[]);
            input.remove(key);
            let error = read_link_daemon_config(&input).expect_err("missing");
            assert!(error.to_string().contains(&format!("{key} is required")));
        }
    }

    #[test]
    fn daemon_config_rejects_non_websocket_edge_and_invalid_contract_pairs() {
        let https = read_link_daemon_config(&env(&[("HERDR_EDGE_URL", "https://example.com/ws")]))
            .expect_err("https");
        assert!(https.to_string().contains("wss:// or ws://"));

        let bad_hash = read_link_daemon_config(&env(&[(
            "HERDR_CONTRACT_HASH",
            &format!("sha256:{}", "0".repeat(64)),
        )]))
        .expect_err("bad hash");
        assert!(
            bad_hash
                .to_string()
                .contains("not a supported public or rollback contract")
        );

        let mismatched = read_link_daemon_config(&env(&[
            ("HERDR_CONTRACT_EPOCH", "1"),
            ("HERDR_CONTRACT_HASH", PUBLIC_CONTRACT_HASH),
        ]))
        .expect_err("mismatched");
        assert!(
            mismatched
                .to_string()
                .contains("not a supported public or rollback contract")
        );
    }

    #[test]
    fn daemon_config_accepts_frozen_epoch1_pair_for_supervised_rollback() {
        let cfg = read_link_daemon_config(&env(&[
            ("HERDR_CONTRACT_EPOCH", "1"),
            ("HERDR_CONTRACT_HASH", LEGACY_EPOCH1_CONTRACT_HASH),
        ]))
        .expect("epoch1");
        assert_eq!(cfg.contract_epoch, 1);
        assert_eq!(cfg.contract_hash, LEGACY_EPOCH1_CONTRACT_HASH);
    }

    #[test]
    fn daemon_config_validates_workstation_id() {
        let error = read_link_daemon_config(&env(&[("HERDR_WORKSTATION_ID", "bad/id")]))
            .expect_err("bad id");
        assert!(error.to_string().contains("WORKSTATION_ID is invalid"));
    }

    #[test]
    fn exit_status_maps_deliberate_stops_to_zero() {
        assert_eq!(exit_status_for_kind(LinkExitKind::Stopped), 0);
        assert_eq!(exit_status_for_kind(LinkExitKind::Superseded), 0);
        assert_eq!(exit_status_for_kind(LinkExitKind::AuthRejected), 0);
        assert_eq!(exit_status_for_kind(LinkExitKind::ContractRejected), 0);
        assert_eq!(exit_status_for_kind(LinkExitKind::MaxReconnect), 1);
        assert_eq!(exit_status_for_kind(LinkExitKind::FatalError), 1);
    }

    #[test]
    fn staged_run_rejects_epoch1_without_cutting_production_link() {
        let cfg = read_link_daemon_config(&env(&[
            ("HERDR_CONTRACT_EPOCH", "1"),
            ("HERDR_CONTRACT_HASH", LEGACY_EPOCH1_CONTRACT_HASH),
        ]))
        .expect("epoch1 config");
        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_link_daemon(cfg))
            .expect_err("epoch1 run");
        assert!(error.contains("requires contract epoch 2"));
    }

    #[tokio::test]
    async fn candidate_assembly_wires_manager_control_and_runner_without_cli() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-link-daemon-assemble-{}-{}",
            std::process::id(),
            system_now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let control_path = dir.join("runtime-control.json");
        let status_path = dir.join("runtime-status.json");
        let cfg = LinkDaemonConfig {
            edge_url: "wss://herdr-edge-dev.example/ws".to_owned(),
            workstation_id: "dev-w1".to_owned(),
            link_token: "link-secret".to_owned(),
            runtime_token: "runtime-secret".to_owned(),
            runtime_endpoint: "http://127.0.0.1:8772/mcp".to_owned(),
            runtime_generation: "local-mcp-active".to_owned(),
            contract_epoch: PUBLIC_CONTRACT_EPOCH,
            contract_hash: PUBLIC_CONTRACT_HASH.to_owned(),
            runtime_control_path: control_path.clone(),
            runtime_status_path: status_path.clone(),
            runtime_control_poll_ms: 1_000,
        };

        let base = RuntimeGenerationSpec {
            generation: cfg.runtime_generation.clone(),
            endpoint: cfg.runtime_endpoint.clone(),
            expected_runtime_version: None,
            runtime_commit: None,
        };
        let manager = Arc::new(
            RuntimeGenerationManager::new(RuntimeGenerationManagerOptions::new(
                base.clone(),
                cfg.runtime_token.clone(),
                cfg.contract_hash.clone(),
            ))
            .expect("manager"),
        );
        assert_eq!(manager.name(), "runtime-generation-manager");
        assert_eq!(manager.active_generation_id(), "local-mcp-active");

        let control = RuntimeControlLoop::new(RuntimeControlLoopOptions {
            manager: Arc::clone(&manager),
            base: base.clone(),
            control_path: control_path.clone(),
            status_path: status_path.clone(),
            poll_interval_ms: Some(cfg.runtime_control_poll_ms),
            now_ms: None,
        });
        control.initialize().await.expect("initialize");
        assert!(control_path.is_file());
        assert!(status_path.is_file());

        let runner = LinkRunnerCore::new(
            RunnerConfig::new("dev-w1", "boot-test", 1),
            Arc::clone(&manager),
            base.generation,
        );
        let transport = LinkTransportCore::new(
            "dev-w1",
            Some(0),
            ExponentialBackoff::new(BackoffOptions {
                base_ms: Some(5.0),
                max_ms: Some(5.0),
                factor: Some(1.0),
                jitter: Some(0.0),
            }),
            TransportConfig {
                heartbeat_ms: DAEMON_HEARTBEAT_MS,
                handshake_timeout_ms: LINK_DEFAULT_HANDSHAKE_TIMEOUT_MS,
                max_frame_bytes: LINK_DEFAULT_MAX_FRAME_BYTES,
                max_silence_ms: DAEMON_MAX_SILENCE_MS,
            },
        );
        let io = LinkIoLoop::production(
            LinkIoConfig {
                edge_url: cfg.edge_url,
                application_protocol: LINK_SUBPROTOCOL.to_owned(),
                link_token: cfg.link_token,
                socket: Default::default(),
                drain_ms: DAEMON_DRAIN_MS,
                now_ms: Arc::new(|| 0),
                rng_sample: Arc::new(|| 0.0),
            },
            transport,
            runner,
        );
        // Construction succeeds; do not open a live edge socket in this unit.
        drop(io);
        control.close();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
