#[cfg(target_os = "macos")]
use crate::cli::ServiceCommand;
use crate::cli::WorkerCommand;
#[cfg(any(target_os = "macos", test))]
use crate::config::Config;
#[cfg(any(target_os = "macos", test))]
use crate::instance::InstanceId;
#[cfg(target_os = "macos")]
use crate::link::ownership::LINK_PROD_LABEL;
use crate::paths::RuntimePaths;
#[cfg(target_os = "macos")]
use reqwest::blocking::{Client, Response};
#[cfg(target_os = "macos")]
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
#[cfg(target_os = "macos")]
use reqwest::redirect::Policy;
use serde_json::Value;
#[cfg(any(target_os = "macos", test))]
use serde_json::json;
#[cfg(any(target_os = "macos", test))]
use std::env;
#[cfg(any(target_os = "macos", test))]
use std::fs::{self, OpenOptions};
#[cfg(target_os = "macos")]
use std::io;
#[cfg(any(target_os = "macos", test))]
use std::io::BufRead;
#[cfg(any(target_os = "macos", test))]
use std::io::Write;
#[cfg(any(target_os = "macos", test))]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(target_os = "macos")]
use std::time::Duration;
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
#[cfg(any(target_os = "macos", test))]
use url::Url;

#[cfg(any(target_os = "macos", test))]
use std::os::unix::fs::OpenOptionsExt;

#[cfg(any(target_os = "macos", test))]
const LEGACY_LINK_KEYCHAIN_SERVICE: &str = "herdr-edge-prod-link-secret";
#[cfg(target_os = "macos")]
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(target_os = "macos")]
struct FleetLinkIdentity {
    edge_origin: String,
    workstation_id: String,
    credential: String,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug)]
pub(crate) struct EnrolledCredential {
    pub(crate) device_id: String,
    pub(crate) workstation_id: String,
    pub(crate) device_secret: String,
}

#[cfg(any(target_os = "macos", test))]
fn pairing_create_request_body(ttl_seconds: u64, name: Option<&str>) -> Value {
    match name {
        Some(name) => json!({ "ttl_seconds": ttl_seconds, "name": name }),
        None => json!({ "ttl_seconds": ttl_seconds }),
    }
}

#[cfg(any(target_os = "macos", test))]
fn pairing_consume_request_body(pairing_id: &str, code: &str, name: Option<&str>) -> Value {
    match name {
        Some(name) => json!({ "pairing_id": pairing_id, "code": code, "name": name }),
        None => json!({ "pairing_id": pairing_id, "code": code }),
    }
}

#[cfg(any(target_os = "macos", test))]
fn automation_create_request_body(name: &str, device: &str) -> Value {
    json!({ "name": name, "device": device })
}

#[cfg(any(target_os = "macos", test))]
fn connector_revoke_request_body(connector_id: &str) -> Value {
    json!({ "connector_id": connector_id })
}

#[cfg(any(target_os = "macos", test))]
fn connector_client_revoke_request_body(client_id: &str) -> Value {
    json!({ "client_id": client_id })
}

fn format_pairing_expiry(expires_at_ms: u64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(expires_at_ms) * 1_000_000)
        .ok()
        .and_then(|value| value.format(&Rfc3339).ok())
}

fn inventory_now_ms() -> u64 {
    OffsetDateTime::now_utc()
        .unix_timestamp_nanos()
        .max(0)
        .saturating_div(1_000_000)
        .min(i128::from(u64::MAX)) as u64
}

fn readable_age(now_ms: u64, timestamp_ms: u64) -> String {
    if timestamp_ms > now_ms {
        return "clock skew".to_owned();
    }
    let seconds = now_ms.saturating_sub(timestamp_ms) / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 24 * 60 * 60 {
        format!("{}h", seconds / (60 * 60))
    } else if seconds < 30 * 24 * 60 * 60 {
        format!("{}d", seconds / (24 * 60 * 60))
    } else if seconds < 365 * 24 * 60 * 60 {
        format!("{}mo", seconds / (30 * 24 * 60 * 60))
    } else {
        format!("{}y", seconds / (365 * 24 * 60 * 60))
    }
}

fn readable_timestamp(timestamp_ms: u64, now_ms: u64) -> String {
    let absolute = format_pairing_expiry(timestamp_ms).unwrap_or_else(|| "invalid time".to_owned());
    let age = readable_age(now_ms, timestamp_ms);
    if age == "clock skew" {
        format!("{absolute} (clock skew)")
    } else {
        format!("{absolute} ({age} ago)")
    }
}

fn annotate_inventory_entry(
    entry: &mut Value,
    created_at_ms: Option<u64>,
    last_used_at_ms: Option<u64>,
    now_ms: u64,
    unknown_legacy_usage: bool,
) {
    let Some(object) = entry.as_object_mut() else {
        return;
    };
    object.insert(
        "age".to_owned(),
        Value::String(
            created_at_ms
                .map(|value| readable_age(now_ms, value))
                .unwrap_or_else(|| "unknown".to_owned()),
        ),
    );
    match last_used_at_ms {
        Some(value) => {
            object.insert(
                "last_used".to_owned(),
                Value::String(readable_timestamp(value, now_ms)),
            );
            object.insert("usage_state".to_owned(), Value::String("used".to_owned()));
        }
        None if unknown_legacy_usage => {
            object.insert(
                "last_used".to_owned(),
                Value::String("unknown (pre-v0.4.6)".to_owned()),
            );
            object.insert(
                "usage_state".to_owned(),
                Value::String("unknown_legacy".to_owned()),
            );
        }
        None => {
            object.insert(
                "last_used".to_owned(),
                Value::String("never used".to_owned()),
            );
            object.insert(
                "usage_state".to_owned(),
                Value::String("never_used".to_owned()),
            );
        }
    }
}

fn render_device_inventory(mut payload: Value, now_ms: u64) -> Value {
    if let Some(devices) = payload.get_mut("devices").and_then(Value::as_array_mut) {
        for device in devices {
            let created = device.get("enrolled_at_ms").and_then(Value::as_u64);
            let last_used = device.get("last_seen_at_ms").and_then(Value::as_u64);
            annotate_inventory_entry(device, created, last_used, now_ms, false);
        }
    }
    payload
}

#[cfg(any(target_os = "macos", test))]
fn render_connector_inventory(mut payload: Value, now_ms: u64) -> Value {
    if let Some(connectors) = payload.get_mut("connectors").and_then(Value::as_array_mut) {
        for connector in connectors {
            let created = connector.get("created_at_ms").and_then(Value::as_u64);
            let last_used = connector.get("last_used_at_ms").and_then(Value::as_u64);
            annotate_inventory_entry(connector, created, last_used, now_ms, false);
        }
    }
    if let Some(legacy) = payload
        .get_mut("legacy_clients")
        .and_then(Value::as_array_mut)
    {
        for client in legacy {
            let created = client.get("created_at_ms").and_then(Value::as_u64);
            let last_used = client.get("last_used_at_ms").and_then(Value::as_u64);
            annotate_inventory_entry(client, created, last_used, now_ms, true);
        }
    }
    payload
}

#[cfg(any(target_os = "macos", test))]
fn render_automation_inventory(mut payload: Value, now_ms: u64) -> Value {
    if let Some(automations) = payload.get_mut("automations").and_then(Value::as_array_mut) {
        for automation in automations {
            let created = automation.get("created_at_ms").and_then(Value::as_u64);
            let last_used = automation.get("last_used_at_ms").and_then(Value::as_u64);
            annotate_inventory_entry(automation, created, last_used, now_ms, false);
        }
    }
    payload
}

pub fn run(command: WorkerCommand) -> Result<ExitCode, String> {
    let paths = RuntimePaths::discover()?;
    if paths.instance.is_named() {
        return Err("Worker pairing is available only on the default Herdr instance".to_owned());
    }
    match command {
        WorkerCommand::List => list_devices(&paths),
        WorkerCommand::Bootstrap => crate::worker_bootstrap::run(&paths),
        WorkerCommand::Pair { ttl_seconds, name } => {
            create_pairing(&paths, ttl_seconds, name.as_deref())
        }
        WorkerCommand::Connect {
            pairing_address,
            name,
        } => {
            let name = name.or_else(crate::device_name::system_device_display_name);
            connect_existing_worker(&paths, &pairing_address, name.as_deref())
        }
        WorkerCommand::Rename { name } => rename_current_device(&paths, &name),
        WorkerCommand::Revoke { device_id } => revoke_device(&paths, &device_id),
        WorkerCommand::ConnectorApprove { request_id } => approve_connector(&paths, &request_id),
        WorkerCommand::ConnectorList => list_connectors(&paths),
        WorkerCommand::ConnectorRevoke { connector_id } => revoke_connector(&paths, &connector_id),
        WorkerCommand::ConnectorClientRevoke { client_id } => {
            revoke_connector_client(&paths, &client_id)
        }
        WorkerCommand::AutomationCreate { name, device } => {
            create_automation(&paths, &name, &device)
        }
        WorkerCommand::AutomationList => list_automations(&paths),
        WorkerCommand::AutomationRotate { client_id } => rotate_automation(&paths, &client_id),
        WorkerCommand::AutomationRevoke { client_id } => revoke_automation(&paths, &client_id),
    }
}

fn list_devices(paths: &RuntimePaths) -> Result<ExitCode, String> {
    let payload = extension_fleet_snapshot(paths)?;
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        let code = payload
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("device_inventory_unavailable");
        return Err(format!("device inventory unavailable: {code}"));
    }
    let payload = render_device_inventory(payload, inventory_now_ms());
    println!(
        "{}",
        serde_json::to_string_pretty(&payload)
            .map_err(|error| format!("cannot encode device inventory: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn extension_fleet_snapshot(_paths: &RuntimePaths) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "ok": false,
        "code": "device_inventory_platform_unsupported",
    }))
}

#[cfg(target_os = "macos")]
pub(crate) fn extension_fleet_snapshot(paths: &RuntimePaths) -> Result<Value, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let owner = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&owner.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&owner.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client()?
        .get(endpoint(&owner.edge_origin, "/devices")?)
        .headers(headers)
        .send()
        .map_err(|error| format!("cannot read Worker device inventory: {error}"))?;
    let status = response.status();
    let payload: Value = response
        .json()
        .map_err(|_| format!("Worker device inventory returned non-JSON HTTP {status}"))?;
    if !status.is_success() || payload.get("ok").and_then(Value::as_bool) != Some(true) {
        let code = payload
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("device_inventory_unavailable");
        return Ok(json!({
            "ok": false,
            "code": code,
            "http_status": status.as_u16(),
        }));
    }

    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is required to inspect the production Link".to_owned())?;
    let link = crate::link::ownership::collect_status_report(&home, &paths.config_dir);
    let alignment = link
        .get("production_runtime_alignment")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let local_device_id = config.edge_device_id.clone().or_else(|| {
        owner
            .workstation_id
            .starts_with("dev_")
            .then(|| owner.workstation_id.clone())
    });

    Ok(json!({
        "ok": true,
        "devices": payload.get("devices").cloned().unwrap_or_else(|| json!([])),
        "observed_at_ms": payload.get("observed_at_ms").cloned().unwrap_or(Value::Null),
        "local": {
            "device_id": local_device_id,
            "runtime_version": crate::runtime_meta::runtime_version(),
            "runtime_generation": alignment.get("current_generation").cloned().unwrap_or(Value::Null),
            "link_active_generation": alignment.get("active_generation").cloned().unwrap_or(Value::Null),
            "link_loaded_generation": alignment.get("loaded_launchd_generation").cloned().unwrap_or(Value::Null),
            "link_generation_stale": alignment.get("loaded_environment_stale").cloned().unwrap_or(Value::Null),
            "link_owner": link.get("production_owner").cloned().unwrap_or(Value::Null),
        },
    }))
}

#[cfg(not(target_os = "macos"))]
fn create_pairing(
    _paths: &RuntimePaths,
    _ttl_seconds: u64,
    _name: Option<&str>,
) -> Result<ExitCode, String> {
    Err("worker pair currently requires macOS Keychain; refusing to create a pairing on this platform"
        .to_owned())
}

#[cfg(target_os = "macos")]
fn create_pairing(
    paths: &RuntimePaths,
    ttl_seconds: u64,
    name: Option<&str>,
) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let owner = resolve_fleet_link_identity(paths, &config)?;
    let endpoint = endpoint(&owner.edge_origin, "/devices/pairings")?;
    let mut headers = bearer_headers(&owner.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&owner.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client()?
        .post(endpoint)
        .headers(headers)
        .json(&pairing_create_request_body(ttl_seconds, name))
        .send()
        .map_err(|error| format!("cannot create device pairing: {error}"))?;
    let payload = parse_json_response(response, "device pairing creation")?;
    let pairing_id = payload
        .get("pairing_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "device pairing creation returned no pairing id".to_owned())?;
    validate_pairing_id(pairing_id)?;
    let code = payload
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| "device pairing creation returned no code".to_owned())?;
    validate_pairing_code(code)?;
    let expires_at_ms = payload
        .get("expires_at_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| "device pairing creation returned no expiry".to_owned())?;

    let pairing_address = format!("{}/pair#{}", owner.edge_origin, pairing_id);
    println!("Pairing created for Worker {}", owner.edge_origin);
    println!();
    println!("Pairing address: {}", pairing_address);
    println!("Verification code: {}", format_pairing_code(code));
    if let Some(expires_at) = format_pairing_expiry(expires_at_ms) {
        println!("Expires at: {expires_at} (UTC)");
    }
    println!("Valid for at most {ttl_seconds} seconds; use it immediately.");
    println!();
    println!("On the new computer, run:");
    println!("  herdr-mcp worker connect \"{}\"", pairing_address);
    println!("and enter the verification code when prompted.");
    println!();
    println!("Agent prompt (copy to the new computer's Coding Agent):");
    println!(
        "Read and follow https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/en/existing-worker-connect.md to connect this computer to my existing Herdr Worker. Pairing address: {}  Then enter the separately displayed 6-digit verification code at the visible CLI prompt (the code is never part of the copyable command).",
        pairing_address
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(target_os = "macos"))]
fn connect_existing_worker(
    _paths: &RuntimePaths,
    _pairing_address: &str,
    _name: Option<&str>,
) -> Result<ExitCode, String> {
    Err(
        "worker connect currently requires macOS Keychain; refusing to pair on this platform"
            .to_owned(),
    )
}

#[cfg(target_os = "macos")]
fn connect_existing_worker(
    paths: &RuntimePaths,
    pairing_address: &str,
    name: Option<&str>,
) -> Result<ExitCode, String> {
    let (edge_origin, pairing_id) = parse_pairing_address(pairing_address)?;
    let code = read_pairing_code_tty()?;
    connect_macos_inner(
        paths,
        &edge_origin,
        &pairing_id,
        &code,
        name,
        crate::macos_credential_helper::store,
        Config::load_for_instance,
        write_config_atomic,
        revoke_self,
        crate::macos_credential_helper::delete,
        activate_connected_runtime,
        crate::link::reconcile_after_service_generation_change,
        consume_pairing,
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn adopt_bootstrap_enrollment(
    paths: &RuntimePaths,
    edge_origin: &str,
    enrolled: EnrolledCredential,
) -> Result<ExitCode, String> {
    connect_macos_inner(
        paths,
        edge_origin,
        "bootstrap-enrollment",
        "000000",
        None,
        crate::macos_credential_helper::store,
        Config::load_for_instance,
        write_config_atomic,
        revoke_self,
        crate::macos_credential_helper::delete,
        activate_connected_runtime,
        crate::link::reconcile_after_service_generation_change,
        move |_, _, _, _| Ok(enrolled.clone()),
    )
}

#[cfg(not(target_os = "macos"))]
fn revoke_device(_paths: &RuntimePaths, _device_id: &str) -> Result<ExitCode, String> {
    Err(
        "worker revoke currently requires macOS Keychain; refusing to revoke on this platform"
            .to_owned(),
    )
}

#[cfg(target_os = "macos")]
fn revoke_device(paths: &RuntimePaths, device_id: &str) -> Result<ExitCode, String> {
    let device_id = crate::config::normalize_device_id(device_id)?;
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client()?
        .post(endpoint(&identity.edge_origin, "/devices/revoke")?)
        .headers(headers)
        .json(&json!({ "device_id": device_id }))
        .send()
        .map_err(|error| format!("cannot revoke Worker device: {error}"))?;
    let payload = parse_json_response(response, "device revoke")?;
    let revoked_device_id = required_string(&payload, "device_id")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "worker_revoke",
            "device_id": revoked_device_id,
            "revoked_at_ms": payload.get("revoked_at_ms").cloned().unwrap_or(Value::Null),
        }))
        .map_err(|error| format!("cannot encode device revoke result: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(target_os = "macos"))]
fn approve_connector(_paths: &RuntimePaths, _request_id: &str) -> Result<ExitCode, String> {
    Err(
        "connector approval currently requires the macOS enrolled-device credential backend"
            .to_owned(),
    )
}

#[cfg(target_os = "macos")]
fn approve_connector(paths: &RuntimePaths, request_id: &str) -> Result<ExitCode, String> {
    if request_id.trim().is_empty() || request_id.len() > 256 {
        return Err("connector approval request id is invalid".to_owned());
    }
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let inspect = client()?
        .post(endpoint(&identity.edge_origin, "/connectors/inspect")?)
        .headers(headers.clone())
        .json(&json!({ "request_id": request_id }))
        .send()
        .map_err(|error| format!("cannot inspect Connector approval: {error}"))?;
    let details = parse_json_response(inspect, "connector approval inspection")?;
    eprintln!("Connector approval request:");
    eprintln!(
        "  client: {} ({})",
        details
            .get("client_name")
            .and_then(Value::as_str)
            .unwrap_or("unnamed client"),
        details
            .get("client_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    eprintln!(
        "  redirect: {}",
        details
            .get("redirect_uri")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    eprintln!(
        "  resource/scope: {} / {}",
        details
            .get("resource")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        details
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    if let Some(expires_at_ms) = details.get("expires_at_ms").and_then(Value::as_u64)
        && let Some(expires_at) = format_pairing_expiry(expires_at_ms)
    {
        eprintln!("  expires: {expires_at}");
    }
    let code = read_pairing_code_tty()?;
    let response = client()?
        .post(endpoint(&identity.edge_origin, "/connectors/approve")?)
        .headers(headers)
        .json(&json!({ "request_id": request_id, "code": code }))
        .send()
        .map_err(|error| format!("cannot approve Connector: {error}"))?;
    let payload = parse_json_response(response, "connector approval")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "connector_approve",
            "client_id": payload.get("client_id").cloned().unwrap_or(Value::Null),
            "approved_at_ms": payload.get("approved_at_ms").cloned().unwrap_or(Value::Null),
        }))
        .map_err(|error| format!("cannot encode connector approval result: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(target_os = "macos"))]
fn revoke_connector(_paths: &RuntimePaths, _connector_id: &str) -> Result<ExitCode, String> {
    Err(
        "connector revoke currently requires the macOS enrolled-device credential backend"
            .to_owned(),
    )
}

#[cfg(target_os = "macos")]
fn revoke_connector(paths: &RuntimePaths, connector_id: &str) -> Result<ExitCode, String> {
    let connector_id = connector_id.trim();
    if !connector_id.starts_with("conn_") || connector_id.len() > 4096 {
        return Err("connector id must be a valid conn_ identifier".to_owned());
    }
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client()?
        .post(endpoint(&identity.edge_origin, "/connectors/revoke")?)
        .headers(headers)
        .json(&connector_revoke_request_body(connector_id))
        .send()
        .map_err(|error| format!("cannot revoke Connector: {error}"))?;
    let payload = parse_json_response(response, "connector revoke")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "connector_revoke",
            "connector_id": payload.get("connector_id").cloned().unwrap_or(Value::String(connector_id.to_owned())),
        }))
        .map_err(|error| format!("cannot encode connector revoke result: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(target_os = "macos"))]
fn revoke_connector_client(_paths: &RuntimePaths, _client_id: &str) -> Result<ExitCode, String> {
    Err(
        "connector client revoke currently requires the macOS enrolled-device credential backend"
            .to_owned(),
    )
}

#[cfg(target_os = "macos")]
fn revoke_connector_client(paths: &RuntimePaths, client_id: &str) -> Result<ExitCode, String> {
    let client_id = client_id.trim();
    if client_id.is_empty() || client_id.len() > 4096 {
        return Err("connector client id must be non-empty and at most 4096 bytes".to_owned());
    }
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client()?
        .post(endpoint(&identity.edge_origin, "/connectors/revoke")?)
        .headers(headers)
        .json(&connector_client_revoke_request_body(client_id))
        .send()
        .map_err(|error| format!("cannot revoke Connector client: {error}"))?;
    let payload = parse_json_response(response, "connector client revoke")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "connector_client_revoke",
            "client_id": payload.get("client_id").cloned().unwrap_or(Value::String(client_id.to_owned())),
        }))
        .map_err(|error| format!("cannot encode connector client revoke result: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(target_os = "macos"))]
fn list_connectors(_paths: &RuntimePaths) -> Result<ExitCode, String> {
    Err(
        "connector inventory currently requires the macOS enrolled-device credential backend"
            .to_owned(),
    )
}

#[cfg(target_os = "macos")]
fn list_connectors(paths: &RuntimePaths) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client()?
        .get(endpoint(&identity.edge_origin, "/connectors")?)
        .headers(headers)
        .send()
        .map_err(|error| format!("cannot list Connectors: {error}"))?;
    let payload = render_connector_inventory(
        parse_json_response(response, "connector inventory")?,
        inventory_now_ms(),
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&payload)
            .map_err(|error| format!("cannot encode connector inventory: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(target_os = "macos"))]
fn create_automation(
    _paths: &RuntimePaths,
    _name: &str,
    _device: &str,
) -> Result<ExitCode, String> {
    Err("automation credential provisioning currently requires the macOS enrolled-device credential backend".to_owned())
}

#[cfg(target_os = "macos")]
fn create_automation(paths: &RuntimePaths, name: &str, device: &str) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client()?
        .post(endpoint(&identity.edge_origin, "/automations")?)
        .headers(headers)
        .json(&automation_create_request_body(name, device))
        .send()
        .map_err(|error| format!("cannot create automation credential: {error}"))?;
    let payload = parse_json_response(response, "automation credential creation")?;
    let client_id = required_string(&payload, "client_id")?;
    let client_secret = required_string(&payload, "client_secret")?;
    let token_endpoint = required_string(&payload, "token_endpoint")?;
    let mcp_url = endpoint(&identity.edge_origin, "/mcp")?.to_string();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "automation_create",
            "name": name,
            "device": device,
            "device_id": payload.get("device_id").cloned().unwrap_or(Value::Null),
            "client_id": client_id,
            "client_secret": client_secret,
            "token_endpoint": token_endpoint,
            "mcp_url": mcp_url,
            "scope": "mcp",
            "gitlab_variables": {
                "HERDR_MCP_URL": mcp_url,
                "HERDR_MCP_CLIENT_ID": client_id,
                "HERDR_MCP_CLIENT_SECRET": client_secret,
            },
            "secret_display": "one_time",
        }))
        .map_err(|error| format!("cannot encode automation credential result: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(target_os = "macos"))]
fn list_automations(_paths: &RuntimePaths) -> Result<ExitCode, String> {
    Err("automation credential inventory currently requires the macOS enrolled-device credential backend".to_owned())
}

#[cfg(target_os = "macos")]
fn list_automations(paths: &RuntimePaths) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client()?
        .get(endpoint(&identity.edge_origin, "/automations")?)
        .headers(headers)
        .send()
        .map_err(|error| format!("cannot list automation credentials: {error}"))?;
    let payload = render_automation_inventory(
        parse_json_response(response, "automation credential inventory")?,
        inventory_now_ms(),
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&payload)
            .map_err(|error| format!("cannot encode automation credential inventory: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(target_os = "macos"))]
fn rotate_automation(_paths: &RuntimePaths, _client_id: &str) -> Result<ExitCode, String> {
    Err("automation credential rotation currently requires the macOS enrolled-device credential backend".to_owned())
}

#[cfg(target_os = "macos")]
fn rotate_automation(paths: &RuntimePaths, client_id: &str) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client()?
        .post(endpoint(&identity.edge_origin, "/automations/rotate")?)
        .headers(headers)
        .json(&json!({ "client_id": client_id }))
        .send()
        .map_err(|error| format!("cannot rotate automation credential: {error}"))?;
    let payload = parse_json_response(response, "automation credential rotation")?;
    let secret = required_string(&payload, "client_secret")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "automation_rotate",
            "client_id": client_id,
            "client_secret": secret,
            "gitlab_variable": { "HERDR_MCP_CLIENT_SECRET": secret },
            "secret_display": "one_time",
        }))
        .map_err(|error| format!("cannot encode automation rotation result: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(target_os = "macos"))]
fn revoke_automation(_paths: &RuntimePaths, _client_id: &str) -> Result<ExitCode, String> {
    Err("automation credential revoke currently requires the macOS enrolled-device credential backend".to_owned())
}

#[cfg(target_os = "macos")]
fn revoke_automation(paths: &RuntimePaths, client_id: &str) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client()?
        .post(endpoint(&identity.edge_origin, "/automations/revoke")?)
        .headers(headers)
        .json(&json!({ "client_id": client_id }))
        .send()
        .map_err(|error| format!("cannot revoke automation credential: {error}"))?;
    let payload = parse_json_response(response, "automation credential revoke")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "automation_revoke",
            "client_id": payload.get("client_id").cloned().unwrap_or(Value::String(client_id.to_owned())),
        }))
        .map_err(|error| format!("cannot encode automation revoke result: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(target_os = "macos"))]
fn rename_current_device(_paths: &RuntimePaths, _name: &str) -> Result<ExitCode, String> {
    Err(
        "worker rename currently requires macOS Keychain; refusing to rename on this platform"
            .to_owned(),
    )
}

#[cfg(target_os = "macos")]
fn rename_current_device(paths: &RuntimePaths, name: &str) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client()?
        .post(endpoint(&identity.edge_origin, "/devices/rename-self")?)
        .headers(headers)
        .json(&json!({
            "workstation_id": identity.workstation_id,
            "name": name,
        }))
        .send()
        .map_err(|error| format!("cannot rename current device: {error}"))?;
    let payload = parse_json_response(response, "device rename")?;
    let device_id = required_string(&payload, "device_id")?;
    let renamed = required_string(&payload, "name")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "worker_rename",
            "device_id": device_id,
            "name": renamed,
        }))
        .map_err(|error| format!("cannot encode device rename result: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(target_os = "macos")]
fn activate_connected_runtime(paths: &RuntimePaths) -> Result<(), String> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is required to activate the connected runtime".to_owned())?;
    let prod_plist = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LINK_PROD_LABEL}.plist"));
    let prod_plist_existed_before = prod_plist.exists();

    match activate_connected_runtime_inner(paths, &home) {
        Ok(()) => Ok(()),
        Err(error) if !prod_plist_existed_before => {
            match crate::link::remove_fresh_owned_prod_link_after_failed_activation(&home) {
                Ok(_) => Err(error),
                Err(cleanup_error) => Err(format!(
                    "{error}; failed to clean up the fresh production Link after activation failure: {cleanup_error}"
                )),
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn activate_connected_runtime_inner(paths: &RuntimePaths, home: &Path) -> Result<(), String> {
    let code = crate::service_lifecycle::run(ServiceCommand::Install { adopt_node: false })?;
    if code != ExitCode::SUCCESS {
        return Err(format!(
            "service install returned a non-success exit code: {code:?}"
        ));
    }

    let service = crate::service_manager::doctor_status()?;
    if service.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("herdr-mcp service is not healthy after worker connect activation".to_owned());
    }

    let link = crate::link::ownership::collect_status_report(home, &paths.config_dir);
    let prod = link
        .get("agents")
        .and_then(Value::as_array)
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent.get("label").and_then(Value::as_str) == Some(LINK_PROD_LABEL))
        })
        .ok_or_else(|| "production Link status is missing link-prod evidence".to_owned())?;
    if prod.get("loaded").and_then(Value::as_bool) != Some(true)
        || prod.get("implementation").and_then(Value::as_str) != Some("rust")
        || prod
            .get("points_at_managed_runtime")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err(
            "production Link is not loaded as an owned Rust managed-runtime process after worker connect activation"
                .to_owned(),
        );
    }

    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let expected_device = config
        .edge_device_id
        .as_deref()
        .ok_or_else(|| "worker connect config is missing device_id after activation".to_owned())?;
    if prod.get("workstation_id").and_then(Value::as_str) != Some(expected_device) {
        return Err(
            "production Link identity does not match the newly paired device after activation"
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn compensate_after_store(
    edge_origin: &str,
    device_id: &str,
    keychain_service: &str,
    account: &str,
    device_secret: &str,
    revoke: &dyn Fn(&str, &str, &str) -> Result<bool, String>,
    delete: &dyn Fn(&str, &str) -> Result<(), String>,
) -> (bool, bool) {
    let revoked = revoke(edge_origin, device_id, device_secret).unwrap_or(false);
    let deleted = delete(keychain_service, account).is_ok();
    (revoked, deleted)
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct ReconcileRollbackEvidence {
    revoked: bool,
    keychain_deleted: bool,
    config_restored: bool,
    link_reconciled: bool,
    restore_error: Option<String>,
    reconcile_error: Option<String>,
}

#[cfg(any(target_os = "macos", test))]
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn rollback_after_reconcile_failure<H, I, J, K>(
    edge_origin: &str,
    device_id: &str,
    keychain_service: &str,
    account: &str,
    device_secret: &str,
    paths: &RuntimePaths,
    previous_config: &Config,
    write_config: &H,
    revoke_fn: &I,
    delete_secret: &J,
    reconcile: &K,
) -> ReconcileRollbackEvidence
where
    H: Fn(&RuntimePaths, &Config) -> Result<(), String>,
    I: Fn(&str, &str, &str) -> Result<bool, String>,
    J: Fn(&str, &str) -> Result<(), String>,
    K: Fn(&RuntimePaths) -> Result<(), String>,
{
    let (revoked, keychain_deleted) = compensate_after_store(
        edge_origin,
        device_id,
        keychain_service,
        account,
        device_secret,
        revoke_fn,
        delete_secret,
    );
    let (config_restored, restore_error) = match write_config(paths, previous_config) {
        Ok(()) => (true, None),
        Err(error) => (false, Some(error)),
    };
    let (link_reconciled, reconcile_error) = if config_restored {
        match reconcile(paths) {
            Ok(()) => (true, None),
            Err(error) => (false, Some(error)),
        }
    } else {
        (false, None)
    };
    ReconcileRollbackEvidence {
        revoked,
        keychain_deleted,
        config_restored,
        link_reconciled,
        restore_error,
        reconcile_error,
    }
}

#[cfg(any(target_os = "macos", test))]
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn connect_macos_inner<F, G, H, I, J, K, L, M>(
    paths: &RuntimePaths,
    edge_origin: &str,
    pairing_id: &str,
    code: &str,
    name: Option<&str>,
    store_secret: F,
    load_config: G,
    write_config: H,
    revoke_fn: I,
    delete_secret: J,
    activate: K,
    reconcile_restore: L,
    consume: M,
) -> Result<ExitCode, String>
where
    F: Fn(&str, &str, &str) -> Result<(), String>,
    G: Fn(&Path, &InstanceId) -> Result<Config, String>,
    H: Fn(&RuntimePaths, &Config) -> Result<(), String>,
    I: Fn(&str, &str, &str) -> Result<bool, String>,
    J: Fn(&str, &str) -> Result<(), String>,
    K: Fn(&RuntimePaths) -> Result<(), String>,
    L: Fn(&RuntimePaths) -> Result<(), String>,
    M: Fn(&str, &str, &str, Option<&str>) -> Result<EnrolledCredential, String>,
{
    let enrolled = consume(edge_origin, pairing_id, code, name)?;
    let device_id = crate::config::normalize_device_id(&enrolled.device_id)?;
    if enrolled.workstation_id != device_id {
        let _ = revoke_fn(
            edge_origin,
            &enrolled.workstation_id,
            &enrolled.device_secret,
        );
        return Err(
            "Worker returned a workstation identity that does not match the immutable device_id"
                .to_owned(),
        );
    }
    if let Err(error) = validate_device_secret(&enrolled.device_secret) {
        let _ = revoke_fn(edge_origin, &device_id, &enrolled.device_secret);
        return Err(error);
    }

    let account = match current_account() {
        Ok(a) => a,
        Err(error) => {
            let _ = revoke_fn(edge_origin, &device_id, &enrolled.device_secret);
            return Err(error);
        }
    };
    let keychain_service = format!("herdr-edge-link-{device_id}");
    if let Err(error) = store_secret(&keychain_service, &account, &enrolled.device_secret) {
        let revoked = revoke_fn(edge_origin, &device_id, &enrolled.device_secret).unwrap_or(false);
        return Err(format!(
            "cannot persist the new device credential; remote compensation revoked={revoked}: {error}"
        ));
    }

    // Any failure after the secret is durably stored must revoke the remote device
    // and delete the local Keychain credential to avoid orphans. No secret is ever
    // printed in the error.
    // Retain the previous local binding so a post-write failure can roll the
    // transaction back instead of leaving config/plist bound to a revoked device.
    let previous_config = match load_config(&paths.config_file, &paths.instance) {
        Ok(c) => c,
        Err(error) => {
            let (revoked, deleted) = compensate_after_store(
                edge_origin,
                &device_id,
                &keychain_service,
                &account,
                &enrolled.device_secret,
                &revoke_fn,
                &delete_secret,
            );
            return Err(format!(
                "config load failed for {device_id}: {error}; compensation revoked={revoked} keychain_deleted={deleted}"
            ));
        }
    };
    let mut config = previous_config.clone();
    if let Err(error) = config.set_edge_public_origin(edge_origin) {
        let (revoked, deleted) = compensate_after_store(
            edge_origin,
            &device_id,
            &keychain_service,
            &account,
            &enrolled.device_secret,
            &revoke_fn,
            &delete_secret,
        );
        return Err(format!(
            "config set origin failed for {device_id}: {error}; compensation revoked={revoked} keychain_deleted={deleted}"
        ));
    }
    if let Err(error) = config.set_edge_device_id(&device_id) {
        let (revoked, deleted) = compensate_after_store(
            edge_origin,
            &device_id,
            &keychain_service,
            &account,
            &enrolled.device_secret,
            &revoke_fn,
            &delete_secret,
        );
        return Err(format!(
            "config set device failed for {device_id}: {error}; compensation revoked={revoked} keychain_deleted={deleted}"
        ));
    }
    if let Err(error) = write_config(paths, &config) {
        let (revoked, deleted) = compensate_after_store(
            edge_origin,
            &device_id,
            &keychain_service,
            &account,
            &enrolled.device_secret,
            &revoke_fn,
            &delete_secret,
        );
        return Err(format!(
            "config update failed for {device_id}: {error}; compensation revoked={revoked} keychain_deleted={deleted}"
        ));
    }

    if let Err(error) = activate(paths) {
        // The config is durably written, but the local runtime/production Link
        // could not be made ready. Roll the whole local transaction back: exact remote
        // revoke-self, local Keychain deletion, best-effort atomic restore of the
        // previous config, and best-effort reconcile of the previous Link identity from
        // that config. Rollback failures are reported, never hidden, and no secret
        // is ever printed. The pairing is already consumed server-side, so it is
        // never reusable.
        let evidence = rollback_after_reconcile_failure(
            edge_origin,
            &device_id,
            &keychain_service,
            &account,
            &enrolled.device_secret,
            paths,
            &previous_config,
            &write_config,
            &revoke_fn,
            &delete_secret,
            &reconcile_restore,
        );
        return Err(format!(
            "device {device_id} is paired but the local runtime could not be activated: {error}; compensation revoked={} keychain_deleted={} config_restored={} link_reconciled={} restore_error={} reconcile_error={}",
            evidence.revoked,
            evidence.keychain_deleted,
            evidence.config_restored,
            evidence.link_reconciled,
            evidence.restore_error.as_deref().unwrap_or("none"),
            evidence.reconcile_error.as_deref().unwrap_or("none"),
        ));
    }

    print_json(&json!({
        "ok": true,
        "action": "worker_connect",
        "device_id": device_id,
        "workstation_id": enrolled.workstation_id,
        "edge_origin": edge_origin,
        "keychain_service": keychain_service,
        "pairing_consumed": true,
        "secret_printed": false,
        "service_ready": true,
        "link_ready": true,
        "link_reconciled": true,
    }))?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(target_os = "macos")]
fn consume_pairing(
    edge_origin: &str,
    pairing_id: &str,
    code: &str,
    name: Option<&str>,
) -> Result<EnrolledCredential, String> {
    let response = client()?
        .post(endpoint(edge_origin, "/devices/pairings/consume")?)
        .header(CONTENT_TYPE, "application/json")
        .json(&pairing_consume_request_body(pairing_id, code, name))
        .send()
        .map_err(|error| format!("cannot consume device pairing: {error}"))?;
    let payload = parse_json_response(response, "device pairing consumption")?;
    Ok(EnrolledCredential {
        device_id: required_string(&payload, "device_id")?,
        workstation_id: required_string(&payload, "workstation_id")?,
        device_secret: required_string(&payload, "device_secret")?,
    })
}

#[cfg(target_os = "macos")]
fn revoke_self(edge_origin: &str, workstation_id: &str, credential: &str) -> Result<bool, String> {
    let response = client()?
        .post(endpoint(edge_origin, "/devices/revoke-self")?)
        .headers(bearer_headers(credential)?)
        .json(&json!({ "workstation_id": workstation_id }))
        .send()
        .map_err(|error| format!("cannot compensate failed device pairing: {error}"))?;
    Ok(response.status().is_success())
}

#[cfg(target_os = "macos")]
fn resolve_fleet_link_identity(
    paths: &RuntimePaths,
    config: &Config,
) -> Result<FleetLinkIdentity, String> {
    // A current enrolled device has both the canonical device id and Edge
    // origin in config, so it must not depend on a LaunchAgent plist merely to
    // create another short-lived pairing. Older installs may still need the
    // production Link environment as a compatibility fallback.
    let needs_legacy_link_fallback =
        config.edge_device_id.is_none() || config.edge_public_origin.is_none();
    let plist_env = if needs_legacy_link_fallback {
        production_link_environment_if_present()?
    } else {
        None
    };
    let (workstation_id, keychain_service, edge_origin) =
        resolve_fleet_link_fields(config, plist_env.as_ref())?;
    let account = current_account()?;
    let credential =
        crate::macos_credential_helper::load(&keychain_service, &account).map_err(|error| {
            enrolled_device_required_error(&format!(
                "the enrolled device credential is unavailable: {error}"
            ))
        })?;
    let _ = paths;
    Ok(FleetLinkIdentity {
        edge_origin,
        workstation_id,
        credential,
    })
}

#[cfg(target_os = "macos")]
fn production_link_environment_if_present()
-> Result<Option<std::collections::BTreeMap<String, String>>, String> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is required to locate the production Link plist".to_owned())?;
    let path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LINK_PROD_LABEL}.plist"));
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot inspect production Link plist {}: {error}",
                path.display()
            ));
        }
    }
    let root = plist::Value::from_file(&path).map_err(|error| {
        format!(
            "cannot read production Link plist {}: {error}",
            path.display()
        )
    })?;
    let env_dict = root
        .as_dictionary()
        .and_then(|dict| dict.get("EnvironmentVariables"))
        .and_then(plist::Value::as_dictionary)
        .ok_or_else(|| "production Link plist has no EnvironmentVariables".to_owned())?;
    Ok(Some(
        env_dict
            .iter()
            .filter_map(|(key, value)| {
                value
                    .as_string()
                    .map(|value| (key.clone(), value.to_owned()))
            })
            .collect(),
    ))
}

#[cfg(any(target_os = "macos", test))]
fn enrolled_device_required_error(reason: &str) -> String {
    format!(
        "This fleet operation requires credentials for a device already enrolled in the target Herdr Worker; {reason}. \
Herdr devices enrolled in the same Worker have no owner/member hierarchy for fleet administration. \
On a new computer joining an existing fleet, use `herdr-mcp worker connect <pairing-address>` with a pairing created by any enrolled device or an explicitly approved WebChat. \
If this is the first Herdr device, complete the first-Worker Cloudflare bootstrap before using pairing or Connector administration."
    )
}

#[cfg(any(target_os = "macos", test))]
fn resolve_fleet_link_fields(
    config: &Config,
    plist_env: Option<&std::collections::BTreeMap<String, String>>,
) -> Result<(String, String, String), String> {
    let workstation_id = config
        .edge_device_id
        .clone()
        .or_else(|| plist_env.and_then(|env| env.get("HERDR_WORKSTATION_ID").cloned()))
        .ok_or_else(|| {
            enrolled_device_required_error("no enrolled device identity is present on this machine")
        })?;
    let keychain_service = config
        .edge_link_keychain_service()
        .or_else(|| plist_env.and_then(|env| env.get("HERDR_LINK_KEYCHAIN_SERVICE").cloned()))
        .unwrap_or_else(|| LEGACY_LINK_KEYCHAIN_SERVICE.to_owned());
    let edge_origin = match config.edge_public_origin.clone() {
        Some(origin) => normalize_edge_origin(&origin)?,
        None => {
            let edge_url = plist_env
                .and_then(|env| env.get("HERDR_EDGE_URL"))
                .ok_or_else(|| {
                    enrolled_device_required_error(
                        "no existing fleet Cloudflare/Edge origin is present on this machine",
                    )
                })?;
            origin_from_ws_url(edge_url)?
        }
    };
    Ok((workstation_id, keychain_service, edge_origin))
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn write_config_atomic(paths: &RuntimePaths, config: &Config) -> Result<(), String> {
    fs::create_dir_all(&paths.config_dir).map_err(|error| {
        format!(
            "cannot create config directory {}: {error}",
            paths.config_dir.display()
        )
    })?;
    let temp = paths
        .config_file
        .with_extension(format!("toml.tmp-{}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temp)
        .map_err(|error| format!("cannot create temporary config {}: {error}", temp.display()))?;
    file.write_all(config.render().as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            format!(
                "cannot persist temporary config {}: {error}",
                temp.display()
            )
        })?;
    fs::rename(&temp, &paths.config_file).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!(
            "cannot replace config {}: {error}",
            paths.config_file.display()
        )
    })
}

#[cfg(target_os = "macos")]
fn client() -> Result<Client, String> {
    Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(Policy::none())
        .build()
        .map_err(|error| format!("cannot initialize Worker HTTP client: {error}"))
}

#[cfg(target_os = "macos")]
fn bearer_headers(credential: &str) -> Result<HeaderMap, String> {
    if credential.is_empty() || credential.len() > 4096 || credential.chars().any(char::is_control)
    {
        return Err("workstation credential is invalid".to_owned());
    }
    let mut headers = HeaderMap::new();
    let value = HeaderValue::from_str(&format!("Bearer {credential}"))
        .map_err(|_| "workstation credential cannot be encoded as an HTTP header".to_owned())?;
    headers.insert(AUTHORIZATION, value);
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(headers)
}

#[cfg(target_os = "macos")]
fn parse_json_response(response: Response, operation: &str) -> Result<Value, String> {
    let status = response.status();
    let payload: Value = response
        .json()
        .map_err(|_| format!("{operation} returned non-JSON HTTP {status}"))?;
    if !status.is_success() || payload.get("ok").and_then(Value::as_bool) != Some(true) {
        let code = payload
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("remote_error");
        return Err(format!("{operation} failed with HTTP {status} code={code}"));
    }
    Ok(payload)
}

#[cfg(target_os = "macos")]
fn endpoint(origin: &str, path: &str) -> Result<Url, String> {
    let mut url = Url::parse(&normalize_edge_origin(origin)?)
        .map_err(|error| format!("invalid Worker origin: {error}"))?;
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn normalize_edge_origin(value: &str) -> Result<String, String> {
    let mut config = Config::default();
    config.set_edge_public_origin(value)?;
    config
        .edge_public_origin
        .ok_or_else(|| "Worker origin is missing".to_owned())
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn origin_from_ws_url(value: &str) -> Result<String, String> {
    let mut url =
        Url::parse(value).map_err(|error| format!("invalid production Link URL: {error}"))?;
    let scheme = match url.scheme() {
        "wss" => "https",
        "ws" => "http",
        other => return Err(format!("unsupported production Link scheme {other}")),
    };
    url.set_scheme(scheme)
        .map_err(|_| "cannot normalize production Link origin".to_owned())?;
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    normalize_edge_origin(url.as_str())
}

#[cfg(target_os = "macos")]
fn required_string(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(|| format!("Worker response is missing {key}"))
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn validate_pairing_id(value: &str) -> Result<(), String> {
    if !value.starts_with("pair_") {
        return Err("pairing id must be pair_ followed by 64 lowercase hex characters".to_owned());
    }
    let hex = &value[5..];
    if hex.len() != 64 || !hex.chars().all(|ch| matches!(ch, '0'..='9' | 'a'..='f')) {
        return Err("pairing id must be pair_ followed by 64 lowercase hex characters".to_owned());
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn validate_pairing_code(value: &str) -> Result<(), String> {
    if value.len() == 6 && value.chars().all(|ch| ch.is_ascii_digit()) {
        Ok(())
    } else {
        Err("pairing code must be exactly six decimal digits".to_owned())
    }
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn format_pairing_code(code: &str) -> String {
    if code.len() == 6 {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_owned()
    }
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_pairing_address(value: &str) -> Result<(String, String), String> {
    let url = Url::parse(value).map_err(|_| "pairing address must be a valid URL".to_owned())?;
    if url.scheme() != "https" {
        return Err("pairing address must use https://".to_owned());
    }
    if url.host_str().is_none() {
        return Err("pairing address must include a host".to_owned());
    }
    if url.path() != "/pair" {
        return Err("pairing address must point at the /pair path".to_owned());
    }
    if url.query().is_some() {
        return Err("pairing address must not include a query string".to_owned());
    }
    let pairing_id = url.fragment().ok_or_else(|| {
        "pairing address must include a pairing id in the URL fragment".to_owned()
    })?;
    validate_pairing_id(pairing_id)?;
    let mut origin_url = url.clone();
    origin_url.set_path("");
    origin_url.set_query(None);
    origin_url.set_fragment(None);
    let origin = normalize_edge_origin(origin_url.as_str())?;
    Ok((origin, pairing_id.to_owned()))
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn read_pairing_code_from<R: BufRead>(reader: &mut R) -> Result<String, String> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| format!("cannot read pairing code: {error}"))?;
    let code = line.trim();
    validate_pairing_code(code)?;
    Ok(code.to_owned())
}

#[cfg(target_os = "macos")]
fn read_pairing_code_tty() -> Result<String, String> {
    use std::io::IsTerminal;
    let stdin = io::stdin();
    if stdin.is_terminal() {
        // The six-digit value is a short-lived verification code, not a
        // password. Keep it out of argv/shell history, but let users see what
        // they type so transcription mistakes are obvious.
        eprint!("Enter 6-digit verification code: ");
        let _ = io::stderr().flush();
    }
    let mut reader = io::BufReader::new(stdin.lock());
    read_pairing_code_from(&mut reader)
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn validate_device_secret(value: &str) -> Result<(), String> {
    let suffix = value
        .strip_prefix("devsec_")
        .ok_or_else(|| "Worker returned an invalid device credential".to_owned())?;
    if suffix.len() != 64 || !suffix.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("Worker returned an invalid device credential".to_owned());
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn current_account() -> Result<String, String> {
    env::var("USER")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| {
            !value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| "USER is required for macOS Keychain device credentials".to_owned())
}

#[cfg(test)]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn print_json(value: &Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|error| format!("cannot encode worker result: {error}"))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_validators_are_strict_and_never_accept_argv_shaped_garbage() {
        assert!(validate_pairing_code("000000").is_ok());
        assert!(validate_pairing_code("123456").is_ok());
        assert!(validate_pairing_code("12345").is_err());
        assert!(validate_pairing_code("12345a").is_err());
        assert!(validate_pairing_code("1234567").is_err());
        assert!(
            validate_pairing_id(
                "pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .is_ok()
        );
        assert!(validate_pairing_id("").is_err());
        assert!(validate_pairing_id("pair with space").is_err());
        assert!(
            validate_pairing_id(
                "pair_ABCDEFaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .is_err()
        );
        assert!(
            validate_pairing_id(
                "pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .is_err()
        );
        assert!(
            validate_pairing_id(
                "pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .is_err()
        );
        assert!(validate_device_secret(&format!("devsec_{}", "b".repeat(64))).is_ok());
        assert!(validate_device_secret("devsec_bad").is_err());
    }

    #[test]
    fn ws_origin_conversion_keeps_only_https_origin() {
        assert_eq!(
            origin_from_ws_url("wss://herdr.example.com/ws?ignored=1").unwrap(),
            "https://herdr.example.com"
        );
    }

    #[test]
    fn fresh_machine_cannot_be_mistaken_for_enrolled_fleet_device() {
        let error = resolve_fleet_link_fields(&Config::default(), None).unwrap_err();
        assert!(error.contains("requires credentials for a device already enrolled"));
        assert!(error.contains("no owner/member hierarchy"));
        assert!(error.contains("worker connect <pairing-address>"));
        assert!(error.contains("first-Worker Cloudflare bootstrap"));
        assert!(!error.contains("dev.herdr-mcp.link-prod.plist"));
        assert!(!error.contains("Io("));
    }

    #[test]
    fn enrolled_config_does_not_require_link_plist_for_fleet_identity() {
        let mut config = Config::default();
        config
            .set_edge_device_id("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV")
            .unwrap();
        config
            .set_edge_public_origin("https://edge.example")
            .unwrap();

        let (device_id, keychain_service, origin) =
            resolve_fleet_link_fields(&config, None).unwrap();
        assert_eq!(device_id, "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(
            keychain_service,
            "herdr-edge-link-dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
        assert_eq!(origin, "https://edge.example");
    }

    #[test]
    fn legacy_install_can_still_resolve_from_production_link_environment() {
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            "HERDR_WORKSTATION_ID".to_owned(),
            "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
        );
        env.insert(
            "HERDR_EDGE_URL".to_owned(),
            "wss://edge.example/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
        );
        env.insert(
            "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
            "legacy-owner-service".to_owned(),
        );
        let (device_id, keychain_service, origin) =
            resolve_fleet_link_fields(&Config::default(), Some(&env)).unwrap();
        assert_eq!(device_id, "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(keychain_service, "legacy-owner-service");
        assert_eq!(origin, "https://edge.example");
    }

    #[test]
    fn pairing_code_formats_with_a_space_for_humans() {
        assert_eq!(format_pairing_code("123456"), "123 456");
        assert_eq!(format_pairing_code("000000"), "000 000");
    }

    #[test]
    fn pairing_expiry_formats_as_absolute_rfc3339_utc() {
        assert_eq!(
            format_pairing_expiry(0).as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
    }

    #[test]
    fn inventory_rendering_exposes_age_usage_and_never_used() {
        let now = 1_000_000_000_u64;
        let two_days_ago = now - 2 * 24 * 60 * 60 * 1_000;
        let three_hours_ago = now - 3 * 60 * 60 * 1_000;

        let connectors = render_connector_inventory(
            json!({
                "ok": true,
                "connectors": [{
                    "connector_id": "conn_example123",
                    "client_id": "client-1",
                    "client_name": "ChatGPT",
                    "scope": "mcp",
                    "created_at_ms": two_days_ago,
                    "last_used_at_ms": null,
                    "grant_origin": "explicit_approval"
                }],
                "legacy_clients": [{
                    "client_id": "legacy-1",
                    "client_name": "Old client",
                    "created_at_ms": two_days_ago,
                    "last_used_at_ms": null,
                    "grant_origin": "pre_v0_4_6_legacy"
                }]
            }),
            now,
        );
        assert_eq!(connectors["connectors"][0]["age"], "2d");
        assert_eq!(connectors["connectors"][0]["last_used"], "never used");
        assert_eq!(connectors["connectors"][0]["usage_state"], "never_used");
        assert_eq!(
            connectors["legacy_clients"][0]["last_used"],
            "unknown (pre-v0.4.6)"
        );
        assert_eq!(
            connectors["legacy_clients"][0]["usage_state"],
            "unknown_legacy"
        );

        let automations = render_automation_inventory(
            json!({
                "automations": [{
                    "client_id": "svc_example123",
                    "name": "gitlab:project",
                    "created_at_ms": two_days_ago,
                    "last_used_at_ms": three_hours_ago
                }]
            }),
            now,
        );
        assert_eq!(automations["automations"][0]["age"], "2d");
        assert!(
            automations["automations"][0]["last_used"]
                .as_str()
                .unwrap()
                .contains("(3h ago)")
        );

        let devices = render_device_inventory(
            json!({
                "devices": [{
                    "device_id": "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "name": "macbook",
                    "enrolled_at_ms": two_days_ago,
                    "last_seen_at_ms": null
                }]
            }),
            now,
        );
        assert_eq!(devices["devices"][0]["age"], "2d");
        assert_eq!(devices["devices"][0]["last_used"], "never used");
        assert_eq!(devices["devices"][0]["usage_state"], "never_used");
    }

    #[test]
    fn pairing_request_bodies_omit_unspecified_name_and_preserve_explicit_name() {
        let unnamed_create = pairing_create_request_body(600, None);
        assert_eq!(unnamed_create["ttl_seconds"], 600);
        assert!(unnamed_create.get("name").is_none());

        let named_create = pairing_create_request_body(600, Some("Nathan Mac"));
        assert_eq!(named_create["name"], "Nathan Mac");

        let pairing_id = "pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let unnamed_consume = pairing_consume_request_body(pairing_id, "123456", None);
        assert_eq!(unnamed_consume["pairing_id"], pairing_id);
        assert_eq!(unnamed_consume["code"], "123456");
        assert!(unnamed_consume.get("name").is_none());

        let named_consume = pairing_consume_request_body(pairing_id, "123456", Some("Nathan Mac"));
        assert_eq!(named_consume["name"], "Nathan Mac");

        let revoke_client = connector_client_revoke_request_body("https://legacy.example/client");
        assert_eq!(revoke_client["client_id"], "https://legacy.example/client");
        assert!(revoke_client.get("connector_id").is_none());
    }

    #[test]
    fn pairing_address_validation_requires_https_pair_path_and_fragment() {
        let (origin, id) = parse_pairing_address("https://edge.example/pair#pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        assert_eq!(origin, "https://edge.example");
        assert_eq!(
            id,
            "pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );

        assert!(parse_pairing_address("http://edge.example/pair#pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_err());
        assert!(parse_pairing_address("https://edge.example/other#pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_err());
        assert!(parse_pairing_address("https://edge.example/pair").is_err());
        assert!(parse_pairing_address("https://edge.example/pair?x=1#pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_err());
        assert!(parse_pairing_address("https://edge.example/pair#pair with space").is_err());
        assert!(parse_pairing_address("not a url").is_err());
    }

    #[test]
    fn read_pairing_code_from_accepts_leading_zero_and_rejects_bad() {
        let mut ok = std::io::Cursor::new(b"000000\n".to_vec());
        assert_eq!(read_pairing_code_from(&mut ok).unwrap(), "000000");

        let mut bad = std::io::Cursor::new(b"12ab\n".to_vec());
        assert!(read_pairing_code_from(&mut bad).is_err());

        let mut short = std::io::Cursor::new(b"123\n".to_vec());
        assert!(read_pairing_code_from(&mut short).is_err());
    }

    #[test]
    fn automation_create_body_is_explicitly_device_bound() {
        let body = automation_create_request_body(
            "gitlab:group/project:prod",
            "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
        );
        assert_eq!(body["name"], "gitlab:group/project:prod");
        assert_eq!(body["device"], "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV");
        // A unique-name selector is passed through verbatim; never resolved client-side.
        let named = automation_create_request_body("gitlab:ci:pipeline", "build-runner-01");
        assert_eq!(named["device"], "build-runner-01");
    }

    #[test]
    fn connector_revoke_body_uses_connector_id_not_client_id() {
        let body = connector_revoke_request_body("conn_abc123XYZ");
        assert_eq!(body["connector_id"], "conn_abc123XYZ");
        assert!(body.get("client_id").is_none());
    }

    #[test]
    fn management_request_bodies_never_carry_enrollment_secrets() {
        // The body builders only ever include the device selector and never a
        // device/owner secret, so a leaked request can never expose credentials.
        let automation = automation_create_request_body("gitlab:ci", "build-runner-01");
        assert!(!automation.to_string().contains("secret"));
        assert!(!automation.to_string().contains("credential"));
        let revoke = connector_revoke_request_body("conn_abc");
        assert!(!revoke.to_string().contains("secret"));
    }

    #[test]
    fn compensation_after_keychain_store_revokes_remote_and_deletes_local_on_config_failure() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let order = Rc::new(RefCell::new(Vec::<String>::new()));
        let order_rev = order.clone();
        let order_del = order.clone();

        let revoke = move |edge: &str, device: &str, secret: &str| -> Result<bool, String> {
            assert_eq!(edge, "https://edge.example");
            assert_eq!(device, "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV");
            assert_eq!(
                secret,
                "devsec_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            );
            order_rev.borrow_mut().push("revoke".to_owned());
            Ok(true)
        };
        let delete = move |service: &str, account: &str| -> Result<(), String> {
            assert_eq!(service, "herdr-edge-link-dev_01ARZ3NDEKTSV4RRFFQ69G5FAV");
            assert_eq!(account, "testuser");
            order_del.borrow_mut().push("delete".to_owned());
            Ok(())
        };

        let (revoked, deleted) = compensate_after_store(
            "https://edge.example",
            "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "herdr-edge-link-dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "testuser",
            "devsec_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            &revoke,
            &delete,
        );
        assert!(revoked);
        assert!(deleted);
        assert_eq!(*order.borrow(), vec!["revoke", "delete"]);
    }

    #[test]
    fn reconcile_failure_rolls_back_local_binding_with_evidence() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let dir = env::temp_dir().join(format!(
            "herdr-worker-reconcile-rollback-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        let paths = crate::paths::RuntimePaths {
            config_dir: dir.clone(),
            config_file: config_path.clone(),
            dev_state_dir: dir.join("dev-state"),
            herdr_socket: None,
            instance: InstanceId::default_instance(),
        };

        // Previous config is the durable local binding that must be restored.
        // OLD and NEW device ids are distinct valid ULIDs so the "new id absent"
        // assertion is meaningful, not a false positive.
        const OLD_DEVICE_ID: &str = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        const NEW_DEVICE_ID: &str = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAW";
        let previous_config = Config {
            edge_public_origin: Some("https://old.example".to_owned()),
            edge_device_id: Some(OLD_DEVICE_ID.to_owned()),
            ..Config::default()
        };

        let written = Rc::new(RefCell::new(Vec::<Config>::new()));
        let written_clone = written.clone();
        let write_config = move |_paths: &RuntimePaths, config: &Config| -> Result<(), String> {
            written_clone.borrow_mut().push(config.clone());
            Ok(())
        };
        let revoke = |_: &str, _: &str, _: &str| -> Result<bool, String> { Ok(true) };
        let delete = |_: &str, _: &str| -> Result<(), String> { Ok(()) };
        let reconcile = |_paths: &RuntimePaths| -> Result<(), String> {
            Err("simulated reconcile failure".to_owned())
        };

        let evidence = rollback_after_reconcile_failure(
            "https://edge.example",
            NEW_DEVICE_ID,
            &format!("herdr-edge-link-{NEW_DEVICE_ID}"),
            "testuser",
            &format!("devsec_{}", "b".repeat(64)),
            &paths,
            &previous_config,
            &write_config,
            &revoke,
            &delete,
            &reconcile,
        );

        assert!(evidence.revoked);
        assert!(evidence.keychain_deleted);
        assert!(evidence.config_restored);
        assert!(!evidence.link_reconciled);
        assert!(evidence.reconcile_error.is_some());
        assert!(evidence.restore_error.is_none());

        // The previous config (old device binding) must be the last thing written,
        // so the new device id is never left in the local binding after rollback.
        let writes = written.borrow();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], previous_config);
        assert_eq!(writes[0].edge_device_id.as_deref(), Some(OLD_DEVICE_ID));
        assert_ne!(writes[0].edge_device_id.as_deref(), Some(NEW_DEVICE_ID));

        // No secret is ever surfaced in the rollback evidence.
        let error = format!(
            "device {} is paired but the local binding could not be reconciled: simulated; compensation revoked={} keychain_deleted={} config_restored={} link_reconciled={} restore_error={} reconcile_error={}",
            NEW_DEVICE_ID,
            evidence.revoked,
            evidence.keychain_deleted,
            evidence.config_restored,
            evidence.link_reconciled,
            evidence.restore_error.as_deref().unwrap_or("none"),
            evidence.reconcile_error.as_deref().unwrap_or("none"),
        );
        assert!(!error.contains("devsec_"));
        assert!(!error.contains("enroll_"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconcile_failure_restore_error_is_reported_not_hidden() {
        let dir = env::temp_dir().join(format!(
            "herdr-worker-reconcile-restore-error-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        let paths = crate::paths::RuntimePaths {
            config_dir: dir.clone(),
            config_file: config_path.clone(),
            dev_state_dir: dir.join("dev-state"),
            herdr_socket: None,
            instance: InstanceId::default_instance(),
        };
        let previous_config = Config::default();

        let write_config = |_paths: &RuntimePaths, _config: &Config| -> Result<(), String> {
            Err("restore write failed".to_owned())
        };
        let revoke = |_: &str, _: &str, _: &str| -> Result<bool, String> { Ok(true) };
        let delete = |_: &str, _: &str| -> Result<(), String> { Ok(()) };
        let reconcile = |_paths: &RuntimePaths| -> Result<(), String> {
            Err("simulated reconcile failure".to_owned())
        };

        let evidence = rollback_after_reconcile_failure(
            "https://edge.example",
            "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "herdr-edge-link-dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "testuser",
            &format!("devsec_{}", "b".repeat(64)),
            &paths,
            &previous_config,
            &write_config,
            &revoke,
            &delete,
            &reconcile,
        );

        // A failed restore must be surfaced, not hidden, and must not claim
        // link_reconciled since the config was never restored.
        assert!(evidence.revoked);
        assert!(evidence.keychain_deleted);
        assert!(!evidence.config_restored);
        assert!(!evidence.link_reconciled);
        assert!(evidence.restore_error.is_some());
        assert!(evidence.reconcile_error.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn connect_macos_inner_rolls_back_whole_transaction_on_reconcile_failure() {
        use std::cell::RefCell;
        use std::rc::Rc;

        const OLD_DEVICE_ID: &str = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        const NEW_DEVICE_ID: &str = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAW";
        const DEVICE_SECRET: &str =
            "devsec_cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

        let dir = env::temp_dir().join(format!(
            "herdr-worker-connect-transaction-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();

        // Config file carries the OLD device binding before the transaction.
        let config_path = dir.join("config.toml");
        let previous_config = Config {
            edge_public_origin: Some("https://old.example".to_owned()),
            edge_device_id: Some(OLD_DEVICE_ID.to_owned()),
            ..Config::default()
        };
        fs::write(&config_path, previous_config.render()).unwrap();

        let paths = crate::paths::RuntimePaths {
            config_dir: dir.clone(),
            config_file: config_path.clone(),
            dev_state_dir: dir.join("dev-state"),
            herdr_socket: None,
            instance: InstanceId::default_instance(),
        };

        // current_account() reads the real USER env var; use it for assertions.
        let account = current_account().unwrap();
        let account_for_store = account.clone();
        let account_for_delete = account.clone();

        let revoke_calls = Rc::new(RefCell::new(0));
        let delete_calls = Rc::new(RefCell::new(0));
        let activation_calls = Rc::new(RefCell::new(0));
        let reconcile_calls = Rc::new(RefCell::new(0));
        let writes = Rc::new(RefCell::new(Vec::<Config>::new()));
        let revoke_calls_hook = revoke_calls.clone();
        let delete_calls_hook = delete_calls.clone();
        let activation_calls_hook = activation_calls.clone();
        let reconcile_calls_hook = reconcile_calls.clone();
        let writes_hook = writes.clone();

        let store_secret = move |service: &str, acct: &str, _secret: &str| -> Result<(), String> {
            assert_eq!(service, format!("herdr-edge-link-{NEW_DEVICE_ID}"));
            assert_eq!(acct, account_for_store);
            Ok(())
        };
        let load_config = |path: &Path, instance: &InstanceId| -> Result<Config, String> {
            Config::load_for_instance(path, instance)
        };
        let write_config = move |_paths: &RuntimePaths, config: &Config| -> Result<(), String> {
            writes_hook.borrow_mut().push(config.clone());
            Ok(())
        };
        let revoke = move |origin: &str, device: &str, secret: &str| -> Result<bool, String> {
            assert_eq!(origin, "https://edge.example");
            assert_eq!(device, NEW_DEVICE_ID);
            assert_eq!(secret, DEVICE_SECRET);
            *revoke_calls_hook.borrow_mut() += 1;
            Ok(true)
        };
        let delete = move |service: &str, acct: &str| -> Result<(), String> {
            assert_eq!(service, format!("herdr-edge-link-{NEW_DEVICE_ID}"));
            assert_eq!(acct, account_for_delete);
            *delete_calls_hook.borrow_mut() += 1;
            Ok(())
        };
        let activate = move |_paths: &RuntimePaths| -> Result<(), String> {
            *activation_calls_hook.borrow_mut() += 1;
            Err("simulated activation failure".to_owned())
        };
        let reconcile = move |_paths: &RuntimePaths| -> Result<(), String> {
            *reconcile_calls_hook.borrow_mut() += 1;
            Ok(())
        };
        let consume = move |origin: &str, id: &str, code: &str, name: Option<&str>| {
            assert_eq!(origin, "https://edge.example");
            assert_eq!(
                id,
                "pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            );
            assert_eq!(code, "123456");
            assert_eq!(name, None);
            Ok(EnrolledCredential {
                device_id: NEW_DEVICE_ID.to_owned(),
                workstation_id: NEW_DEVICE_ID.to_owned(),
                device_secret: DEVICE_SECRET.to_owned(),
            })
        };

        let result = connect_macos_inner(
            &paths,
            "https://edge.example",
            "pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "123456",
            None,
            store_secret,
            load_config,
            write_config,
            revoke,
            delete,
            activate,
            reconcile,
            consume,
        );

        // The transaction must fail closed with rollback evidence.
        let error = result.unwrap_err();
        assert!(error.contains("could not be activated"));
        assert!(error.contains("revoked=true"));
        assert!(error.contains("keychain_deleted=true"));
        assert!(error.contains("config_restored=true"));
        assert!(error.contains("link_reconciled=true"));
        assert!(!error.contains("devsec_"));
        assert!(!error.contains("enroll_"));

        // Remote revoke + Keychain delete happened exactly once, in the rollback.
        assert_eq!(*revoke_calls.borrow(), 1);
        assert_eq!(*delete_calls.borrow(), 1);
        assert_eq!(*activation_calls.borrow(), 1);
        // Reconcile-back runs once after restoring the old config.
        assert_eq!(*reconcile_calls.borrow(), 1);

        // Final write must be the OLD config: old id present, new id absent.
        let final_writes = writes.borrow();
        assert_eq!(final_writes.len(), 2);
        assert_eq!(
            final_writes[0].edge_device_id.as_deref(),
            Some(NEW_DEVICE_ID)
        );
        assert_eq!(
            final_writes[1].edge_device_id.as_deref(),
            Some(OLD_DEVICE_ID)
        );
        assert_ne!(
            final_writes[1].edge_device_id.as_deref(),
            Some(NEW_DEVICE_ID)
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn connect_macos_inner_success_consumes_exact_pairing_and_persists() {
        use std::cell::RefCell;
        use std::rc::Rc;

        const NEW_DEVICE_ID: &str = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAW";
        const DEVICE_SECRET: &str =
            "devsec_cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

        let dir = env::temp_dir().join(format!(
            "herdr-worker-connect-success-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        let paths = crate::paths::RuntimePaths {
            config_dir: dir.clone(),
            config_file: config_path.clone(),
            dev_state_dir: dir.join("dev-state"),
            herdr_socket: None,
            instance: InstanceId::default_instance(),
        };

        let account = current_account().unwrap();
        let account_for_store = account.clone();
        let consume_calls = Rc::new(RefCell::new(0));
        let writes = Rc::new(RefCell::new(Vec::<Config>::new()));
        let consume_calls_hook = consume_calls.clone();
        let writes_hook = writes.clone();

        let store_secret = move |service: &str, acct: &str, secret: &str| -> Result<(), String> {
            assert_eq!(service, format!("herdr-edge-link-{NEW_DEVICE_ID}"));
            assert_eq!(acct, account_for_store);
            assert_eq!(secret, DEVICE_SECRET);
            Ok(())
        };
        let load_config = |path: &Path, instance: &InstanceId| -> Result<Config, String> {
            Config::load_for_instance(path, instance)
        };
        let write_config = move |_paths: &RuntimePaths, config: &Config| -> Result<(), String> {
            writes_hook.borrow_mut().push(config.clone());
            Ok(())
        };
        let revoke = |_: &str, _: &str, _: &str| -> Result<bool, String> { Ok(true) };
        let delete = |_: &str, _: &str| -> Result<(), String> { Ok(()) };
        let activate = |_paths: &RuntimePaths| -> Result<(), String> { Ok(()) };
        let reconcile = |_paths: &RuntimePaths| -> Result<(), String> { Ok(()) };
        let consume = move |origin: &str, id: &str, code: &str, name: Option<&str>| {
            assert_eq!(origin, "https://edge.example");
            assert_eq!(
                id,
                "pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            );
            assert_eq!(code, "000000");
            assert_eq!(name, Some("mac-b"));
            *consume_calls_hook.borrow_mut() += 1;
            Ok(EnrolledCredential {
                device_id: NEW_DEVICE_ID.to_owned(),
                workstation_id: NEW_DEVICE_ID.to_owned(),
                device_secret: DEVICE_SECRET.to_owned(),
            })
        };

        let result = connect_macos_inner(
            &paths,
            "https://edge.example",
            "pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "000000",
            Some("mac-b"),
            store_secret,
            load_config,
            write_config,
            revoke,
            delete,
            activate,
            reconcile,
            consume,
        );

        assert!(result.is_ok());
        assert_eq!(*consume_calls.borrow(), 1);
        // The new config (with the new device id) is written exactly once.
        let final_writes = writes.borrow();
        assert_eq!(final_writes.len(), 1);
        assert_eq!(
            final_writes[0].edge_device_id.as_deref(),
            Some(NEW_DEVICE_ID)
        );
        assert_eq!(
            final_writes[0].edge_public_origin.as_deref(),
            Some("https://edge.example")
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
