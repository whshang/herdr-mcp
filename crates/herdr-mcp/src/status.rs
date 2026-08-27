use crate::config::Config;
use crate::herdr::HerdrClient;
use crate::native_host_install;
use crate::native_tools;
use crate::paths::RuntimePaths;
use crate::service_manager;
use crate::snapshot;
use crate::state_cache::EventCache;
use crate::updater_store::UpdateStore;
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum RuntimeHealth {
    Healthy(u16),
    UnexpectedHttp(u16),
    Unreachable,
}

#[derive(Debug)]
struct StatusReport {
    runtime: RuntimeHealth,
    herdr_transport_reachable: bool,
}

#[derive(Debug)]
struct EventCacheProbe {
    healthy: bool,
    cursor: u64,
    digest_events: usize,
    agents: usize,
    workspaces: usize,
    snapshot_panes: usize,
    stream_events: u64,
    last_event_at: Option<String>,
    needs_reconcile: bool,
    error: Option<String>,
}

fn collect(paths: &RuntimePaths, config: &Config) -> StatusReport {
    StatusReport {
        runtime: probe_runtime(config.runtime_port),
        herdr_transport_reachable: probe_herdr_transport(paths),
    }
}

pub fn print_status(paths: &RuntimePaths, config: &Config) {
    let report = collect(paths, config);
    println!("Herdr MCP {}", env!("CARGO_PKG_VERSION"));
    println!("config: {}", paths.config_file.display());
    println!(
        "runtime: {}",
        runtime_label(report.runtime, config.runtime_port)
    );
    println!(
        "herdr transport: {}",
        if report.herdr_transport_reachable {
            "reachable"
        } else {
            "unreachable"
        }
    );
    println!("update channel: {}", config.update_channel.as_str());
    println!(
        "update checks: {}",
        if config.update_check {
            "enabled"
        } else {
            "disabled"
        }
    );
}

pub fn print_doctor(paths: &RuntimePaths, config: &Config) -> bool {
    let report = collect(paths, config);
    let runtime_healthy = matches!(report.runtime, RuntimeHealth::Healthy(_));
    let methods_result = native_tools::methods("");
    let schema_healthy = methods_result["ok"].as_bool() == Some(true);
    let native_call_result = paths
        .herdr_socket
        .as_ref()
        .map(|socket| native_tools::call(&HerdrClient::new(socket), "ping", json!({})))
        .unwrap_or_else(|| json!({"ok": false}));
    let native_call_healthy = native_call_result["ok"].as_bool() == Some(true);
    let snapshot_result = match paths.herdr_socket.as_ref() {
        Some(socket) => snapshot::fetch(&HerdrClient::new(socket)),
        None => Err("Herdr local transport is unavailable".to_owned()),
    };
    let snapshot_healthy = snapshot_result.is_ok();
    let inspect_result = paths
        .herdr_socket
        .as_ref()
        .map(|socket| native_tools::inspect(&HerdrClient::new(socket), None, None))
        .unwrap_or_else(|| json!({"ok": false}));
    let inspect_healthy = inspect_result["ok"].as_bool() == Some(true);
    let event_cache = probe_event_cache(paths);
    println!("Herdr MCP doctor");
    print_check("runtime endpoint", runtime_healthy);
    print_check("Herdr local transport", report.herdr_transport_reachable);
    print_check("Herdr API schema", schema_healthy);
    print_check("validated Herdr RPC", native_call_healthy);
    print_check("Herdr snapshot state", snapshot_healthy);
    print_check("Herdr inspect projection", inspect_healthy);
    print_check("Herdr event cache", event_cache.healthy);
    print_layer_ownership(paths, config, &report);
    println!("INFO config {}", paths.config_file.display());
    println!("INFO state {}", paths.config_dir.display());
    println!("INFO dev-state {}", paths.dev_state_dir.display());
    if let Some(socket) = &paths.herdr_socket {
        println!("INFO herdr-socket {}", socket.display());
    }
    println!("INFO update-channel {}", config.update_channel.as_str());
    if let Some(count) = methods_result["count"].as_u64() {
        println!("INFO herdr-methods {count}");
    }
    if let Ok(snapshot_result) = &snapshot_result {
        println!("INFO snapshot-source {}", snapshot_result.source.as_str());
        println!(
            "INFO snapshot-counts workspaces={} panes={} agents={}",
            snapshot::collection_count(&snapshot_result.value, "workspaces"),
            snapshot::collection_count(&snapshot_result.value, "panes"),
            snapshot::collection_count(&snapshot_result.value, "agents")
        );
    }
    println!(
        "INFO event-cache cursor={} events={} agents={} workspaces={} panes={} stream-events={} reconcile={}",
        event_cache.cursor,
        event_cache.digest_events,
        event_cache.agents,
        event_cache.workspaces,
        event_cache.snapshot_panes,
        event_cache.stream_events,
        event_cache.needs_reconcile
    );
    if let Some(last_event_at) = &event_cache.last_event_at {
        println!("INFO event-cache-last-event {last_event_at}");
    }
    if let Some(error) = &event_cache.error {
        println!("WARN event-cache {error}");
    }

    runtime_healthy
        && report.herdr_transport_reachable
        && schema_healthy
        && native_call_healthy
        && snapshot_healthy
        && inspect_healthy
        && event_cache.healthy
}

/// Product-layer ownership map. Local probes only; never opens Edge/WSS.
fn print_layer_ownership(paths: &RuntimePaths, config: &Config, report: &StatusReport) {
    println!("LAYER herdr {}", format_herdr_layer(paths, report));
    println!(
        "LAYER local-runtime {}",
        format_local_runtime_layer(paths, config, report.runtime)
    );
    println!("LAYER service {}", format_service_layer());
    println!("LAYER local-ipc {}", format_local_ipc_layer(paths));
    println!("LAYER native-messaging {}", format_native_messaging_layer());
    println!("LAYER link {}", format_link_layer(paths));
    println!("LAYER edge {}", format_edge_layer(paths));
    println!("LAYER update-state {}", format_update_state_layer(paths));
}

fn format_herdr_layer(paths: &RuntimePaths, report: &StatusReport) -> String {
    let sock = paths
        .herdr_socket
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unset".to_owned());
    if report.herdr_transport_reachable {
        format!("owned reachable sock={sock}")
    } else {
        format!("unowned unreachable sock={sock}")
    }
}

fn format_local_runtime_layer(
    paths: &RuntimePaths,
    config: &Config,
    health: RuntimeHealth,
) -> String {
    let current = paths.config_dir.join("runtime").join("current");
    let generation = read_runtime_generation(&current);
    let health_label = match health {
        RuntimeHealth::Healthy(code) => format!("healthy http={code}"),
        RuntimeHealth::UnexpectedHttp(code) => format!("unexpected http={code}"),
        RuntimeHealth::Unreachable => "unreachable".to_owned(),
    };
    match generation {
        Ok(Some(generation)) => format!(
            "owned {health_label} port={} generation={generation}",
            config.runtime_port
        ),
        Ok(None) => format!(
            "unowned {health_label} port={} generation=missing",
            config.runtime_port
        ),
        Err(detail) => format!(
            "unowned {health_label} port={} generation=invalid detail={detail}",
            config.runtime_port
        ),
    }
}

fn format_service_layer() -> String {
    match service_manager::doctor_status() {
        Ok(value) => {
            let implementation = value
                .get("implementation")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let loaded = value
                .get("loaded")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let healthy = value
                .get("healthy")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let label = value
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("dev.herdr-mcp.server");
            let generation = value
                .get("generation")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let ownership = if implementation == "rust" && loaded {
                "owned"
            } else if implementation == "missing" {
                "absent"
            } else {
                "unowned"
            };
            format!(
                "{ownership} implementation={implementation} loaded={loaded} healthy={healthy} label={label} generation={generation}"
            )
        }
        Err(error) => format!("error detail={}", compact_detail(&error)),
    }
}

fn format_local_ipc_layer(paths: &RuntimePaths) -> String {
    let path = paths.config_dir.join("extension.sock");
    match inspect_unix_socket(&path) {
        SocketView::Present { mode } => {
            format!("owned present mode={mode:04o} path={}", path.display())
        }
        SocketView::Absent => format!("absent path={}", path.display()),
        SocketView::Invalid { detail } => {
            format!("unowned invalid path={} detail={detail}", path.display())
        }
    }
}

fn format_native_messaging_layer() -> String {
    match native_host_install::doctor_status() {
        Ok(value) => {
            let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
            let owned = value
                .get("owned_manifest_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let wrapper_ok = value
                .get("wrapper_ok")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let runtime_ok = value
                .get("runtime_binary_ok")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let ownership = if ok {
                "owned"
            } else if owned == 0 && !wrapper_ok && !runtime_ok {
                "absent"
            } else {
                "unowned"
            };
            format!(
                "{ownership} manifests={owned} wrapper_ok={wrapper_ok} runtime_binary_ok={runtime_ok}"
            )
        }
        Err(error) => format!("error detail={}", compact_detail(&error)),
    }
}

fn format_link_layer(paths: &RuntimePaths) -> String {
    let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
    crate::link::doctor_layer_summary(&home, &paths.config_dir)
}

fn format_edge_layer(_paths: &RuntimePaths) -> String {
    // Honest local view only: Edge host from link LaunchAgent env, never WSS dial.
    let Some(home) = home_dir() else {
        return "absent remote-probe=skipped".to_owned();
    };
    let candidates = [
        home.join("Library")
            .join("LaunchAgents")
            .join("dev.herdr-mcp.link-prod.plist"),
        home.join("Library")
            .join("LaunchAgents")
            .join("dev.herdr-mcp.link.plist"),
    ];
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        if let Some(host) = edge_host_from_plist(&path) {
            return format!(
                "configured-local host={host} plist={} remote-probe=skipped",
                path.display()
            );
        }
    }
    "absent remote-probe=skipped".to_owned()
}

fn format_update_state_layer(paths: &RuntimePaths) -> String {
    let db = paths.config_dir.join("update").join("state.db");
    if !db.is_file() {
        return "absent db=missing".to_owned();
    }
    match UpdateStore::open(paths).and_then(|store| store.latest_update_job()) {
        Ok(Some(job)) => format!(
            "owned job={} version={} state={}",
            job.job_id, job.version, job.state
        ),
        Ok(None) => "owned job=none".to_owned(),
        Err(error) => format!("error detail={}", compact_detail(&error)),
    }
}

fn read_runtime_generation(current: &Path) -> Result<Option<String>, String> {
    match fs::symlink_metadata(current) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot stat runtime/current: {error}")),
        Ok(metadata) => {
            if !metadata.file_type().is_symlink() {
                return Err("runtime/current is not a symlink".to_owned());
            }
            let target = fs::read_link(current)
                .map_err(|error| format!("cannot read runtime/current: {error}"))?;
            let name = target
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| "runtime/current target is not a generation id".to_owned())?;
            if !name.starts_with("rust-") {
                return Err(format!("unmanaged generation target {name}"));
            }
            Ok(Some(name.to_owned()))
        }
    }
}

#[derive(Debug)]
enum SocketView {
    Present { mode: u32 },
    Absent,
    Invalid { detail: String },
}

fn inspect_unix_socket(path: &Path) -> SocketView {
    #[cfg(unix)]
    {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => SocketView::Absent,
            Err(error) => SocketView::Invalid {
                detail: format!("stat-failed:{error}"),
            },
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return SocketView::Invalid {
                        detail: "symlink-refused".to_owned(),
                    };
                }
                if !metadata.file_type().is_socket() {
                    return SocketView::Invalid {
                        detail: "not-a-socket".to_owned(),
                    };
                }
                SocketView::Present {
                    mode: metadata.permissions().mode() & 0o777,
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        SocketView::Invalid {
            detail: "unix-socket-unsupported".to_owned(),
        }
    }
}

fn edge_host_from_plist(path: &Path) -> Option<String> {
    let value = plist::Value::from_file(path).ok()?;
    let env = value
        .as_dictionary()?
        .get("EnvironmentVariables")?
        .as_dictionary()?;
    let edge_url = env.get("HERDR_EDGE_URL")?.as_string()?;
    edge_host(edge_url)
}

fn edge_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("wss://") || trimmed.starts_with("ws://")) {
        return None;
    }
    let rest = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let host = rest.split(['/', '?', '#']).next()?.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_owned())
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn compact_detail(detail: &str) -> String {
    detail
        .chars()
        .map(|ch| if ch.is_whitespace() { '-' } else { ch })
        .take(120)
        .collect()
}

fn probe_event_cache(paths: &RuntimePaths) -> EventCacheProbe {
    let Some(socket) = paths.herdr_socket.as_ref() else {
        return EventCacheProbe {
            healthy: false,
            cursor: 0,
            digest_events: 0,
            agents: 0,
            workspaces: 0,
            snapshot_panes: 0,
            stream_events: 0,
            last_event_at: None,
            needs_reconcile: false,
            error: Some("Herdr local transport is unavailable".to_owned()),
        };
    };

    let mut cache = EventCache::start(HerdrClient::new(socket));
    let ready = cache.wait_ready(Duration::from_secs(2));
    let stream_live = cache.wait_stream_live(Duration::from_secs(1));
    let since_result = native_tools::since(&cache, 0, None);
    let snapshot_state = cache.snapshot();
    let diagnostics = cache.diagnostics();
    let error = cache.last_error();
    cache.shutdown();

    let cursor = since_result["cursor"].as_u64().unwrap_or(0);
    let digest_events = since_result["events"].as_array().map(Vec::len).unwrap_or(0);
    let agents = since_result["agents"].as_array().map(Vec::len).unwrap_or(0);
    let workspaces = since_result["workspaces"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);

    EventCacheProbe {
        healthy: ready
            && stream_live
            && since_result["ok"].as_bool() == Some(true)
            && error.is_none(),
        cursor,
        digest_events,
        agents,
        workspaces,
        snapshot_panes: snapshot::collection_count(&snapshot_state, "panes"),
        stream_events: diagnostics.event_count,
        last_event_at: diagnostics.last_event_at,
        needs_reconcile: diagnostics.needs_reconcile,
        error,
    }
}

fn probe_runtime(port: u16) -> RuntimeHealth {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(500)) else {
        return RuntimeHealth::Unreachable;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(750)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(750)));

    let body = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{}}"#;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return RuntimeHealth::Unreachable;
    }

    let mut buffer = [0_u8; 512];
    let Ok(count) = stream.read(&mut buffer) else {
        return RuntimeHealth::Unreachable;
    };
    let response = String::from_utf8_lossy(&buffer[..count]);
    match parse_http_status(&response) {
        Some(code @ (200 | 401)) => RuntimeHealth::Healthy(code),
        Some(code) => RuntimeHealth::UnexpectedHttp(code),
        None => RuntimeHealth::Unreachable,
    }
}

fn probe_herdr_transport(paths: &RuntimePaths) -> bool {
    paths
        .herdr_socket
        .as_ref()
        .is_some_and(|socket| HerdrClient::new(socket).ping().is_ok())
}

fn parse_http_status(response: &str) -> Option<u16> {
    let first_line = response.lines().next()?;
    let mut parts = first_line.split_whitespace();
    let protocol = parts.next()?;
    if !protocol.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

fn runtime_label(health: RuntimeHealth, port: u16) -> String {
    match health {
        RuntimeHealth::Healthy(code) => format!("127.0.0.1:{port} healthy (HTTP {code})"),
        RuntimeHealth::UnexpectedHttp(code) => {
            format!("127.0.0.1:{port} unexpected response (HTTP {code})")
        }
        RuntimeHealth::Unreachable => format!("127.0.0.1:{port} unreachable"),
    }
}

fn print_check(label: &str, pass: bool) {
    println!("{} {label}", if pass { "PASS" } else { "FAIL" });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_status_line() {
        assert_eq!(
            parse_http_status("HTTP/1.1 401 Unauthorized\r\n"),
            Some(401)
        );
        assert_eq!(parse_http_status("HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(parse_http_status("not-http"), None);
    }

    #[test]
    fn labels_runtime_state() {
        assert!(runtime_label(RuntimeHealth::Healthy(401), 8772).contains("healthy"));
        assert!(runtime_label(RuntimeHealth::Unreachable, 8772).contains("unreachable"));
    }

    #[test]
    fn extracts_edge_host_without_credentials() {
        assert_eq!(
            edge_host("wss://herdr-edge-prod.example/ws?link_token=secret"),
            Some("herdr-edge-prod.example".to_owned())
        );
        assert_eq!(edge_host("https://example"), None);
    }

    #[test]
    fn reads_managed_runtime_generation_symlink() {
        let root = std::env::temp_dir().join(format!(
            "herdr-doctor-gen-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("generations").join("rust-abc123")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink("generations/rust-abc123", root.join("current")).unwrap();
            assert_eq!(
                read_runtime_generation(&root.join("current")).unwrap(),
                Some("rust-abc123".to_owned())
            );
        }
        let _ = fs::remove_dir_all(&root);
    }
}
