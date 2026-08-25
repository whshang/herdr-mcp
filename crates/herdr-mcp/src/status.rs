use crate::config::Config;
use crate::herdr::HerdrClient;
use crate::native_tools;
use crate::paths::RuntimePaths;
use crate::snapshot;
use crate::state_cache::EventCache;
use serde_json::json;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

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
        .map(|socket| native_tools::inspect(&HerdrClient::new(socket), None))
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
}
