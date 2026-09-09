use crate::cli::OutputMode;
use crate::config::Config;
use crate::herdr::HerdrClient;
use crate::herdr_supervisor;
use crate::locale::{self, Locale};
use crate::macos_privacy;
use crate::native_host_install;
use crate::native_tools;
use crate::paths::RuntimePaths;
use crate::service_manager;
use crate::snapshot;
use crate::state_cache::{EventCache, EventCacheHealth};
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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum DiagnosticState {
    Pass,
    Fail,
    NotProbed,
}

impl DiagnosticState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotProbed => "not_probed",
        }
    }
}

#[derive(Debug, Clone)]
struct AuthenticatedMcpProbe {
    state: DiagnosticState,
    detail: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum OverallReadiness {
    Fail,
    NotProven,
}

impl OverallReadiness {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fail => "fail",
            Self::NotProven => "not_proven",
        }
    }
}

#[derive(Debug)]
struct EventCacheProbe {
    healthy: bool,
    mode: &'static str,
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

/// A collected report is rendered without performing any further probes.
#[derive(Debug)]
struct CliReport {
    facts: Value,
    details: Vec<String>,
}

impl CliReport {
    fn render(&self, mode: OutputMode, language: Locale, doctor: bool) -> String {
        locale::render_report(&self.facts, &self.details, mode, language, doctor)
    }
}

fn pass_code(pass: bool) -> &'static str {
    if pass { "pass" } else { "fail" }
}

fn aggregate_check(states: &[&str]) -> &'static str {
    if states.contains(&"fail") {
        "fail"
    } else if states.contains(&"unconfigured") {
        "unconfigured"
    } else if states.iter().all(|s| *s == "not_applicable") {
        "not_applicable"
    } else if states
        .iter()
        .any(|s| !matches!(*s, "pass" | "not_applicable"))
    {
        "not_probed"
    } else {
        "pass"
    }
}

fn browser_states(
    native_messaging: &Result<Value, String>,
    ipc: &SocketView,
) -> (&'static str, &'static str) {
    let browser_state = match native_messaging {
        Ok(v) if v["implementation"] == "unsupported" => "not_applicable",
        Ok(v) if v["ok"] == true => "pass",
        Ok(v)
            if v["owned_manifest_count"].as_u64().unwrap_or(0) == 0
                && v["wrapper_ok"] != true
                && v["runtime_binary_ok"] != true =>
        {
            "unconfigured"
        }
        _ => "fail",
    };
    let ipc_state = if browser_state == "not_applicable" {
        "not_applicable"
    } else {
        match ipc {
            SocketView::Present { .. } => "pass",
            SocketView::Absent => "not_probed",
            SocketView::Invalid { .. } => "fail",
        }
    };
    (browser_state, ipc_state)
}

fn scheduler_code(value: &Value) -> &'static str {
    if value["skipped"] == true {
        "not_applicable"
    } else if value["ok"] == true {
        "enabled"
    } else if value["present"] == false {
        "not_installed"
    } else if value["owned"] == true && value["loaded"] == false {
        "not_loaded"
    } else {
        "unknown"
    }
}

pub fn print_status(paths: &RuntimePaths, config: &Config, mode: OutputMode, language: Locale) {
    let report = collect(paths, config);
    let service = service_manager::doctor_status();
    let service_ok = service
        .as_ref()
        .is_ok_and(|v| v["healthy"] == true && v["loaded"] == true)
        && matches!(report.runtime, RuntimeHealth::Healthy(_));
    let link = collect_link(paths, &service);
    let edge = resolve_edge_config(config);
    let scheduler_snapshot = crate::update_scheduler::status_snapshot();
    let scheduler = scheduler_snapshot
        .as_ref()
        .cloned()
        .unwrap_or_else(|_| json!({}));
    let facts = json!({
        "version": crate::runtime_meta::runtime_version(),
        "overall": pass_code(service_ok && report.herdr_transport_reachable && (edge.is_none() || link_state(&link) != "fail")),
        "service_health": pass_code(service_ok),
        "herdr": pass_code(report.herdr_transport_reachable),
        "link": if edge.is_none() { "unconfigured" } else { link_state(&link) },
        "cloud": if edge.is_some() { "configured" } else { "unconfigured" },
        "update_channel": config.update_channel.as_str(),
        "update_checks": if config.update_check { "enabled" } else { "disabled" },
        "scheduler": scheduler_code(&scheduler),
    });
    let details = vec![
        format!(
            "runtime channel: {}",
            crate::runtime_meta::runtime_channel()
        ),
        format!(
            "runtime source: {} dirty={}",
            crate::runtime_meta::compiled_source_commit().unwrap_or("release"),
            crate::runtime_meta::compiled_source_dirty()
        ),
        format!("config: {}", paths.config_file.display()),
        format!(
            "runtime: {}",
            runtime_label(report.runtime, config.runtime_port)
        ),
        format!(
            "runtime ownership: {}",
            format_local_runtime_layer(paths, config, report.runtime)
        ),
        format!("service: {}", format_service_layer(&service)),
        format!(
            "tcc broker: {}",
            crate::tcc_broker::status_line(&paths.config_dir)
        ),
        format!("link: {link}"),
        format!(
            "auto update scheduler: {}",
            crate::update_scheduler::status_line(&scheduler_snapshot)
        ),
        format!("edge: {}", format_edge_configured_layer(&edge, config)),
        format!("lifecycle residue: {}", crate::residue::status_line()),
        format!(
            "relay pool: {}",
            crate::link::relay_manifest::status_line(paths, unix_now_seconds())
        ),
        format!("relay use: {}", crate::link::RELAY_POLICY_DESCRIPTION),
    ];
    print!(
        "{}",
        CliReport { facts, details }.render(mode, language, false)
    );
}

pub fn print_doctor(
    paths: &RuntimePaths,
    config: &Config,
    mode: OutputMode,
    language: Locale,
) -> bool {
    let mut details = Vec::new();
    let report = collect(paths, config);
    let runtime_healthy = matches!(report.runtime, RuntimeHealth::Healthy(_));
    let methods_result = native_tools::methods("");
    // `native_tools::methods` can still return the local progressive-method
    // registry when live Herdr schema reflection is unavailable. Doctor must
    // prove the live schema itself rather than letting that local fallback
    // mask a broken Herdr executable lookup.
    let schema_healthy = crate::schema::list_methods("").is_ok();
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
    let documents_permission = macos_privacy::probe_documents_permission(&paths.config_dir);
    let code_identity = macos_privacy::probe_code_identity();
    let authenticated_local_mcp = probe_authenticated_local_mcp(config.runtime_port);
    details.push("Herdr MCP doctor".to_owned());
    details.push(format!(
        "runtime provenance: channel={} version={} source={}{}",
        crate::runtime_meta::runtime_channel(),
        crate::runtime_meta::runtime_version(),
        crate::runtime_meta::compiled_source_commit().unwrap_or("release"),
        if crate::runtime_meta::compiled_source_dirty() {
            " dirty"
        } else {
            ""
        }
    ));
    collect_check(&mut details, "runtime endpoint", runtime_healthy);
    collect_check(
        &mut details,
        "Herdr local transport",
        report.herdr_transport_reachable,
    );
    collect_check(&mut details, "Herdr API schema", schema_healthy);
    collect_check(&mut details, "validated Herdr RPC", native_call_healthy);
    collect_check(&mut details, "Herdr snapshot state", snapshot_healthy);
    collect_check(&mut details, "Herdr inspect projection", inspect_healthy);
    collect_check(&mut details, "Herdr event cache", event_cache.healthy);
    let macos_permissions = crate::macos_permissions::collect_status();
    details.push(documents_permission.doctor_line());
    details.push(crate::macos_permissions::doctor_layer_from(
        &macos_permissions,
    ));
    details.push(crate::tcc_broker::doctor_line(&paths.config_dir));
    details.push(code_identity.doctor_line());
    details.push(herdr_supervisor::doctor_line());
    details.push(crate::child_process::doctor_line());
    let service = service_manager::doctor_status();
    let native_messaging = native_host_install::doctor_status();
    let ipc = inspect_unix_socket(&paths.config_dir.join("extension.sock"));
    let link = collect_link(paths, &service);
    details.push(format!(
        "LAYER herdr {}",
        format_herdr_layer(paths, &report)
    ));
    details.push(format!(
        "LAYER local-runtime {}",
        format_local_runtime_layer(paths, config, report.runtime)
    ));
    details.push(format!("LAYER service {}", format_service_layer(&service)));
    details.push(format!(
        "LAYER local-ipc {}",
        format_local_ipc_layer(paths, &ipc)
    ));
    details.push(format!(
        "LAYER native-messaging {}",
        format_native_messaging_layer(&native_messaging)
    ));
    details.push(format!(
        "LAYER link {}",
        if cfg!(target_os = "macos") {
            crate::link::doctor_layer_summary(&link)
        } else {
            link.to_string()
        }
    ));
    details.push(format!(
        "LAYER link-transport {}",
        format_link_transport_layer(paths, config)
    ));
    details.push(format!(
        "LAYER relay-pool {}",
        crate::link::relay_manifest::status_line(paths, unix_now_seconds())
    ));
    let edge = resolve_edge_config(config);
    details.push(format!(
        "LAYER edge {}",
        format_edge_configured_layer(&edge, config)
    ));
    let remote = edge
        .as_ref()
        .map(probe_edge_remote)
        .unwrap_or(RemoteProbeReport::absent());
    details.push(format!("LAYER edge-reachable {}", remote.edge_reachable));
    details.push(format!("LAYER oauth-metadata {}", remote.oauth_metadata));
    details.push(format!("LAYER mcp-endpoint {}", remote.mcp_endpoint));
    details.push(format!(
        "LAYER update-state {}",
        format_update_state_layer(paths)
    ));
    details.push(crate::residue::doctor_line());
    details.push(format!(
        "LAYER authenticated-local-mcp {}",
        authenticated_local_mcp.detail
    ));
    details.push(
        "LAYER authenticated-remote-mcp not_probed reason=no-connector-oauth-credential".to_owned(),
    );
    let service_ok = service
        .as_ref()
        .is_ok_and(|v| v["healthy"] == true && v["loaded"] == true);
    let (browser_state, ipc_state) = browser_states(&native_messaging, &ipc);
    let service_health = (edge.is_none() || link_state(&link) != "fail")
        && runtime_healthy
        && service_ok
        && browser_state != "fail"
        && ipc_state != "fail"
        && report.herdr_transport_reachable
        && schema_healthy
        && native_call_healthy
        && snapshot_healthy
        && inspect_healthy
        && event_cache.healthy
        && documents_permission.doctor_pass()
        && macos_permissions
            .as_ref()
            .map(crate::macos_permissions::report_doctor_pass)
            .unwrap_or(true);
    let readiness = overall_readiness(service_health, authenticated_local_mcp.state, &remote);
    details.push(format!(
        "READINESS service_health={} authenticated_local_mcp={} authenticated_remote_mcp=not_probed end_to_end_readiness={}",
        if service_health { "pass" } else { "fail" },
        authenticated_local_mcp.state.as_str(),
        readiness.as_str()
    ));
    let permissions_ok = documents_permission.doctor_pass()
        && macos_permissions
            .as_ref()
            .map(crate::macos_permissions::report_doctor_pass)
            .unwrap_or(true);
    let herdr_ok = report.herdr_transport_reachable
        && schema_healthy
        && native_call_healthy
        && snapshot_healthy
        && inspect_healthy
        && event_cache.healthy;
    let facts = json!({
        "version": crate::runtime_meta::runtime_version(),
        "service_health": pass_code(service_health),
        "herdr": pass_code(herdr_ok),
        "permissions": if !cfg!(target_os = "macos") { "not_applicable" }
            else if macos_permissions.is_err() { "not_probed" }
            else { pass_code(permissions_ok) },
        "browser_integration": aggregate_check(&[browser_state, ipc_state]),
        "cloud_health": if edge.is_none() { "unconfigured" } else { aggregate_check(&[remote.edge_state.as_str(), remote.oauth_state.as_str(), remote.mcp_surface_state.as_str()]) },
        "browser": browser_state,
        "local_ipc": ipc_state,
        "link": if remote.edge_state == DiagnosticState::NotProbed { "unconfigured" } else { link_state(&link) },
        "authenticated_local_mcp": authenticated_local_mcp.state.as_str(),
        "authenticated_remote_mcp": "not_probed",
        "remote_reason": "no_connector_oauth_credential",
        "edge_reachable": remote.edge_state.as_str(),
        "oauth_metadata": remote.oauth_state.as_str(),
        "mcp_surface": remote.mcp_surface_state.as_str(),
        "overall": pass_code(readiness != OverallReadiness::Fail),
        "next_step": if !runtime_healthy || !service_ok { "repair_local" }
            else if !herdr_ok { "connect_herdr" }
            else if !permissions_ok { "permissions_setup" }
            else if readiness == OverallReadiness::Fail { "inspect_details" }
            else { "verify_remote_mcp" },
    });
    details.push(format!("INFO config {}", paths.config_file.display()));
    details.push(format!("INFO state {}", paths.config_dir.display()));
    details.push(format!("INFO dev-state {}", paths.dev_state_dir.display()));
    if let Some(socket) = &paths.herdr_socket {
        details.push(format!("INFO herdr-socket {}", socket.display()));
    }
    details.push(format!(
        "INFO update-channel {}",
        config.update_channel.as_str()
    ));
    if let Some(count) = methods_result["count"].as_u64() {
        details.push(format!("INFO herdr-methods {count}"));
    }
    if let Ok(snapshot_result) = &snapshot_result {
        details.push(format!(
            "INFO snapshot-source {}",
            snapshot_result.source.as_str()
        ));
        details.push(format!(
            "INFO snapshot-counts workspaces={} panes={} agents={}",
            snapshot::collection_count(&snapshot_result.value, "workspaces"),
            snapshot::collection_count(&snapshot_result.value, "panes"),
            snapshot::collection_count(&snapshot_result.value, "agents")
        ));
    }
    details.push(format!(
        "INFO event-cache cursor={} events={} agents={} workspaces={} panes={} stream-events={} reconcile={} mode={}",
        event_cache.cursor,
        event_cache.digest_events,
        event_cache.agents,
        event_cache.workspaces,
        event_cache.snapshot_panes,
        event_cache.stream_events,
        event_cache.needs_reconcile,
        event_cache.mode
    ));
    if let Some(last_event_at) = &event_cache.last_event_at {
        details.push(format!("INFO event-cache-last-event {last_event_at}"));
    }
    if let Some(error) = &event_cache.error {
        details.push(format!("WARN event-cache {error}"));
    }

    print!(
        "{}",
        CliReport { facts, details }.render(mode, language, true)
    );
    service_health && readiness != OverallReadiness::Fail
}

fn unix_now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
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

fn format_service_layer(service: &Result<Value, String>) -> String {
    match service {
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
        Err(error) => format!("error detail={}", compact_detail(error)),
    }
}

fn format_local_ipc_layer(paths: &RuntimePaths, socket: &SocketView) -> String {
    let path = paths.config_dir.join("extension.sock");
    match socket {
        SocketView::Present { mode } => {
            format!("owned present mode={mode:04o} path={}", path.display())
        }
        SocketView::Absent => format!("absent path={}", path.display()),
        SocketView::Invalid { detail } => {
            format!("unowned invalid path={} detail={detail}", path.display())
        }
    }
}

fn format_native_messaging_layer(status: &Result<Value, String>) -> String {
    match status {
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
            let runtime_matches = value
                .get("runtime_matches_current")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let version_consistent = value
                .get("version_consistent")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let stale_runtime = value
                .get("stale_runtime")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let ownership = if ok {
                "owned"
            } else if owned == 0 && !wrapper_ok && !runtime_ok {
                "absent"
            } else {
                "unowned"
            };
            let stale = if stale_runtime { " stale-runtime" } else { "" };
            format!(
                "{ownership}{stale} manifests={owned} wrapper_ok={wrapper_ok} runtime_binary_ok={runtime_ok} runtime_matches_current={runtime_matches} version_consistent={version_consistent}"
            )
        }
        Err(error) => format!("error detail={}", compact_detail(error)),
    }
}

fn collect_link(paths: &RuntimePaths, service: &Result<Value, String>) -> Value {
    #[cfg(target_os = "macos")]
    {
        let _ = service;
        let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
        crate::link::ownership::collect_status_report(&home, &paths.config_dir)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = paths;
        // Reuse the exact systemd/process owner already inspected by service status.
        json!({
            "state": link_state_from_service(service),
            "verification": "service_owner_only"
        })
    }
}

#[cfg(any(not(target_os = "macos"), test))]
fn link_state_from_service(service: &Result<Value, String>) -> &'static str {
    match service {
        Ok(value) if value["link_loaded"] == true => "pass",
        Ok(value) if value["link_loaded"] == false => "fail",
        _ => "not_probed",
    }
}

fn link_state(link: &Value) -> &'static str {
    match link["state"].as_str() {
        Some("pass" | "running") => "pass",
        Some("fail") => "fail",
        Some("unconfigured") => "unconfigured",
        Some("not_probed") => "not_probed",
        Some(_) => "not_probed",
        None => match link["operational_ready"].as_bool() {
            Some(true) => "pass",
            Some(false) => "fail",
            None => "not_probed",
        },
    }
}

fn format_link_transport_layer(paths: &RuntimePaths, config: &Config) -> String {
    let pool = crate::link::relay_manifest::load_cached_pool(paths, unix_now_seconds());
    let evidence = crate::link::collect_transport_evidence_with_pool(
        config.edge_public_origin.as_deref(),
        config.edge_link_upstream_origin.as_deref(),
        &pool.relays,
        pool.source,
    );
    format!(
        "mcp_origin={} link_upstream={} live_transport={} configured_preferred_transport={} proxy_source={} relay={} relay_policy={} relay_selection={} pool_source={} failover_ready={}",
        evidence.mcp_origin,
        evidence.link_upstream,
        evidence.live_transport,
        evidence.configured_preferred_transport,
        evidence.proxy_source,
        evidence.relay,
        evidence.relay_policy,
        evidence.relay_selection,
        evidence.pool_source,
        evidence.failover_ready,
    )
}

fn format_edge_configured_layer(edge: &Option<EdgeConfigView>, config: &Config) -> String {
    let upstream_info = match config.edge_link_upstream_origin.as_deref() {
        Some(upstream) => format!(" upstream={upstream}"),
        None => String::new(),
    };
    match edge {
        Some(edge) => format!(
            "configured-local source={} label={} host={} origin={}{} plist={}",
            edge.source.as_str(),
            edge.label.as_deref().unwrap_or("-"),
            edge.host,
            edge.origin,
            upstream_info,
            edge.plist
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "unset".to_owned())
        ),
        None => "unconfigured reason=no-link-plist-or-HERDR_EDGE_URL".to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeConfigSource {
    ConfigToml,
    LinkProdPlist,
    LinkPlist,
    LinkCandidatePlist,
    ProcessEnv,
}

impl EdgeConfigSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::ConfigToml => "config-toml",
            Self::LinkProdPlist => "link-prod-plist",
            Self::LinkPlist => "link-plist",
            Self::LinkCandidatePlist => "link-candidate-plist",
            Self::ProcessEnv => "link-env",
        }
    }
}

#[derive(Debug, Clone)]
struct EdgeConfigView {
    host: String,
    origin: String,
    plist: Option<PathBuf>,
    source: EdgeConfigSource,
    label: Option<String>,
}

#[derive(Debug, Clone)]
struct RemoteProbeReport {
    edge_reachable: String,
    oauth_metadata: String,
    mcp_endpoint: String,
    edge_state: DiagnosticState,
    oauth_state: DiagnosticState,
    mcp_surface_state: DiagnosticState,
}

impl RemoteProbeReport {
    fn absent() -> Self {
        Self {
            edge_reachable: "skipped reason=edge-unconfigured".to_owned(),
            oauth_metadata: "skipped reason=edge-unconfigured".to_owned(),
            mcp_endpoint: "skipped reason=edge-unconfigured".to_owned(),
            edge_state: DiagnosticState::NotProbed,
            oauth_state: DiagnosticState::NotProbed,
            mcp_surface_state: DiagnosticState::NotProbed,
        }
    }
}

fn resolve_edge_config(config: &Config) -> Option<EdgeConfigView> {
    let home = home_dir();

    // Check if a real link LaunchAgent plist exists
    let plist_info = home.as_ref().and_then(|h| {
        let plist_candidates = [
            ("dev.herdr-mcp.link-prod", EdgeConfigSource::LinkProdPlist),
            ("dev.herdr-mcp.link", EdgeConfigSource::LinkPlist),
            (
                "dev.herdr-mcp.link-rust-candidate",
                EdgeConfigSource::LinkCandidatePlist,
            ),
        ];
        for (label, source) in plist_candidates {
            let path = h
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{label}.plist"));
            if path.is_file() {
                let host = edge_host_from_plist(&path);
                return Some((path, label.to_owned(), source, host));
            }
        }
        None
    });

    // If [edge].public_origin is configured in config.toml, it is the authoritative public identity
    if let Some(public_origin) = config.edge_public_origin.as_deref()
        && let Ok(parsed) = url::Url::parse(public_origin)
        && let Some(host) = parsed.host_str()
    {
        return Some(EdgeConfigView {
            host: host.to_owned(),
            origin: public_origin.to_owned(),
            plist: plist_info.as_ref().map(|(p, _, _, _)| p.clone()),
            source: EdgeConfigSource::ConfigToml,
            label: plist_info.as_ref().map(|(_, l, _, _)| l.clone()),
        });
    }

    if let Some((path, label, source, Some(host))) = plist_info
        && let Some(origin) = https_origin_for_host(&host)
    {
        return Some(EdgeConfigView {
            host,
            origin,
            plist: Some(path),
            source,
            label: Some(label),
        });
    }

    let edge_url = std::env::var("HERDR_EDGE_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())?;
    let host = edge_host(&edge_url)?;
    let origin = https_origin_for_host(&host)?;
    Some(EdgeConfigView {
        host,
        origin,
        plist: None,
        source: EdgeConfigSource::ProcessEnv,
        label: None,
    })
}

fn https_origin_for_host(host: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() || host.contains('/') || host.contains('@') || host.contains(' ') {
        return None;
    }
    // Refuse credential-shaped hosts and keep output host-only.
    if host.contains(':') && !host.starts_with('[') {
        // allow host:port
        let (name, port) = host.split_once(':')?;
        if name.is_empty() || port.parse::<u16>().is_err() {
            return None;
        }
    }
    Some(format!("https://{host}"))
}

fn probe_edge_remote(edge: &EdgeConfigView) -> RemoteProbeReport {
    let client = match remote_probe_client() {
        Ok(client) => client,
        Err(detail) => {
            let failed = format!("error detail={}", compact_detail(&detail));
            return RemoteProbeReport {
                edge_reachable: failed.clone(),
                oauth_metadata: failed.clone(),
                mcp_endpoint: failed,
                edge_state: DiagnosticState::Fail,
                oauth_state: DiagnosticState::Fail,
                mcp_surface_state: DiagnosticState::Fail,
            };
        }
    };

    let health_url = format!("{}/health", edge.origin);
    let oauth_url = format!("{}/.well-known/oauth-authorization-server", edge.origin);
    let mcp_url = format!("{}/mcp", edge.origin);

    let (edge_reachable, edge_state) =
        match probe_https_get(&client, &health_url, RemoteExpect::Health) {
            Ok(summary) => (format!("reachable {summary}"), DiagnosticState::Pass),
            Err(detail) => (
                format!("unreachable detail={}", compact_detail(&detail)),
                DiagnosticState::Fail,
            ),
        };
    let (oauth_metadata, oauth_state) =
        match probe_https_get(&client, &oauth_url, RemoteExpect::OauthMetadata) {
            Ok(summary) => (format!("reachable {summary}"), DiagnosticState::Pass),
            Err(detail) => (
                format!("unreachable detail={}", compact_detail(&detail)),
                DiagnosticState::Fail,
            ),
        };
    let (mcp_endpoint, mcp_surface_state) =
        match probe_https_get(&client, &mcp_url, RemoteExpect::McpEndpoint) {
            Ok(summary) => (format!("reachable {summary}"), DiagnosticState::Pass),
            Err(detail) => (
                format!("unreachable detail={}", compact_detail(&detail)),
                DiagnosticState::Fail,
            ),
        };

    RemoteProbeReport {
        edge_reachable,
        oauth_metadata,
        mcp_endpoint,
        edge_state,
        oauth_state,
        mcp_surface_state,
    }
}

#[derive(Debug, Clone, Copy)]
enum RemoteExpect {
    Health,
    OauthMetadata,
    McpEndpoint,
}

const REMOTE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const REMOTE_PROBE_MAX_BYTES: usize = 64 * 1024;

fn remote_probe_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(REMOTE_PROBE_TIMEOUT)
        .connect_timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|error| format!("cannot build remote probe client: {error}"))
}

fn probe_https_get(
    client: &reqwest::blocking::Client,
    url: &str,
    expect: RemoteExpect,
) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|_| "invalid probe URL".to_owned())?;
    if parsed.scheme() != "https" {
        return Err("remote probe requires https".to_owned());
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err("probe URL must not carry credentials".to_owned());
    }
    if parsed.query().is_some() {
        return Err("probe URL must not carry query credentials".to_owned());
    }

    let response = client
        .get(parsed)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .map_err(|error| format!("request failed: {error}"))?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .map_err(|error| format!("read failed: {error}"))?;
    if bytes.len() > REMOTE_PROBE_MAX_BYTES {
        return Err("response exceeds probe byte budget".to_owned());
    }
    let body = String::from_utf8_lossy(&bytes);

    match expect {
        RemoteExpect::Health => {
            if status != 200 {
                return Err(format!("unexpected http={status}"));
            }
            let service =
                json_string_field(&body, "service").unwrap_or_else(|| "unknown".to_owned());
            let epoch = json_u64_field(&body, "contractEpoch")
                .map(|epoch| epoch.to_string())
                .unwrap_or_else(|| "unknown".to_owned());
            Ok(format!(
                "http={status} service={} contract_epoch={}",
                sanitize_probe_token(&service),
                sanitize_probe_token(&epoch)
            ))
        }
        RemoteExpect::OauthMetadata => {
            if status != 200 {
                return Err(format!("unexpected http={status}"));
            }
            let issuer = json_string_field(&body, "issuer")
                .and_then(|issuer| issuer_host(&issuer))
                .unwrap_or_else(|| "unknown".to_owned());
            Ok(format!(
                "http={status} issuer_host={}",
                sanitize_probe_token(&issuer)
            ))
        }
        RemoteExpect::McpEndpoint => {
            // Never send Authorization. 401 proves the public MCP surface exists.
            if matches!(status, 200 | 401) {
                Ok(format!("http={status} auth=not-sent"))
            } else {
                Err(format!("unexpected http={status}"))
            }
        }
    }
}

fn json_string_field(body: &str, key: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn json_u64_field(body: &str, key: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value.get(key).and_then(Value::as_u64)
}

fn issuer_host(issuer: &str) -> Option<String> {
    let parsed = url::Url::parse(issuer).ok()?;
    if parsed.username() != "" || parsed.password().is_some() || parsed.query().is_some() {
        return None;
    }
    parsed.host_str().map(str::to_owned)
}

pub(crate) fn sanitize_probe_token(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("token")
        || lower.contains("secret")
        || lower.contains("bearer")
        || lower.contains("authorization")
        || value.contains('=')
        || value.len() > 96
    {
        return "redacted".to_owned();
    }
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ':') {
                ch
            } else {
                '-'
            }
        })
        .take(64)
        .collect()
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
            mode: "failed",
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
    // Keep the initial ready/live waits unchanged. The extra reconcile budget only
    // covers an already-observed resubscribe / needs_reconcile window so doctor
    // does not randomly FAIL while the cache is mid-cycle.
    let health = cache.wait_for_doctor_probe(
        Duration::from_secs(2),
        Duration::from_secs(1),
        Duration::from_secs(2),
    );
    let since_result = native_tools::since(&cache, 0, None, Ok(None));
    let snapshot_state = cache.snapshot();
    let diagnostics = cache.diagnostics();
    let since_ok = since_result["ok"].as_bool() == Some(true);
    let error = health
        .error_message()
        .map(str::to_owned)
        .or_else(|| cache.last_error());
    cache.shutdown();

    let cursor = since_result["cursor"].as_u64().unwrap_or(0);
    let digest_events = since_result["events"].as_array().map(Vec::len).unwrap_or(0);
    let agents = since_result["agents"].as_array().map(Vec::len).unwrap_or(0);
    let workspaces = since_result["workspaces"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);

    EventCacheProbe {
        healthy: event_cache_doctor_pass(&health, since_ok),
        mode: health.mode(),
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

fn event_cache_doctor_pass(health: &EventCacheHealth, since_ok: bool) -> bool {
    health.doctor_pass() && since_ok
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

fn probe_authenticated_local_mcp(port: u16) -> AuthenticatedMcpProbe {
    let token = match service_manager::doctor_runtime_token() {
        Ok(Some(token)) => token,
        Ok(None) => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::NotProbed,
                detail: "not_probed reason=runtime-bearer-unavailable".to_owned(),
            };
        }
        Err(error) => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::NotProbed,
                detail: format!(
                    "not_probed reason=runtime-bearer-unavailable detail={}",
                    compact_detail(&error)
                ),
            };
        }
    };
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .connect_timeout(Duration::from_secs(1))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::Fail,
                detail: format!(
                    "fail phase=client detail={}",
                    compact_detail(&error.to_string())
                ),
            };
        }
    };
    let url = format!("http://127.0.0.1:{port}/mcp");
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": "doctor-init",
        "method": "initialize",
        "params": {
            "protocolVersion": crate::mcp::SDK_WIRE_PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "herdr-doctor", "version": crate::runtime_meta::runtime_version()}
        }
    });
    let response = match client
        .post(&url)
        .bearer_auth(&token)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&initialize)
        .send()
    {
        Ok(response) => response,
        Err(error) => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::Fail,
                detail: format!(
                    "fail phase=initialize detail={}",
                    compact_detail(&error.to_string())
                ),
            };
        }
    };
    if response.status().as_u16() != 200 {
        return AuthenticatedMcpProbe {
            state: DiagnosticState::Fail,
            detail: format!("fail phase=initialize http={}", response.status().as_u16()),
        };
    }
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let initialize_bytes = match response.bytes() {
        Ok(bytes) => bytes,
        Err(error) => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::Fail,
                detail: format!(
                    "fail phase=initialize-read detail={}",
                    compact_detail(&error.to_string())
                ),
            };
        }
    };
    let initialize_payload: Value = match serde_json::from_slice(&initialize_bytes) {
        Ok(value) => value,
        Err(_) => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::Fail,
                detail: "fail phase=initialize-decode".to_owned(),
            };
        }
    };
    let Some(protocol) = initialize_payload
        .pointer("/result/protocolVersion")
        .and_then(Value::as_str)
    else {
        return AuthenticatedMcpProbe {
            state: DiagnosticState::Fail,
            detail: "fail phase=initialize-result".to_owned(),
        };
    };
    let Some(session_id) = session_id else {
        return AuthenticatedMcpProbe {
            state: DiagnosticState::Fail,
            detail: "fail phase=initialize-session".to_owned(),
        };
    };

    let list =
        json!({"jsonrpc": "2.0", "id": "doctor-tools", "method": "tools/list", "params": {}});
    let response = match client
        .post(&url)
        .bearer_auth(&token)
        .header(reqwest::header::ACCEPT, "application/json")
        .header("mcp-session-id", &session_id)
        .json(&list)
        .send()
    {
        Ok(response) => response,
        Err(error) => {
            let _ = client
                .delete(&url)
                .bearer_auth(&token)
                .header("mcp-session-id", &session_id)
                .send();
            return AuthenticatedMcpProbe {
                state: DiagnosticState::Fail,
                detail: format!(
                    "fail phase=tools-list detail={}",
                    compact_detail(&error.to_string())
                ),
            };
        }
    };
    let status = response.status().as_u16();
    let list_bytes = response.bytes();
    let _ = client
        .delete(&url)
        .bearer_auth(&token)
        .header("mcp-session-id", &session_id)
        .send();
    if status != 200 {
        return AuthenticatedMcpProbe {
            state: DiagnosticState::Fail,
            detail: format!("fail phase=tools-list http={status}"),
        };
    }
    let list_payload: Value = match list_bytes
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(value) => value,
        None => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::Fail,
                detail: "fail phase=tools-list-decode".to_owned(),
            };
        }
    };
    let Some(tools) = list_payload
        .pointer("/result/tools")
        .and_then(Value::as_array)
    else {
        return AuthenticatedMcpProbe {
            state: DiagnosticState::Fail,
            detail: "fail phase=tools-list-result".to_owned(),
        };
    };
    AuthenticatedMcpProbe {
        state: DiagnosticState::Pass,
        detail: format!(
            "pass protocol={} tools={}",
            sanitize_probe_token(protocol),
            tools.len()
        ),
    }
}

fn overall_readiness(
    service_health: bool,
    local_authenticated: DiagnosticState,
    remote: &RemoteProbeReport,
) -> OverallReadiness {
    if !service_health
        || local_authenticated == DiagnosticState::Fail
        || remote.edge_state == DiagnosticState::Fail
        || remote.oauth_state == DiagnosticState::Fail
        || remote.mcp_surface_state == DiagnosticState::Fail
    {
        OverallReadiness::Fail
    } else {
        // Doctor intentionally does not possess or mint a user's Connector
        // OAuth credential. A healthy public MCP surface therefore proves
        // reachability, not authenticated end-to-end Connector usability.
        OverallReadiness::NotProven
    }
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

fn collect_check(details: &mut Vec<String>, label: &str, pass: bool) {
    details.push(format!("{} {label}", if pass { "PASS" } else { "FAIL" }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_one_report_as_localized_human_details_and_stable_json() {
        let report = CliReport {
            facts: json!({
                "version": "1.0.0", "overall": "pass", "service_health": "pass",
                "herdr": "pass", "authenticated_local_mcp": "pass",
                "authenticated_remote_mcp": "not_probed", "next_step": "verify_remote_mcp",
                "permissions": "not_applicable",
                "browser": "pass", "local_ipc": "pass", "browser_integration": aggregate_check(&["pass", "pass"]),
                "edge_reachable": "pass", "oauth_metadata": "pass", "mcp_surface": "pass",
                "cloud_health": aggregate_check(&["pass", "pass", "pass"]), "link": "pass",
                "update_channel": "stable", "scheduler": "enabled"
            }),
            details: vec![
                "LAYER source=abc generation=rust-abc path=/private/runtime".into(),
                "INFO config /private/config".into(),
            ],
        };
        let mut baseline = None;
        for (language, heading, remote) in [
            (Locale::En, "Overall", "Not verified"),
            (Locale::ZhCn, "整体状态", "未验证"),
            (Locale::Ja, "全体の状態", "未検証"),
        ] {
            let human = report.render(OutputMode::Human, language, true);
            assert!(human.contains(heading));
            assert!(human.contains(remote));
            assert!(human.lines().next().unwrap().contains(language.text(
                "Health check",
                "健康检查",
                "ヘルスチェック"
            )));
            assert!(human.lines().count() <= 11);
            assert!(human.contains(locale::label(language, "browser_integration")));
            assert!(human.contains(locale::label(language, "cloud_health")));
            for hidden in ["local_ipc", "oauth_metadata", "mcp_surface", "permissions"] {
                assert!(!human.contains(locale::label(language, hidden)), "{hidden}");
            }
            assert!(
                !report
                    .render(OutputMode::Human, language, false)
                    .contains("This device")
            );
            for noise in [
                "LAYER",
                "INFO",
                "DOCTOR_JSON",
                "source=",
                "generation=",
                "/private",
            ] {
                assert!(!human.contains(noise), "{noise}");
                assert!(
                    !report
                        .render(OutputMode::Human, language, false)
                        .contains(noise)
                );
            }
            assert!(
                report
                    .render(OutputMode::Details, language, true)
                    .contains("generation=rust-abc")
            );
            let machine: Value =
                serde_json::from_str(&report.render(OutputMode::Json, language, true)).unwrap();
            assert_eq!(machine["authenticated_remote_mcp"], "not_probed");
            assert_eq!(machine["overall"], "pass");
            assert!(machine.get("details").is_none());
            assert!(machine.get("scheduler_state").is_none());
            assert!(machine.get("device").is_none());
            for field in [
                "browser",
                "local_ipc",
                "edge_reachable",
                "oauth_metadata",
                "mcp_surface",
            ] {
                assert_eq!(machine[field], "pass");
            }
            assert!(!machine.to_string().contains("/private"));
            if let Some(expected) = &baseline {
                assert_eq!(&machine, expected);
            }
            baseline = Some(machine);
        }
        assert_eq!(aggregate_check(&["pass", "fail"]), "fail");
        assert_eq!(aggregate_check(&["pass", "not_probed"]), "not_probed");
        assert_eq!(
            aggregate_check(&["unconfigured", "not_probed"]),
            "unconfigured"
        );
        let mut failed = report;
        failed.facts["overall"] = json!("fail");
        failed.facts["next_step"] = json!("repair_local");
        assert!(
            failed
                .render(OutputMode::Human, Locale::En, true)
                .contains("herdr-mcp install")
        );
    }

    #[test]
    fn normalizes_cross_platform_link_and_browser_states() {
        assert_eq!(
            link_state_from_service(&Ok(json!({"link_loaded": true}))),
            "pass"
        );
        assert_eq!(
            link_state_from_service(&Ok(json!({"link_loaded": false}))),
            "fail"
        );
        assert_eq!(
            link_state_from_service(&Err("unavailable".into())),
            "not_probed"
        );
        assert_eq!(link_state(&json!({"state": "running"})), "pass");
        assert_eq!(link_state(&json!({"state": "unexpected"})), "not_probed");
        assert_eq!(link_state(&json!({"operational_ready": true})), "pass");
        assert_eq!(link_state(&json!({})), "not_probed");

        let unsupported = Ok(json!({"implementation": "unsupported", "ok": false}));
        assert_eq!(
            browser_states(&unsupported, &SocketView::Absent),
            ("not_applicable", "not_applicable")
        );
        let configured = Ok(json!({"ok": true}));
        assert_eq!(
            browser_states(&configured, &SocketView::Absent),
            ("pass", "not_probed")
        );
        assert_eq!(
            browser_states(
                &configured,
                &SocketView::Invalid {
                    detail: "bad".into()
                }
            ),
            ("pass", "fail")
        );
    }

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
    fn remote_probe_helpers_never_echo_secrets() {
        assert_eq!(
            https_origin_for_host("herdr-edge-prod.example").as_deref(),
            Some("https://herdr-edge-prod.example")
        );
        assert_eq!(https_origin_for_host("user:pass@host"), None);
        assert_eq!(
            issuer_host("https://issuer.example/oauth").as_deref(),
            Some("issuer.example".to_owned()).as_deref()
        );
        assert_eq!(
            issuer_host("https://issuer.example/oauth?token=secret"),
            None
        );
        assert_eq!(sanitize_probe_token("herdr-edge-prod"), "herdr-edge-prod");
        assert_eq!(sanitize_probe_token("Bearer abc"), "redacted");
        assert_eq!(sanitize_probe_token("link_token=secret"), "redacted");
    }

    #[test]
    fn mcp_endpoint_accepts_unauthorized_without_sending_auth() {
        assert!(matches!(
            RemoteExpect::McpEndpoint,
            RemoteExpect::McpEndpoint
        ));
        let body = r#"{"service":"herdr-edge-prod","contractEpoch":2}"#;
        assert_eq!(
            json_string_field(body, "service").as_deref(),
            Some("herdr-edge-prod")
        );
        assert_eq!(json_u64_field(body, "contractEpoch"), Some(2));
    }

    #[test]
    fn readiness_never_promotes_unauthenticated_remote_surface_to_ready() {
        let mut remote = RemoteProbeReport::absent();
        remote.edge_state = DiagnosticState::Pass;
        remote.oauth_state = DiagnosticState::Pass;
        remote.mcp_surface_state = DiagnosticState::Pass;
        assert_eq!(
            overall_readiness(true, DiagnosticState::Pass, &remote),
            OverallReadiness::NotProven
        );
    }

    #[test]
    fn readiness_fails_on_known_authenticated_or_remote_breakage() {
        let mut remote = RemoteProbeReport::absent();
        remote.edge_state = DiagnosticState::Pass;
        remote.oauth_state = DiagnosticState::Pass;
        remote.mcp_surface_state = DiagnosticState::Pass;
        assert_eq!(
            overall_readiness(true, DiagnosticState::Fail, &remote),
            OverallReadiness::Fail
        );
        remote.oauth_state = DiagnosticState::Fail;
        assert_eq!(
            overall_readiness(true, DiagnosticState::Pass, &remote),
            OverallReadiness::Fail
        );
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

    #[test]
    fn edge_config_source_labels_candidate_and_env_paths() {
        assert_eq!(
            EdgeConfigSource::LinkCandidatePlist.as_str(),
            "link-candidate-plist"
        );
        assert_eq!(EdgeConfigSource::ProcessEnv.as_str(), "link-env");
        let edge = EdgeConfigView {
            host: "herdr-edge-device.username.workers.dev".to_owned(),
            origin: "https://herdr-edge-device.username.workers.dev".to_owned(),
            plist: None,
            source: EdgeConfigSource::ProcessEnv,
            label: None,
        };
        let formatted = format_edge_configured_layer(&Some(edge), &Config::default());
        assert!(formatted.contains("source=link-env"));
        assert!(!formatted.contains("unconfigured"));
    }

    #[test]
    fn split_edge_layer_keeps_public_origin_upstream_and_plist_evidence_distinct() {
        let edge = EdgeConfigView {
            host: "custom.example".to_owned(),
            origin: "https://custom.example".to_owned(),
            plist: Some(PathBuf::from(
                "/Users/test/Library/LaunchAgents/dev.herdr-mcp.link-prod.plist",
            )),
            source: EdgeConfigSource::ConfigToml,
            label: Some("dev.herdr-mcp.link-prod".to_owned()),
        };
        let config = Config {
            edge_public_origin: Some("https://custom.example".to_owned()),
            edge_link_upstream_origin: Some("https://backend.workers.dev".to_owned()),
            ..Config::default()
        };

        let formatted = format_edge_configured_layer(&Some(edge), &config);
        assert!(formatted.contains("origin=https://custom.example"));
        assert!(formatted.contains("upstream=https://backend.workers.dev"));
        assert!(formatted.contains("dev.herdr-mcp.link-prod.plist"));
        assert!(formatted.contains("label=dev.herdr-mcp.link-prod"));
    }

    #[test]
    fn unconfigured_edge_layer_names_missing_link_and_env() {
        let formatted = format_edge_configured_layer(&None, &Config::default());
        assert!(formatted.contains("unconfigured"));
        assert!(formatted.contains("HERDR_EDGE_URL"));
    }

    #[test]
    fn event_cache_doctor_pass_accepts_healthy_and_reconciling() {
        assert!(event_cache_doctor_pass(&EventCacheHealth::Healthy, true));
        assert!(event_cache_doctor_pass(
            &EventCacheHealth::Reconciling,
            true
        ));
        assert!(!event_cache_doctor_pass(
            &EventCacheHealth::Failed("boom".to_owned()),
            true
        ));
        assert!(!event_cache_doctor_pass(&EventCacheHealth::Healthy, false));
        assert!(!event_cache_doctor_pass(
            &EventCacheHealth::Reconciling,
            false
        ));
    }
}
