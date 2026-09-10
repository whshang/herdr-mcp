#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use crate::cli::ServiceCommand;
use crate::cli::WorkerCommand;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
use crate::config::Config;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
use crate::instance::InstanceId;
#[cfg(target_os = "macos")]
use crate::link::ownership::LINK_PROD_LABEL;
use crate::paths::RuntimePaths;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use reqwest::blocking::{Client, Response};
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::Value;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
use serde_json::json;
#[cfg(any(target_os = "macos", target_os = "linux", test))]
use sha2::{Digest, Sha256};
#[cfg(any(target_os = "macos", target_os = "linux", test))]
use std::env;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
use std::fs::{self, OpenOptions};
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use std::io;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
use std::io::BufRead;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
use std::io::Write;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
use url::Url;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
const LEGACY_LINK_KEYCHAIN_SERVICE: &str = "herdr-edge-prod-link-secret";

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
struct FleetLinkIdentity {
    edge_origin: String,
    workstation_id: String,
    credential: String,
}

#[derive(Clone, Debug)]
pub(crate) struct EnrolledCredential {
    pub(crate) device_id: String,
    pub(crate) workstation_id: String,
    pub(crate) device_secret: String,
    pub(crate) recovered_existing: bool,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
fn pairing_create_request_body(
    ttl_seconds: u64,
    name: Option<&str>,
    recover_device_id: Option<&str>,
) -> Value {
    let mut body = json!({ "ttl_seconds": ttl_seconds });
    if let Some(name) = name {
        body["name"] = json!(name);
    }
    if let Some(device_id) = recover_device_id {
        body["recover_device_id"] = json!(device_id);
    }
    body
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
fn pairing_consume_request_body(pairing_id: &str, code: &str, name: Option<&str>) -> Value {
    match name {
        Some(name) => json!({ "pairing_id": pairing_id, "code": code, "name": name }),
        None => json!({ "pairing_id": pairing_id, "code": code }),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
fn automation_create_request_body(name: &str, device: &str) -> Value {
    json!({ "name": name, "device": device })
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
fn connector_revoke_request_body(connector_id: &str) -> Value {
    json!({ "connector_id": connector_id })
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
fn render_connector_inventory(mut payload: Value, now_ms: u64, include_all: bool) -> Value {
    if let Some(connectors) = payload.get_mut("connectors").and_then(Value::as_array_mut) {
        if !include_all {
            connectors.retain(|connector| {
                connector.get("status").and_then(Value::as_str) != Some("revoked")
            });
        }
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
        if !include_all {
            legacy.retain(|client| {
                client.get("registration_state").and_then(Value::as_str) != Some("revoked")
            });
        }
        for client in legacy {
            let created = client.get("created_at_ms").and_then(Value::as_u64);
            let last_used = client.get("last_used_at_ms").and_then(Value::as_u64);
            annotate_inventory_entry(client, created, last_used, now_ms, true);
        }
    }

    let listed_counts = displayed_connector_token_counts(&payload);
    if let Some(object) = payload.as_object_mut() {
        // Preserve the Edge aggregate for backward compatibility, but make its
        // scope explicit because the default CLI view intentionally hides
        // revoked/history rows. The displayed-record aggregate is always
        // directly reconcilable with the rows the operator can see.
        object.insert(
            "inventory_filter".to_owned(),
            json!(if include_all { "all" } else { "actionable" }),
        );
        if object.contains_key("token_counts") {
            object.insert("token_counts_scope".to_owned(), json!("all_stored"));
        }
        object.insert("listed_token_counts".to_owned(), listed_counts);
        object.insert(
            "listed_token_counts_scope".to_owned(),
            json!("displayed_records"),
        );
    }
    payload
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
fn displayed_connector_token_counts(payload: &Value) -> Value {
    let mut active_access = 0_u64;
    let mut active_refresh = 0_u64;
    for key in ["connectors", "legacy_clients"] {
        if let Some(entries) = payload.get(key).and_then(Value::as_array) {
            for entry in entries {
                active_access = active_access.saturating_add(
                    entry
                        .get("active_access_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                active_refresh = active_refresh.saturating_add(
                    entry
                        .get("active_refresh_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
            }
        }
    }
    json!({"active_access": active_access, "active_refresh": active_refresh})
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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

#[cfg(any(target_os = "linux", test))]
fn worker_command_requires_supported_workstation(command: &WorkerCommand) -> bool {
    !matches!(
        command,
        WorkerCommand::List | WorkerCommand::ConnectorList { .. } | WorkerCommand::AutomationList
    )
}

pub fn run(command: WorkerCommand) -> Result<ExitCode, String> {
    let paths = RuntimePaths::discover()?;
    if paths.instance.is_named() {
        return Err("Worker pairing is available only on the default Herdr instance".to_owned());
    }
    #[cfg(target_os = "linux")]
    if worker_command_requires_supported_workstation(&command) {
        crate::linux_service_manager::ensure_supported_linux_workstation("Worker mutation")?;
    }
    match command {
        WorkerCommand::List => list_devices(&paths),
        WorkerCommand::Bootstrap => crate::worker_bootstrap::run(&paths),
        WorkerCommand::Pair {
            ttl_seconds,
            name,
            recover_device_id,
        } => create_pairing(
            &paths,
            ttl_seconds,
            name.as_deref(),
            recover_device_id.as_deref(),
        ),
        WorkerCommand::Connect {
            pairing_address,
            name,
        } => {
            let name = name.or_else(crate::device_name::system_device_display_name);
            connect_existing_worker(&paths, &pairing_address, name.as_deref())
        }
        WorkerCommand::Rename { name } => rename_current_device(&paths, &name),
        WorkerCommand::Revoke { device_id } => revoke_device(&paths, &device_id),
        WorkerCommand::CredentialRepairPrepare => prepare_device_credential_repair(&paths),
        WorkerCommand::CredentialRepairApply {
            device_id,
            credential_verifier_sha256,
        } => apply_device_credential_repair(&paths, &device_id, &credential_verifier_sha256),
        WorkerCommand::CredentialRepairFinalize => finalize_device_credential_repair(&paths),
        WorkerCommand::ConnectorApprove { request_id } => approve_connector(&paths, &request_id),
        WorkerCommand::ConnectorCancel { request_id } => cancel_connector(&paths, &request_id),
        WorkerCommand::ConnectorList { include_all } => list_connectors(&paths, include_all),
        WorkerCommand::ConnectorRevoke { connector_id } => revoke_connector(&paths, &connector_id),
        WorkerCommand::ConnectorClientRevoke { client_id } => {
            revoke_connector_client(&paths, &client_id)
        }
        WorkerCommand::ConnectorPlannerControl { action, request_id } => {
            connector_planner_control(&paths, &action, request_id.as_deref())
        }
        WorkerCommand::ConnectorWebChatControl {
            connector_id,
            device_id,
            endpoint_ref,
            provider,
            account_ref,
            allowed,
        } => set_connector_webchat_control(
            &paths,
            &connector_id,
            &device_id,
            &endpoint_ref,
            &provider,
            &account_ref,
            allowed,
        ),
        WorkerCommand::ConnectorPageAssist {
            connector_id,
            device_id,
            endpoint_ref,
            allowed,
        } => set_connector_page_assist(&paths, &connector_id, &device_id, &endpoint_ref, allowed),
        WorkerCommand::AutomationCreate { name, device } => {
            create_automation(&paths, &name, &device)
        }
        WorkerCommand::AutomationList => list_automations(&paths),
        WorkerCommand::AutomationRotate { client_id } => rotate_automation(&paths, &client_id),
        WorkerCommand::AutomationRevoke { client_id } => revoke_automation(&paths, &client_id),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn credential_repair_staging_service(device_id: &str) -> String {
    format!("herdr-edge-link-recovery-{device_id}")
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn device_secret_verifier(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn new_local_device_secret() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("cannot generate credential-repair secret: {error}"))?;
    let mut out = String::with_capacity(64 + "devsec_".len());
    out.push_str("devsec_");
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}")
            .map_err(|_| "cannot encode credential-repair secret".to_owned())?;
    }
    Ok(out)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn prepare_device_credential_repair(_paths: &RuntimePaths) -> Result<ExitCode, String> {
    Err("credential repair is supported on macOS and Linux enrolled devices".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn prepare_device_credential_repair(paths: &RuntimePaths) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let device_id = config
        .edge_device_id
        .as_deref()
        .ok_or_else(|| "credential repair requires an enrolled device_id".to_owned())?;
    let device_id = crate::config::normalize_device_id(device_id)?;
    let account = current_account()?;
    let staging_service = credential_repair_staging_service(&device_id);
    let secret = new_local_device_secret()?;
    validate_device_secret(&secret)?;
    crate::credential_store::store(&staging_service, &account, &secret)?;
    let verifier = device_secret_verifier(&secret);
    print_json(&json!({
        "ok": true,
        "action": "credential_repair_prepare",
        "device_id": device_id,
        "credential_verifier_sha256": verifier,
        "staged": true,
        "secret_printed": false,
        "next": "run worker credential-repair apply from another enrolled fleet-admin device before finalize, or use worker pair --recover-device for exact-device recovery",
    }))?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn apply_device_credential_repair(
    _paths: &RuntimePaths,
    _device_id: &str,
    _verifier: &str,
) -> Result<ExitCode, String> {
    Err("credential repair is supported on macOS and Linux enrolled devices".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rebind_device_credential_verifier(
    identity: &FleetLinkIdentity,
    device_id: &str,
    verifier: &str,
) -> Result<Value, String> {
    let device_id = crate::config::normalize_device_id(device_id)?;
    if verifier.len() != 64
        || !verifier
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(
            "credential repair verifier must be exactly 64 lowercase hexadecimal characters"
                .to_owned(),
        );
    }
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client_for_origin(&identity.edge_origin)?
        .post(endpoint(
            &identity.edge_origin,
            "/devices/credential-rebind",
        )?)
        .headers(headers)
        .json(&json!({
            "device_id": device_id,
            "credential_verifier_sha256": verifier,
        }))
        .send()
        .map_err(|error| format!("cannot apply device credential repair: {error}"))?;
    parse_json_response(response, "device credential repair")
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn apply_device_credential_repair(
    paths: &RuntimePaths,
    device_id: &str,
    verifier: &str,
) -> Result<ExitCode, String> {
    let device_id = crate::config::normalize_device_id(device_id)?;
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let payload = rebind_device_credential_verifier(&identity, &device_id, verifier)?;
    print_json(&json!({
        "ok": true,
        "action": "credential_repair_apply",
        "device_id": payload.get("device_id").and_then(Value::as_str).unwrap_or(&device_id),
        "updated_at_ms": payload.get("updated_at_ms").and_then(Value::as_u64),
        "secret_printed": false,
        "next": "run worker credential-repair finalize on the repaired device",
    }))?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn finalize_device_credential_repair(_paths: &RuntimePaths) -> Result<ExitCode, String> {
    Err("credential repair is supported on macOS and Linux enrolled devices".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn finalize_device_credential_repair(paths: &RuntimePaths) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let device_id = config
        .edge_device_id
        .as_deref()
        .ok_or_else(|| "credential repair requires an enrolled device_id".to_owned())?;
    let device_id = crate::config::normalize_device_id(device_id)?;
    let target_service = config.edge_link_keychain_service().ok_or_else(|| {
        "credential repair requires a device-specific credential service".to_owned()
    })?;
    let expected_service = format!("herdr-edge-link-{device_id}");
    if target_service != expected_service {
        return Err("configured credential service does not match the enrolled device".to_owned());
    }
    let account = current_account()?;
    let staging_service = credential_repair_staging_service(&device_id);
    let secret = crate::credential_store::load(&staging_service, &account)
        .map_err(|error| format!("cannot load staged credential repair secret: {error}"))?;
    validate_device_secret(&secret)?;

    crate::credential_store::store(&target_service, &account, &secret).map_err(|error| {
        format!("cannot commit repaired credential to the device service: {error}")
    })?;

    #[cfg(target_os = "macos")]
    crate::link::switch_prod_link_credential_service(paths, &device_id, &target_service)?;
    #[cfg(target_os = "linux")]
    crate::linux_service_manager::reconcile_link()?;

    crate::credential_store::delete(&staging_service, &account).map_err(|error| {
        format!("credential repair succeeded but staging cleanup failed: {error}")
    })?;
    print_json(&json!({
        "ok": true,
        "action": "credential_repair_finalize",
        "device_id": device_id,
        "credential_service": target_service,
        "staging_deleted": true,
        "secret_printed": false,
    }))?;
    Ok(ExitCode::SUCCESS)
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

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub(crate) fn extension_fleet_snapshot(_paths: &RuntimePaths) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "ok": false,
        "code": "device_inventory_platform_unsupported",
    }))
}

#[cfg(target_os = "linux")]
pub(crate) fn extension_fleet_snapshot(paths: &RuntimePaths) -> Result<Value, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let owner = resolve_fleet_link_identity(paths, &config)?;
    let client = client_for_origin(&owner.edge_origin)?;
    extension_fleet_snapshot_with_client(paths, &client)
}

#[cfg(target_os = "linux")]
pub(crate) fn extension_fleet_snapshot_with_client(
    paths: &RuntimePaths,
    client: &Client,
) -> Result<Value, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let owner = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&owner.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&owner.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client
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
    let service = crate::linux_service_manager::doctor_status().unwrap_or_else(|_| json!({}));
    Ok(json!({
        "ok": true,
        "devices": payload.get("devices").cloned().unwrap_or_else(|| json!([])),
        "observed_at_ms": payload.get("observed_at_ms").cloned().unwrap_or(Value::Null),
        "local": {
            "device_id": config.edge_device_id,
            "runtime_version": crate::runtime_meta::runtime_version(),
            "runtime_generation": service.get("generation").cloned().unwrap_or(Value::Null),
            "link_loaded": service.get("link_loaded").cloned().unwrap_or(Value::Null),
            "service_healthy": service.get("healthy").cloned().unwrap_or(Value::Null),
        },
    }))
}

#[cfg(target_os = "macos")]
pub(crate) fn extension_fleet_snapshot(paths: &RuntimePaths) -> Result<Value, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let owner = resolve_fleet_link_identity(paths, &config)?;
    let client = client_for_origin(&owner.edge_origin)?;
    extension_fleet_snapshot_with_client(paths, &client)
}

#[cfg(target_os = "macos")]
pub(crate) fn extension_fleet_snapshot_with_client(
    paths: &RuntimePaths,
    client: &Client,
) -> Result<Value, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let owner = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&owner.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&owner.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client
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

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn create_pairing(
    _paths: &RuntimePaths,
    _ttl_seconds: u64,
    _name: Option<&str>,
) -> Result<ExitCode, String> {
    Err("worker pair is supported on macOS and Linux enrolled devices; this platform has no supported enrolled-device credential backend"
        .to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn create_pairing(
    paths: &RuntimePaths,
    ttl_seconds: u64,
    name: Option<&str>,
    recover_device_id: Option<&str>,
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
    let response = client_for_origin(&owner.edge_origin)?
        .post(endpoint)
        .headers(headers)
        .json(&pairing_create_request_body(
            ttl_seconds,
            name,
            recover_device_id,
        ))
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

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn connect_existing_worker(
    _paths: &RuntimePaths,
    _pairing_address: &str,
    _name: Option<&str>,
) -> Result<ExitCode, String> {
    Err("worker connect is currently supported on macOS and Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn connect_existing_worker(
    paths: &RuntimePaths,
    pairing_address: &str,
    name: Option<&str>,
) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    connect_existing_worker_flow(
        paths,
        pairing_address,
        name,
        &config,
        || extension_fleet_snapshot(paths),
        read_pairing_code_tty,
        |paths, edge_origin, pairing_id, code, name| {
            connect_macos_inner(
                paths,
                edge_origin,
                pairing_id,
                code,
                name,
                crate::credential_store::store,
                Config::load_for_instance,
                write_config_atomic,
                revoke_self,
                crate::credential_store::delete,
                activate_connected_runtime_after_pairing,
                |_paths| crate::linux_service_manager::reconcile_link(),
                consume_pairing,
            )
        },
    )
}

#[cfg(target_os = "macos")]
fn connect_existing_worker(
    paths: &RuntimePaths,
    pairing_address: &str,
    name: Option<&str>,
) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    connect_existing_worker_flow(
        paths,
        pairing_address,
        name,
        &config,
        || extension_fleet_snapshot(paths),
        read_pairing_code_tty,
        |paths, edge_origin, pairing_id, code, name| {
            connect_macos_inner(
                paths,
                edge_origin,
                pairing_id,
                code,
                name,
                crate::credential_store::store,
                Config::load_for_instance,
                write_config_atomic,
                revoke_self,
                crate::credential_store::delete,
                activate_connected_runtime_after_pairing,
                crate::link::reconcile_after_service_generation_change,
                consume_pairing,
            )
        },
    )
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn existing_enrollment_for_worker(
    config: &Config,
    requested_origin: &str,
) -> Result<Option<String>, String> {
    let (Some(existing_origin), Some(device_id)) = (
        config.edge_public_origin.as_deref(),
        config.edge_device_id.as_deref(),
    ) else {
        return Ok(None);
    };
    if normalize_edge_origin(existing_origin)? != normalize_edge_origin(requested_origin)? {
        return Ok(None);
    }
    Ok(Some(crate::config::normalize_device_id(device_id)?))
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn inventory_has_active_device(snapshot: &Value, device_id: &str) -> bool {
    snapshot.get("ok").and_then(Value::as_bool) == Some(true)
        && snapshot
            .get("devices")
            .and_then(Value::as_array)
            .is_some_and(|devices| {
                devices.iter().any(|device| {
                    device.get("device_id").and_then(Value::as_str) == Some(device_id)
                        && device.get("authorization").and_then(Value::as_str) == Some("active")
                })
            })
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn connect_existing_worker_flow<I, R, C>(
    paths: &RuntimePaths,
    pairing_address: &str,
    name: Option<&str>,
    config: &Config,
    inventory: I,
    read_code: R,
    connect_new: C,
) -> Result<ExitCode, String>
where
    I: FnOnce() -> Result<Value, String>,
    R: FnOnce() -> Result<String, String>,
    C: FnOnce(&RuntimePaths, &str, &str, &str, Option<&str>) -> Result<ExitCode, String>,
{
    let (edge_origin, pairing_id) = parse_pairing_address(pairing_address)?;
    if let Some(device_id) = existing_enrollment_for_worker(config, &edge_origin)? {
        let snapshot = inventory()?;
        if !inventory_has_active_device(&snapshot, &device_id) {
            return Err(format!(
                "existing enrollment {device_id} is not active on Worker {edge_origin}; refusing to create a second device identity"
            ));
        }
        print_json(&json!({
            "ok": true,
            "action": "worker_connect",
            "device_id": device_id,
            "workstation_id": device_id,
            "edge_origin": edge_origin,
            "pairing_consumed": false,
            "reused_existing_enrollment": true,
            "secret_printed": false,
        }))?;
        return Ok(ExitCode::SUCCESS);
    }

    let code = read_code()?;
    connect_new(paths, &edge_origin, &pairing_id, &code, name)
}

#[cfg(target_os = "linux")]
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
        crate::credential_store::store,
        Config::load_for_instance,
        write_config_atomic,
        revoke_self,
        crate::credential_store::delete,
        activate_connected_runtime_after_pairing,
        |_paths| crate::linux_service_manager::reconcile_link(),
        move |_, _, _, _| Ok(enrolled.clone()),
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
        crate::credential_store::store,
        Config::load_for_instance,
        write_config_atomic,
        revoke_self,
        crate::credential_store::delete,
        activate_connected_runtime_after_pairing,
        crate::link::reconcile_after_service_generation_change,
        move |_, _, _, _| Ok(enrolled.clone()),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub(crate) fn adopt_bootstrap_enrollment(
    _paths: &RuntimePaths,
    _edge_origin: &str,
    _enrolled: EnrolledCredential,
) -> Result<ExitCode, String> {
    Err("first-Worker enrollment activation is currently supported on macOS and Linux".to_owned())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn revoke_device(_paths: &RuntimePaths, _device_id: &str) -> Result<ExitCode, String> {
    Err(
        "worker revoke is supported on macOS and Linux enrolled devices; this platform has no supported enrolled-device credential backend"
            .to_owned(),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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
    let response = client_for_origin(&identity.edge_origin)?
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

#[cfg(any(target_os = "macos", test))]
fn connector_service_ready(status: &Value) -> bool {
    status.get("ok").and_then(Value::as_bool) == Some(true)
        && status.get("loaded").and_then(Value::as_bool) == Some(true)
        && status.get("healthy").and_then(Value::as_bool) == Some(true)
}

#[cfg(target_os = "macos")]
fn ensure_connector_local_runtime_ready() -> Result<(), String> {
    let service = crate::service_manager::doctor_status()?;
    if !connector_service_ready(&service) {
        return Err(
            "local herdr-mcp service is not ready; run `herdr-mcp service start`, verify `herdr-mcp service status`, then retry this approval command"
                .to_owned(),
        );
    }
    if !crate::herdr_supervisor::connector_ready()? {
        return Err(
            "local Herdr server is not ready; run `herdr-mcp herdr-supervisor start`, verify `herdr-mcp herdr-supervisor status`, then retry this approval command"
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn ensure_connector_local_runtime_ready() -> Result<(), String> {
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn approve_connector(_paths: &RuntimePaths, _request_id: &str) -> Result<ExitCode, String> {
    Err(
        "connector approval is supported on macOS and Linux enrolled devices; this platform has no supported enrolled-device credential backend"
            .to_owned(),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn approve_connector(paths: &RuntimePaths, request_id: &str) -> Result<ExitCode, String> {
    if request_id.trim().is_empty() || request_id.len() > 256 {
        return Err("connector approval request id is invalid".to_owned());
    }
    ensure_connector_local_runtime_ready()?;
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let client = client_for_origin(&identity.edge_origin)?;
    let inspect = client
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
    let response = client
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

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn cancel_connector(_paths: &RuntimePaths, _request_id: &str) -> Result<ExitCode, String> {
    Err(
        "connector approval cancel is supported on macOS and Linux enrolled devices; this platform has no supported enrolled-device credential backend"
            .to_owned(),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn cancel_connector(paths: &RuntimePaths, request_id: &str) -> Result<ExitCode, String> {
    let request_id = request_id.trim();
    if request_id.is_empty() || request_id.len() > 256 {
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
    let response = client_for_origin(&identity.edge_origin)?
        .post(endpoint(&identity.edge_origin, "/connectors/cancel")?)
        .headers(headers)
        .json(&json!({ "request_id": request_id }))
        .send()
        .map_err(|error| format!("cannot cancel Connector approval: {error}"))?;
    let payload = parse_json_response(response, "connector approval cancel")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "connector_cancel",
            "request_id": request_id,
            "connector_deleted": payload.get("connector_deleted").cloned().unwrap_or(Value::Bool(false)),
        }))
        .map_err(|error| format!("cannot encode connector cancel result: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn revoke_connector(_paths: &RuntimePaths, _connector_id: &str) -> Result<ExitCode, String> {
    Err(
        "connector revoke is supported on macOS and Linux enrolled devices; this platform has no supported enrolled-device credential backend"
            .to_owned(),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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
    let response = client_for_origin(&identity.edge_origin)?
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

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn revoke_connector_client(_paths: &RuntimePaths, _client_id: &str) -> Result<ExitCode, String> {
    Err(
        "connector client revoke is supported on macOS and Linux enrolled devices; this platform has no supported enrolled-device credential backend"
            .to_owned(),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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
    let response = client_for_origin(&identity.edge_origin)?
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

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn connector_planner_control(
    _paths: &RuntimePaths,
    _action: &str,
    _request_id: Option<&str>,
) -> Result<ExitCode, String> {
    Err("planner control requires a supported enrolled-owner credential backend".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn connector_planner_control(
    paths: &RuntimePaths,
    action: &str,
    request_id: Option<&str>,
) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_owner_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client_for_origin(&identity.edge_origin)?
        .post(endpoint(
            &identity.edge_origin,
            "/connectors/planner-control",
        )?)
        .headers(headers)
        .json(&json!({ "action": action, "request_id": request_id }))
        .send()
        .map_err(|error| format!("cannot manage planner control: {error}"))?;
    let payload = parse_json_response(response, "planner control")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&payload)
            .map_err(|error| format!("cannot encode planner control result: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn set_connector_webchat_control(
    _paths: &RuntimePaths,
    _connector_id: &str,
    _device_id: &str,
    _endpoint_ref: &str,
    _provider: &str,
    _account_ref: &str,
    _allowed: bool,
) -> Result<ExitCode, String> {
    Err(
        "connector WebChat Control requires a supported enrolled-owner credential backend"
            .to_owned(),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn set_connector_webchat_control(
    paths: &RuntimePaths,
    connector_id: &str,
    device_id: &str,
    endpoint_ref: &str,
    provider: &str,
    account_ref: &str,
    allowed: bool,
) -> Result<ExitCode, String> {
    let connector_id = connector_id.trim();
    let device_id = crate::config::normalize_device_id(device_id)?;
    let endpoint_ref = endpoint_ref.trim();
    let provider = provider.trim();
    let account_ref = account_ref.trim();
    if !connector_id.starts_with("conn_")
        || connector_id.len() < 13
        || connector_id.len() > 133
        || !connector_id
            .chars()
            .skip(5)
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err("connector id is invalid".to_owned());
    }
    if endpoint_ref.is_empty()
        || endpoint_ref.len() > 96
        || endpoint_ref.chars().any(char::is_control)
    {
        return Err("browser endpoint ref is invalid".to_owned());
    }
    if account_ref.is_empty() || account_ref.len() > 96 || account_ref.chars().any(char::is_control)
    {
        return Err("browser account ref is invalid".to_owned());
    }
    if provider.is_empty()
        || provider.len() > 32
        || !provider.chars().enumerate().all(|(index, ch)| {
            if index == 0 {
                ch.is_ascii_lowercase() || ch.is_ascii_digit()
            } else {
                ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-')
            }
        })
    {
        return Err("browser provider is invalid".to_owned());
    }
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_owner_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client_for_origin(&identity.edge_origin)?
        .post(endpoint(
            &identity.edge_origin,
            "/connectors/webchat-control",
        )?)
        .headers(headers)
        .json(&json!({
            "connector_id": connector_id,
            "device_id": device_id,
            "endpoint_ref": endpoint_ref,
            "provider": provider,
            "account_ref": account_ref,
            "allowed": allowed,
        }))
        .send()
        .map_err(|error| format!("cannot change Connector WebChat Control grant: {error}"))?;
    let payload = parse_json_response(response, "connector WebChat Control")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "connector_webchat_control_set",
            "connector_id": connector_id,
            "device_id": device_id,
            "endpoint_ref": endpoint_ref,
            "provider": provider,
            "account_ref": account_ref,
            "allowed": payload.get("allowed").cloned().unwrap_or(Value::Bool(allowed)),
        }))
        .map_err(|error| format!("cannot encode Connector WebChat Control result: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn set_connector_page_assist(
    _paths: &RuntimePaths,
    _connector_id: &str,
    _device_id: &str,
    _endpoint_ref: &str,
    _allowed: bool,
) -> Result<ExitCode, String> {
    Err("connector Page Assist requires a supported enrolled-owner credential backend".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn set_connector_page_assist(
    paths: &RuntimePaths,
    connector_id: &str,
    device_id: &str,
    endpoint_ref: &str,
    allowed: bool,
) -> Result<ExitCode, String> {
    let connector_id = connector_id.trim();
    let device_id = crate::config::normalize_device_id(device_id)?;
    let endpoint_ref = endpoint_ref.trim();
    if !connector_id.starts_with("conn_")
        || connector_id.len() < 13
        || connector_id.len() > 133
        || !connector_id
            .chars()
            .skip(5)
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err("connector id is invalid".to_owned());
    }
    if endpoint_ref.is_empty()
        || endpoint_ref.len() > 96
        || endpoint_ref.chars().any(char::is_control)
    {
        return Err("browser endpoint ref is invalid".to_owned());
    }
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_owner_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client_for_origin(&identity.edge_origin)?
        .post(endpoint(&identity.edge_origin, "/connectors/page-assist")?)
        .headers(headers)
        .json(&json!({
            "connector_id": connector_id,
            "device_id": device_id,
            "endpoint_ref": endpoint_ref,
            "allowed": allowed,
        }))
        .send()
        .map_err(|error| format!("cannot change Connector Page Assist grant: {error}"))?;
    let payload = parse_json_response(response, "connector Page Assist")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "connector_page_assist_set",
            "connector_id": connector_id,
            "device_id": device_id,
            "endpoint_ref": endpoint_ref,
            "allowed": payload.get("allowed").cloned().unwrap_or(Value::Bool(allowed)),
        }))
        .map_err(|error| format!("cannot encode Connector Page Assist result: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn list_connectors(_paths: &RuntimePaths, _include_all: bool) -> Result<ExitCode, String> {
    Err(
        "connector inventory is supported on macOS and Linux enrolled devices; this platform has no supported enrolled-device credential backend"
            .to_owned(),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn list_connectors(paths: &RuntimePaths, include_all: bool) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client_for_origin(&identity.edge_origin)?
        .get(endpoint(&identity.edge_origin, "/connectors")?)
        .headers(headers)
        .send()
        .map_err(|error| format!("cannot list Connectors: {error}"))?;
    let payload = render_connector_inventory(
        parse_json_response(response, "connector inventory")?,
        inventory_now_ms(),
        include_all,
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&payload)
            .map_err(|error| format!("cannot encode connector inventory: {error}"))?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn create_automation(
    _paths: &RuntimePaths,
    _name: &str,
    _device: &str,
) -> Result<ExitCode, String> {
    Err("automation credential provisioning is supported on macOS and Linux enrolled devices; this platform has no supported enrolled-device credential backend".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn create_automation(paths: &RuntimePaths, name: &str, device: &str) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client_for_origin(&identity.edge_origin)?
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

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn list_automations(_paths: &RuntimePaths) -> Result<ExitCode, String> {
    Err("automation credential inventory is supported on macOS and Linux enrolled devices; this platform has no supported enrolled-device credential backend".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn list_automations(paths: &RuntimePaths) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client_for_origin(&identity.edge_origin)?
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

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn rotate_automation(_paths: &RuntimePaths, _client_id: &str) -> Result<ExitCode, String> {
    Err("automation credential rotation is supported on macOS and Linux enrolled devices; this platform has no supported enrolled-device credential backend".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn rotate_automation(paths: &RuntimePaths, client_id: &str) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client_for_origin(&identity.edge_origin)?
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

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn revoke_automation(_paths: &RuntimePaths, _client_id: &str) -> Result<ExitCode, String> {
    Err("automation credential revoke is supported on macOS and Linux enrolled devices; this platform has no supported enrolled-device credential backend".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn revoke_automation(paths: &RuntimePaths, client_id: &str) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_fleet_link_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client_for_origin(&identity.edge_origin)?
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

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn rename_current_device(_paths: &RuntimePaths, _name: &str) -> Result<ExitCode, String> {
    Err(
        "worker rename is supported on macOS and Linux enrolled devices; this platform has no supported enrolled-device credential backend"
            .to_owned(),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn rename_current_device(paths: &RuntimePaths, name: &str) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let identity = resolve_enrolled_device_identity(paths, &config)?;
    let mut headers = bearer_headers(&identity.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&identity.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client_for_origin(&identity.edge_origin)?
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

#[cfg(target_os = "linux")]
fn activate_connected_runtime_after_pairing(
    paths: &RuntimePaths,
    _recovered_existing: bool,
    _device_id: &str,
    _keychain_service: &str,
) -> Result<(), String> {
    activate_connected_runtime(paths)
}

#[cfg(target_os = "macos")]
fn activate_connected_runtime_after_pairing(
    paths: &RuntimePaths,
    recovered_existing: bool,
    device_id: &str,
    keychain_service: &str,
) -> Result<(), String> {
    // Ordinary generation refresh deliberately preserves the current Link
    // credential owner. Exact-device recovery is an explicit enrollment
    // transition, so only this path may switch an existing owner Link to the
    // freshly recovered device credential before normal activation checks.
    if recovered_existing {
        crate::link::switch_prod_link_credential_service(paths, device_id, keychain_service)?;
    }
    activate_connected_runtime(paths)
}

#[cfg(target_os = "linux")]
fn activate_connected_runtime(_paths: &RuntimePaths) -> Result<(), String> {
    let code = crate::service_lifecycle::run(ServiceCommand::Install { adopt_node: false })?;
    if code != ExitCode::SUCCESS {
        return Err(format!(
            "Linux service install returned a non-success exit code: {code:?}"
        ));
    }
    let service = crate::linux_service_manager::doctor_status()?;
    if service.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(
            "herdr-mcp Linux service is not healthy after worker connect activation".to_owned(),
        );
    }
    crate::linux_service_manager::ensure_link_installed()?;
    Ok(())
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct ReconcileRollbackEvidence {
    revoked: bool,
    keychain_deleted: bool,
    config_restored: bool,
    link_reconciled: bool,
    restore_error: Option<String>,
    reconcile_error: Option<String>,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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
    K: Fn(&RuntimePaths, bool, &str, &str) -> Result<(), String>,
    L: Fn(&RuntimePaths) -> Result<(), String>,
    M: Fn(&str, &str, &str, Option<&str>) -> Result<EnrolledCredential, String>,
{
    // Snapshot the local binding before consuming the one-time pairing. The
    // server marker is authoritative, while the same-device local binding is
    // a mixed-version safety fence: an existing device must never be revoked
    // merely because an older Edge omitted recovered_existing.
    let previous_config_result = load_config(&paths.config_file, &paths.instance);
    let enrolled = consume(edge_origin, pairing_id, code, name)?;
    let device_id = crate::config::normalize_device_id(&enrolled.device_id)?;
    let recovered_existing = enrolled.recovered_existing
        || previous_config_result
            .as_ref()
            .ok()
            .and_then(|config| config.edge_device_id.as_deref())
            == Some(device_id.as_str());
    if enrolled.workstation_id != device_id {
        if !recovered_existing {
            let _ = revoke_fn(
                edge_origin,
                &enrolled.workstation_id,
                &enrolled.device_secret,
            );
        }
        return Err(
            "Worker returned a workstation identity that does not match the immutable device_id"
                .to_owned(),
        );
    }
    if let Err(error) = validate_device_secret(&enrolled.device_secret) {
        if !recovered_existing {
            let _ = revoke_fn(edge_origin, &device_id, &enrolled.device_secret);
        }
        return Err(error);
    }

    let account = match current_account() {
        Ok(a) => a,
        Err(error) => {
            if !recovered_existing {
                let _ = revoke_fn(edge_origin, &device_id, &enrolled.device_secret);
            }
            return Err(error);
        }
    };
    let keychain_service = format!("herdr-edge-link-{device_id}");
    if let Err(error) = store_secret(&keychain_service, &account, &enrolled.device_secret) {
        if recovered_existing {
            return Err(format!(
                "cannot persist the recovered credential for existing device {device_id}; the device was not revoked, but its Worker verifier has rotated, so create another recovery pairing: {error}"
            ));
        }
        let revoked = revoke_fn(edge_origin, &device_id, &enrolled.device_secret).unwrap_or(false);
        return Err(format!(
            "cannot persist the new device credential; remote compensation revoked={revoked}: {error}"
        ));
    }

    // New-device enrollment failures after durable secret storage revoke the
    // newly-created remote device and remove its local credential. Existing-
    // device recovery is different: the remote identity predates this attempt,
    // so failures preserve the rotated credential and never revoke the device.
    // Any failure after the secret is durably stored must revoke the remote device
    // and delete the local Keychain credential to avoid orphans. No secret is ever
    // printed in the error.
    // Retain the previous local binding so a post-write failure can roll the
    // transaction back instead of leaving config/plist bound to a revoked device.
    let previous_config = match previous_config_result {
        Ok(c) => c,
        Err(error) => {
            if recovered_existing {
                return Err(format!(
                    "local config unavailable after credential recovery for existing device {device_id}; the device was not revoked and the recovered credential was preserved: {error}"
                ));
            }
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
        if recovered_existing {
            return Err(format!(
                "config origin update failed after credential recovery for existing device {device_id}; the device was not revoked and the recovered credential was preserved: {error}"
            ));
        }
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
        if recovered_existing {
            return Err(format!(
                "config device update failed after credential recovery for existing device {device_id}; the device was not revoked and the recovered credential was preserved: {error}"
            ));
        }
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
        if recovered_existing {
            return Err(format!(
                "config write failed after credential recovery for existing device {device_id}; the device was not revoked and the recovered credential was preserved: {error}"
            ));
        }
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

    if let Err(error) = activate(paths, recovered_existing, &device_id, &keychain_service) {
        if recovered_existing {
            return Err(format!(
                "existing device {device_id} credential recovery was persisted, but the local runtime could not be activated: {error}; the device was not revoked and the recovered credential/config were preserved for repair"
            ));
        }
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
        "recovered_existing": recovered_existing,
        "secret_printed": false,
        "service_ready": true,
        "link_ready": true,
        "link_reconciled": true,
    }))?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn consume_pairing(
    edge_origin: &str,
    pairing_id: &str,
    code: &str,
    name: Option<&str>,
) -> Result<EnrolledCredential, String> {
    let response = client_for_origin(edge_origin)?
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
        recovered_existing: payload
            .get("recovered_existing")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn revoke_self(edge_origin: &str, workstation_id: &str, credential: &str) -> Result<bool, String> {
    let response = client_for_origin(edge_origin)?
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
    prefer_fleet_admin_identity(resolve_enrolled_device_identity(paths, config), || {
        resolve_owner_link_identity(paths, config)
    })
}

#[cfg(target_os = "linux")]
fn resolve_fleet_link_identity(
    paths: &RuntimePaths,
    config: &Config,
) -> Result<FleetLinkIdentity, String> {
    resolve_enrolled_device_identity(paths, config)
}

#[cfg(any(target_os = "macos", test))]
fn prefer_fleet_admin_identity<T, F>(enrolled: Result<T, String>, owner: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    match enrolled {
        Ok(identity) => Ok(identity),
        Err(enrolled_error) => owner().map_err(|owner_error| {
            format!(
                "fleet administration identity unavailable: enrolled-device path failed: {enrolled_error}; production Link fallback failed: {owner_error}"
            )
        }),
    }
}

#[cfg(target_os = "macos")]
fn resolve_owner_link_identity(
    _paths: &RuntimePaths,
    config: &Config,
) -> Result<FleetLinkIdentity, String> {
    let plist_env = production_link_environment_if_present()?
        .ok_or_else(|| "owner authority requires the production Herdr Link identity".to_owned())?;
    let (workstation_id, credential_service, edge_origin) =
        resolve_owner_link_fields(config, &plist_env)?;
    let account = current_account()?;
    let credential = crate::credential_store::load(&credential_service, &account)
        .map_err(|error| format!("owner credential is unavailable: {error}"))?;
    Ok(FleetLinkIdentity {
        edge_origin,
        workstation_id,
        credential,
    })
}

#[cfg(target_os = "linux")]
fn resolve_owner_link_identity(
    paths: &RuntimePaths,
    config: &Config,
) -> Result<FleetLinkIdentity, String> {
    // Linux has no macOS LaunchAgent owner tuple to recover locally. Present
    // the exact enrolled-device identity and let Edge's DEFAULT_WORKSTATION_ID
    // gate decide whether this device is the configured owner.
    resolve_enrolled_device_identity(paths, config)
}

#[cfg(target_os = "macos")]
fn resolve_enrolled_device_identity(
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
    let (workstation_id, credential_service, edge_origin) =
        resolve_fleet_link_fields(config, plist_env.as_ref())?;
    let account = current_account()?;
    let credential =
        crate::credential_store::load(&credential_service, &account).map_err(|error| {
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

#[cfg(target_os = "linux")]
fn resolve_enrolled_device_identity(
    paths: &RuntimePaths,
    config: &Config,
) -> Result<FleetLinkIdentity, String> {
    let (workstation_id, credential_service, edge_origin) =
        resolve_fleet_link_fields(config, None)?;
    let account = current_account()?;
    let credential =
        crate::credential_store::load(&credential_service, &account).map_err(|error| {
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

#[cfg(target_os = "windows")]
fn resolve_fleet_link_identity(
    paths: &RuntimePaths,
    config: &Config,
) -> Result<FleetLinkIdentity, String> {
    let (workstation_id, credential_service, edge_origin) =
        resolve_fleet_link_fields(config, None)?;
    let account = current_account()?;
    let credential =
        crate::credential_store::load(&credential_service, &account).map_err(|error| {
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
fn resolve_owner_link_fields(
    config: &Config,
    plist_env: &std::collections::BTreeMap<String, String>,
) -> Result<(String, String, String), String> {
    let workstation_id = plist_env
        .get("HERDR_WORKSTATION_ID")
        .map(|value| value.trim())
        .filter(|value| {
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| "production Herdr Link has no valid owner workstation identity".to_owned())?
        .to_owned();
    let credential_service = plist_env
        .get("HERDR_LINK_KEYCHAIN_SERVICE")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "production Herdr Link has no credential service".to_owned())?;
    let device_credential_service = config.edge_link_keychain_service();
    let uses_matching_device_credential = config.edge_device_id.as_deref()
        == Some(workstation_id.as_str())
        && device_credential_service.as_deref() == Some(credential_service);
    if credential_service != LEGACY_LINK_KEYCHAIN_SERVICE && !uses_matching_device_credential {
        return Err(
            "production Herdr Link does not use the stable owner credential service".to_owned(),
        );
    }
    let edge_origin = match config.edge_public_origin.clone() {
        Some(origin) => normalize_edge_origin(&origin)?,
        None => {
            let edge_url = plist_env.get("HERDR_EDGE_URL").ok_or_else(|| {
                "production Herdr Link has no Edge origin for owner authority".to_owned()
            })?;
            origin_from_ws_url(edge_url)?
        }
    };
    Ok((workstation_id, credential_service.to_owned(), edge_origin))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
fn enrolled_device_required_error(reason: &str) -> String {
    format!(
        "This fleet operation requires credentials for a device already enrolled in the target Herdr Worker; {reason}. \
Herdr devices enrolled in the same Worker have no owner/member hierarchy for fleet administration. \
On a new computer joining an existing fleet, use `herdr-mcp worker connect <pairing-address>` with a pairing created by any enrolled device or an explicitly approved WebChat. \
If this is the first Herdr device, complete the first-Worker Cloudflare bootstrap before using pairing or Connector administration."
    )
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn client_for_origin(edge_origin: &str) -> Result<Client, String> {
    crate::worker_bootstrap::client_for_edge_origin(edge_origin)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn endpoint(origin: &str, path: &str) -> Result<Url, String> {
    let mut url = Url::parse(&normalize_edge_origin(origin)?)
        .map_err(|error| format!("invalid Worker origin: {error}"))?;
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn normalize_edge_origin(value: &str) -> Result<String, String> {
    let mut config = Config::default();
    config.set_edge_public_origin(value)?;
    config
        .edge_public_origin
        .ok_or_else(|| "Worker origin is missing".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn required_string(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(|| format!("Worker response is missing {key}"))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn validate_pairing_code(value: &str) -> Result<(), String> {
    if value.len() == 6 && value.chars().all(|ch| ch.is_ascii_digit()) {
        Ok(())
    } else {
        Err("pairing code must be exactly six decimal digits".to_owned())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn format_pairing_code(code: &str) -> String {
    if code.len() == 6 {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_owned()
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn current_account() -> Result<String, String> {
    let variable = if cfg!(windows) { "USERNAME" } else { "USER" };
    env::var(variable)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| {
            !value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| format!("{variable} is required for enrolled-device credentials"))
}

#[cfg(test)]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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
    fn unsupported_linux_subenvironments_allow_only_worker_inventory_reads() {
        assert!(!worker_command_requires_supported_workstation(
            &WorkerCommand::List
        ));
        assert!(!worker_command_requires_supported_workstation(
            &WorkerCommand::ConnectorList { include_all: false }
        ));
        assert!(!worker_command_requires_supported_workstation(
            &WorkerCommand::AutomationList
        ));
        assert!(worker_command_requires_supported_workstation(
            &WorkerCommand::Connect {
                pairing_address: "https://edge.example/pair#pair_test".to_owned(),
                name: None,
            }
        ));
        assert!(worker_command_requires_supported_workstation(
            &WorkerCommand::Pair {
                ttl_seconds: 600,
                name: None,
                recover_device_id: None,
            }
        ));
        assert!(worker_command_requires_supported_workstation(
            &WorkerCommand::ConnectorApprove {
                request_id: "request_test".to_owned(),
            }
        ));
        assert!(worker_command_requires_supported_workstation(
            &WorkerCommand::AutomationCreate {
                name: "ci".to_owned(),
                device: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            }
        ));
    }

    #[test]
    fn device_secret_verifier_is_lowercase_sha256_without_exposing_secret() {
        let secret = "devsec_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let verifier = device_secret_verifier(secret);
        assert_eq!(verifier.len(), 64);
        assert!(
            verifier
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        assert_ne!(verifier, secret);
        assert_eq!(
            verifier,
            "639b0a222f754d2ada9b701c05d329e84424ca54f4bb1dc82feab6f42f6daf3f"
        );
    }

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
    fn fleet_admin_identity_prefers_enrolled_and_falls_back_to_owner() {
        let mut owner_called = false;
        let enrolled = prefer_fleet_admin_identity(Ok("device"), || {
            owner_called = true;
            Ok("owner")
        })
        .unwrap();
        assert_eq!(enrolled, "device");
        assert!(!owner_called);

        let fallback =
            prefer_fleet_admin_identity(Err::<&str, _>("device missing".to_owned()), || {
                Ok("owner")
            })
            .unwrap();
        assert_eq!(fallback, "owner");

        let error =
            prefer_fleet_admin_identity(Err::<&str, _>("device missing".to_owned()), || {
                Err("owner missing".to_owned())
            })
            .unwrap_err();
        assert!(error.contains("device missing"));
        assert!(error.contains("owner missing"));
    }

    #[test]
    fn owner_control_uses_production_owner_identity_not_enrolled_device_identity() {
        let mut config = Config::default();
        config
            .set_edge_device_id("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV")
            .unwrap();
        config
            .set_edge_public_origin("https://edge.example")
            .unwrap();
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            "HERDR_WORKSTATION_ID".to_owned(),
            "prod-real-runtime".to_owned(),
        );
        env.insert(
            "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
            LEGACY_LINK_KEYCHAIN_SERVICE.to_owned(),
        );
        env.insert(
            "HERDR_EDGE_URL".to_owned(),
            "wss://edge.example/ws".to_owned(),
        );

        let (workstation_id, credential_service, origin) =
            resolve_owner_link_fields(&config, &env).unwrap();
        assert_eq!(workstation_id, "prod-real-runtime");
        assert_eq!(credential_service, LEGACY_LINK_KEYCHAIN_SERVICE);
        assert_eq!(origin, "https://edge.example");
    }

    #[test]
    fn owner_control_accepts_only_matching_device_credential_after_repair() {
        let device_id = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let credential_service = format!("herdr-edge-link-{device_id}");
        let mut config = Config::default();
        config.set_edge_device_id(device_id).unwrap();
        config
            .set_edge_public_origin("https://edge.example")
            .unwrap();
        let mut env = std::collections::BTreeMap::from([
            ("HERDR_WORKSTATION_ID".to_owned(), device_id.to_owned()),
            (
                "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
                credential_service.clone(),
            ),
            (
                "HERDR_EDGE_URL".to_owned(),
                "wss://edge.example/ws".to_owned(),
            ),
        ]);

        let resolved = resolve_owner_link_fields(&config, &env).unwrap();
        assert_eq!(resolved.0, device_id);
        assert_eq!(resolved.1, credential_service);
        assert_eq!(resolved.2, "https://edge.example");

        env.insert(
            "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
            "herdr-edge-link-dev_01ARZ3NDEKTSV4RRFFQ69G5FAW".to_owned(),
        );
        assert!(resolve_owner_link_fields(&config, &env).is_err());
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
                    "grant_origin": "explicit_approval",
                    "status": "active"
                }],
                "legacy_clients": [{
                    "client_id": "legacy-1",
                    "client_name": "Old client",
                    "created_at_ms": two_days_ago,
                    "last_used_at_ms": null,
                    "grant_origin": "pre_v0_4_6_legacy",
                    "registration_state": "active_credentials"
                }]
            }),
            now,
            false,
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

        let inventory = json!({
            "token_counts": {"active_access": 6, "active_refresh": 10},
            "connectors": [
                {"connector_id": "conn_active123", "status": "active", "active_access_tokens": 1, "active_refresh_tokens": 2},
                {"connector_id": "conn_revoked123", "status": "revoked", "active_access_tokens": 2, "active_refresh_tokens": 3}
            ],
            "legacy_clients": [
                {"client_id": "legacy-active", "registration_state": "active_credentials", "active_access_tokens": 1, "active_refresh_tokens": 1},
                {"client_id": "legacy-revoked", "registration_state": "revoked", "active_access_tokens": 2, "active_refresh_tokens": 4}
            ]
        });
        let filtered = render_connector_inventory(inventory.clone(), now, false);
        assert_eq!(filtered["connectors"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["legacy_clients"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["inventory_filter"], "actionable");
        assert_eq!(filtered["token_counts_scope"], "all_stored");
        assert_eq!(filtered["token_counts"]["active_access"], 6);
        assert_eq!(filtered["token_counts"]["active_refresh"], 10);
        assert_eq!(filtered["listed_token_counts"]["active_access"], 2);
        assert_eq!(filtered["listed_token_counts"]["active_refresh"], 3);
        assert_eq!(filtered["listed_token_counts_scope"], "displayed_records");

        let all = render_connector_inventory(inventory, now, true);
        assert_eq!(all["connectors"].as_array().unwrap().len(), 2);
        assert_eq!(all["legacy_clients"].as_array().unwrap().len(), 2);
        assert_eq!(all["inventory_filter"], "all");
        assert_eq!(all["listed_token_counts"]["active_access"], 6);
        assert_eq!(all["listed_token_counts"]["active_refresh"], 10);

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
        let unnamed_create = pairing_create_request_body(600, None, None);
        assert_eq!(unnamed_create["ttl_seconds"], 600);
        assert!(unnamed_create.get("name").is_none());

        let named_create = pairing_create_request_body(600, Some("Nathan Mac"), None);
        assert_eq!(named_create["name"], "Nathan Mac");

        let recovery_create =
            pairing_create_request_body(600, None, Some("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert_eq!(
            recovery_create["recover_device_id"],
            "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );

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
    fn same_worker_active_enrollment_reuses_identity_without_reading_code_or_consuming_pairing() {
        use std::cell::Cell;

        const DEVICE_ID: &str = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let mut config = Config::default();
        config.set_edge_device_id(DEVICE_ID).unwrap();
        config
            .set_edge_public_origin("https://edge.example")
            .unwrap();
        let paths = RuntimePaths {
            config_dir: env::temp_dir(),
            config_file: env::temp_dir().join("herdr-worker-reuse.toml"),
            dev_state_dir: env::temp_dir().join("herdr-worker-reuse-dev"),
            herdr_socket: None,
            instance: InstanceId::default_instance(),
        };
        let read_code_called = Cell::new(false);
        let connect_new_called = Cell::new(false);

        let result = connect_existing_worker_flow(
            &paths,
            "https://edge.example/pair#pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("ignored-name"),
            &config,
            || {
                Ok(json!({
                    "ok": true,
                    "devices": [{"device_id": DEVICE_ID, "authorization": "active"}]
                }))
            },
            || {
                read_code_called.set(true);
                Ok("123456".to_owned())
            },
            |_, _, _, _, _| {
                connect_new_called.set(true);
                Ok(ExitCode::SUCCESS)
            },
        );

        assert_eq!(result.unwrap(), ExitCode::SUCCESS);
        assert!(!read_code_called.get());
        assert!(!connect_new_called.get());
    }

    #[test]
    fn same_worker_stale_enrollment_fails_closed_without_consuming_pairing() {
        const DEVICE_ID: &str = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let mut config = Config::default();
        config.set_edge_device_id(DEVICE_ID).unwrap();
        config
            .set_edge_public_origin("https://edge.example/")
            .unwrap();
        let paths = RuntimePaths {
            config_dir: env::temp_dir(),
            config_file: env::temp_dir().join("herdr-worker-stale.toml"),
            dev_state_dir: env::temp_dir().join("herdr-worker-stale-dev"),
            herdr_socket: None,
            instance: InstanceId::default_instance(),
        };

        let error = connect_existing_worker_flow(
            &paths,
            "https://edge.example/pair#pair_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            None,
            &config,
            || {
                Ok(json!({
                    "ok": true,
                    "devices": [{"device_id": DEVICE_ID, "authorization": "revoked"}]
                }))
            },
            || panic!("same-Worker stale enrollment must not read a new pairing code"),
            |_, _, _, _, _| panic!("same-Worker stale enrollment must not consume a pairing"),
        )
        .unwrap_err();

        assert!(error.contains("refusing to create a second device identity"));
        assert!(error.contains(DEVICE_ID));
    }

    #[test]
    fn different_worker_keeps_explicit_pairing_path() {
        const DEVICE_ID: &str = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let mut config = Config::default();
        config.set_edge_device_id(DEVICE_ID).unwrap();
        config
            .set_edge_public_origin("https://old-edge.example")
            .unwrap();
        let paths = RuntimePaths {
            config_dir: env::temp_dir(),
            config_file: env::temp_dir().join("herdr-worker-other-edge.toml"),
            dev_state_dir: env::temp_dir().join("herdr-worker-other-edge-dev"),
            herdr_socket: None,
            instance: InstanceId::default_instance(),
        };
        let connect_new_called = std::cell::Cell::new(false);

        let result = connect_existing_worker_flow(
            &paths,
            "https://new-edge.example/pair#pair_cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            Some("new-worker-device"),
            &config,
            || panic!("different Worker must not query the old Worker inventory"),
            || Ok("654321".to_owned()),
            |_, origin, pairing_id, code, name| {
                assert_eq!(origin, "https://new-edge.example");
                assert_eq!(
                    pairing_id,
                    "pair_cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                );
                assert_eq!(code, "654321");
                assert_eq!(name, Some("new-worker-device"));
                connect_new_called.set(true);
                Ok(ExitCode::SUCCESS)
            },
        );

        assert_eq!(result.unwrap(), ExitCode::SUCCESS);
        assert!(connect_new_called.get());
    }

    #[test]
    fn connector_approval_requires_loaded_healthy_local_service() {
        assert!(connector_service_ready(&json!({
            "ok": true,
            "loaded": true,
            "healthy": true,
        })));
        for status in [
            json!({"ok": false, "loaded": true, "healthy": true}),
            json!({"ok": true, "loaded": false, "healthy": true}),
            json!({"ok": true, "loaded": true, "healthy": false}),
            json!({"ok": true}),
        ] {
            assert!(!connector_service_ready(&status));
        }
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
        let activate = move |_paths: &RuntimePaths,
                             _recovered_existing: bool,
                             _device_id: &str,
                             _keychain_service: &str|
              -> Result<(), String> {
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
                recovered_existing: false,
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
    fn recovered_existing_store_failure_never_revokes_device() {
        use std::cell::Cell;
        use std::rc::Rc;

        const DEVICE_ID: &str = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        const DEVICE_SECRET: &str =
            "devsec_dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
        let dir = env::temp_dir().join(format!(
            "herdr-worker-recovery-store-failure-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let paths = crate::paths::RuntimePaths {
            config_dir: dir.clone(),
            config_file: dir.join("config.toml"),
            dev_state_dir: dir.join("dev-state"),
            herdr_socket: None,
            instance: InstanceId::default_instance(),
        };
        let previous_config = Config {
            edge_public_origin: Some("https://edge.example".to_owned()),
            edge_device_id: Some(DEVICE_ID.to_owned()),
            ..Config::default()
        };
        let revoke_calls = Rc::new(Cell::new(0_u32));
        let delete_calls = Rc::new(Cell::new(0_u32));
        let revoke_calls_hook = revoke_calls.clone();
        let delete_calls_hook = delete_calls.clone();
        let load_config = move |_: &Path, _: &InstanceId| Ok(previous_config.clone());
        let store_secret = |_: &str, _: &str, _: &str| -> Result<(), String> {
            Err("simulated Keychain write failure".to_owned())
        };
        let write_config = |_: &RuntimePaths, _: &Config| Ok(());
        let revoke = move |_: &str, _: &str, _: &str| -> Result<bool, String> {
            revoke_calls_hook.set(revoke_calls_hook.get() + 1);
            Ok(true)
        };
        let delete = move |_: &str, _: &str| -> Result<(), String> {
            delete_calls_hook.set(delete_calls_hook.get() + 1);
            Ok(())
        };
        let activate = |_: &RuntimePaths, _: bool, _: &str, _: &str| Ok(());
        let reconcile = |_: &RuntimePaths| Ok(());
        let consume = |_: &str, _: &str, _: &str, _: Option<&str>| {
            Ok(EnrolledCredential {
                device_id: DEVICE_ID.to_owned(),
                workstation_id: DEVICE_ID.to_owned(),
                device_secret: DEVICE_SECRET.to_owned(),
                recovered_existing: true,
            })
        };

        let error = connect_macos_inner(
            &paths,
            "https://edge.example",
            "pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "000000",
            None,
            store_secret,
            load_config,
            write_config,
            revoke,
            delete,
            activate,
            reconcile,
            consume,
        )
        .unwrap_err();

        assert!(error.contains("device was not revoked"));
        assert!(error.contains("another recovery pairing"));
        assert!(!error.contains("devsec_"));
        assert_eq!(revoke_calls.get(), 0);
        assert_eq!(delete_calls.get(), 0);
    }

    #[test]
    fn recovered_existing_activation_failure_preserves_device_and_credential() {
        use std::cell::Cell;
        use std::rc::Rc;

        const DEVICE_ID: &str = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        const DEVICE_SECRET: &str =
            "devsec_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let dir = env::temp_dir().join(format!(
            "herdr-worker-recovery-activation-failure-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let paths = crate::paths::RuntimePaths {
            config_dir: dir.clone(),
            config_file: dir.join("config.toml"),
            dev_state_dir: dir.join("dev-state"),
            herdr_socket: None,
            instance: InstanceId::default_instance(),
        };
        let previous_config = Config {
            edge_public_origin: Some("https://edge.example".to_owned()),
            edge_device_id: Some(DEVICE_ID.to_owned()),
            ..Config::default()
        };
        let store_calls = Rc::new(Cell::new(0_u32));
        let revoke_calls = Rc::new(Cell::new(0_u32));
        let delete_calls = Rc::new(Cell::new(0_u32));
        let reconcile_calls = Rc::new(Cell::new(0_u32));
        let store_hook = store_calls.clone();
        let revoke_hook = revoke_calls.clone();
        let delete_hook = delete_calls.clone();
        let reconcile_hook = reconcile_calls.clone();
        let load_config = move |_: &Path, _: &InstanceId| Ok(previous_config.clone());
        let store_secret = move |service: &str, _: &str, secret: &str| -> Result<(), String> {
            assert_eq!(service, format!("herdr-edge-link-{DEVICE_ID}"));
            assert_eq!(secret, DEVICE_SECRET);
            store_hook.set(store_hook.get() + 1);
            Ok(())
        };
        let write_config = |_: &RuntimePaths, config: &Config| -> Result<(), String> {
            assert_eq!(config.edge_device_id.as_deref(), Some(DEVICE_ID));
            Ok(())
        };
        let revoke = move |_: &str, _: &str, _: &str| -> Result<bool, String> {
            revoke_hook.set(revoke_hook.get() + 1);
            Ok(true)
        };
        let delete = move |_: &str, _: &str| -> Result<(), String> {
            delete_hook.set(delete_hook.get() + 1);
            Ok(())
        };
        let activate = |_: &RuntimePaths,
                        recovered_existing: bool,
                        device_id: &str,
                        keychain_service: &str|
         -> Result<(), String> {
            assert!(recovered_existing);
            assert_eq!(device_id, DEVICE_ID);
            assert_eq!(keychain_service, format!("herdr-edge-link-{DEVICE_ID}"));
            Err("simulated activation failure".to_owned())
        };
        let reconcile = move |_: &RuntimePaths| -> Result<(), String> {
            reconcile_hook.set(reconcile_hook.get() + 1);
            Ok(())
        };
        let consume = |_: &str, _: &str, _: &str, _: Option<&str>| {
            Ok(EnrolledCredential {
                device_id: DEVICE_ID.to_owned(),
                workstation_id: DEVICE_ID.to_owned(),
                device_secret: DEVICE_SECRET.to_owned(),
                // Deliberately false: the local same-device binding is the
                // mixed-version safety fence when an older Edge omits the bit.
                recovered_existing: false,
            })
        };

        let error = connect_macos_inner(
            &paths,
            "https://edge.example",
            "pair_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "000000",
            None,
            store_secret,
            load_config,
            write_config,
            revoke,
            delete,
            activate,
            reconcile,
            consume,
        )
        .unwrap_err();

        assert!(error.contains("device was not revoked"));
        assert!(error.contains("credential/config were preserved"));
        assert!(!error.contains("devsec_"));
        assert_eq!(store_calls.get(), 1);
        assert_eq!(revoke_calls.get(), 0);
        assert_eq!(delete_calls.get(), 0);
        assert_eq!(reconcile_calls.get(), 0);
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
        let activate =
            |_: &RuntimePaths, _: bool, _: &str, _: &str| -> Result<(), String> { Ok(()) };
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
                recovered_existing: false,
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
