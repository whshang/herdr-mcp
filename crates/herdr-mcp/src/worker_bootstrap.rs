use crate::paths::RuntimePaths;
use crate::release_trust;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::io::IsTerminal;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Stdio;
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CLOUDFLARE_API: &str = "https://api.cloudflare.com/client/v4";
const CLOUDFLARE_DEVICE_AUTH: &str = "https://dash.cloudflare.com/oauth2/device/auth";
const CLOUDFLARE_TOKEN: &str = "https://dash.cloudflare.com/oauth2/token";
const CLOUDFLARE_CLIENT_ID: &str = "54d11594-84e4-41aa-b438-e81b8fa78ee7";
const CLOUDFLARE_SCOPES: &str = "account:read user:read workers_scripts:write offline_access";
const EDGE_COMPATIBILITY_DATE: &str = "2024-09-23";
const EDGE_MAIN_MODULE: &str = "herdr-edge.mjs";
const EDGE_CRON: &str = "*/10 * * * *";
const EDGE_MANIFEST_MAX_BYTES: usize = 1024 * 1024;
const EDGE_BUNDLE_MAX_BYTES: usize = 64 * 1024 * 1024;
const EDGE_ATTESTATION_MAX_BYTES: usize = 2 * 1024 * 1024;
const JOURNAL_SCHEMA: u32 = 1;
const JOURNAL_FILE: &str = "first-worker-v1.json";
const TEMP_OPERATOR_SECRET: &str = "STATIC_MCP_BEARER_SECRET";
const TEMP_OPERATOR_EXPIRY_SECRET: &str = "STATIC_MCP_BEARER_EXPIRES_AT_MS";
const PAIRING_PEPPER_SECRET: &str = "LINK_SHARED_SECRET";
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
const DEVICE_FLOW_MAX: Duration = Duration::from_secs(300);
const TEMP_OPERATOR_TTL: Duration = Duration::from_secs(10 * 60);
const CLOUDFLARE_DOH_HOST: &str = "cloudflare-dns.com";
const GOOGLE_DOH_HOST: &str = "dns.google";
const MANAGED_HOSTS_MARKER: &str = "# herdr-mcp workers.dev";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Classified,
    SubdomainReady,
    WorkerDeploying,
    WorkerDeployed,
    SecretsProvisioned,
    DeviceEnrolled,
    OperatorRemoved,
    OperationalReady,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BootstrapJournal {
    schema: u32,
    phase: Phase,
    source_commit: String,
    runtime_version: String,
    account_id: String,
    worker_name: String,
    workers_dev_origin: Option<String>,
    canonical_device_id: Option<String>,
    expected_runtime_generation: Option<String>,
    updated_at_ms: u64,
}

impl BootstrapJournal {
    fn new(
        source_commit: String,
        runtime_version: String,
        account_id: String,
        worker_name: String,
    ) -> Self {
        Self {
            schema: JOURNAL_SCHEMA,
            phase: Phase::Classified,
            source_commit,
            runtime_version,
            account_id,
            worker_name,
            workers_dev_origin: None,
            canonical_device_id: None,
            expected_runtime_generation: current_runtime_generation(),
            updated_at_ms: now_ms(),
        }
    }

    fn advance(&mut self, phase: Phase) {
        if phase > self.phase {
            self.phase = phase;
        }
        self.updated_at_ms = now_ms();
    }
}

struct SecretBytes(Vec<u8>);

impl SecretBytes {
    fn from_string(value: String) -> Result<Self, String> {
        if value.trim().is_empty() {
            return Err("credential is empty".to_owned());
        }
        Ok(Self(value.into_bytes()))
    }

    fn expose(&self) -> Result<&str, String> {
        std::str::from_utf8(&self.0).map_err(|_| "credential is not UTF-8".to_owned())
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBytes([REDACTED])")
    }
}

#[derive(Deserialize)]
struct DeviceFlowAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: Option<u64>,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct DeviceFlowTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct Account {
    id: String,
    name: String,
}

#[derive(Debug, Clone)]
struct Script {
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FleetClassification {
    FirstFleet,
    ResumeOwned,
    ExistingFleet { worker_name: String },
    AmbiguousTarget,
}

#[derive(Debug, Default)]
struct MutationGate {
    classification_complete: bool,
}

impl MutationGate {
    fn mark_classified(&mut self) {
        self.classification_complete = true;
    }

    fn require(&self, operation: &str) -> Result<(), String> {
        if self.classification_complete {
            Ok(())
        } else {
            Err(format!(
                "bootstrap invariant refused Cloudflare mutation '{operation}' before fleet classification"
            ))
        }
    }
}

#[derive(Debug, Clone)]
struct EdgeHttpClient {
    client: reqwest::blocking::Client,
}

impl EdgeHttpClient {
    fn direct() -> Result<Self, String> {
        Self::build(None, None)
    }

    fn via_proxy(proxy: &crate::link::proxy::ResolvedProxy) -> Result<Self, String> {
        Self::build(Some(proxy.url.as_str()), None)
    }

    fn direct_resolved(host: &str, ips: &[IpAddr]) -> Result<Self, String> {
        Self::build(None, Some((host, ips)))
    }

    fn cloudflare_dns() -> Result<Self, String> {
        Self::build(
            None,
            Some((CLOUDFLARE_DOH_HOST, &cloudflare_doh_bootstrap_ips())),
        )
    }

    fn google_dns() -> Result<Self, String> {
        Self::build(None, Some((GOOGLE_DOH_HOST, &google_doh_bootstrap_ips())))
    }

    fn via_socks_resolved(
        proxy: &crate::link::proxy::ResolvedProxy,
        host: &str,
        ips: &[IpAddr],
    ) -> Result<Self, String> {
        let mut url = proxy
            .url
            .parse::<url::Url>()
            .map_err(|_| "cannot parse bootstrap SOCKS proxy URL".to_owned())?;
        if !matches!(url.scheme(), "socks5" | "socks5h") {
            return Err("trusted-DNS bootstrap fallback requires a SOCKS5 proxy".to_owned());
        }
        url.set_scheme("socks5")
            .map_err(|_| "cannot configure local-DNS SOCKS5 proxy".to_owned())?;
        let proxy_url = url.to_string();
        Self::build(Some(&proxy_url), Some((host, ips)))
    }

    fn build(
        proxy_url: Option<&str>,
        resolved_host: Option<(&str, &[IpAddr])>,
    ) -> Result<Self, String> {
        let mut builder = reqwest::blocking::Client::builder().timeout(HTTP_TIMEOUT);
        if proxy_url.is_none() {
            builder = builder.no_proxy();
        }
        if let Some(proxy_url) = proxy_url {
            let proxy = reqwest::Proxy::all(proxy_url)
                .map_err(|error| format!("cannot configure bootstrap Edge proxy: {error}"))?;
            builder = builder.proxy(proxy);
        }
        if let Some((host, ips)) = resolved_host {
            if ips.is_empty() {
                return Err("trusted DNS returned no usable Worker address".to_owned());
            }
            let addrs = ips
                .iter()
                .copied()
                .map(|ip| SocketAddr::new(ip, 443))
                .collect::<Vec<_>>();
            builder = builder.resolve_to_addrs(host, &addrs);
        }
        let client = builder
            .build()
            .map_err(|error| format!("cannot create bootstrap Edge client: {error}"))?;
        Ok(Self { client })
    }
}

#[derive(Debug)]
enum EdgeHealthProbeError {
    Transport(String),
    Validation(String),
}

impl EdgeHealthProbeError {
    fn may_retry_via_proxy(&self) -> bool {
        matches!(self, Self::Transport(_))
    }

    fn into_message(self) -> String {
        match self {
            Self::Transport(message) | Self::Validation(message) => message,
        }
    }
}

struct Cloudflare<'a> {
    client: reqwest::blocking::Client,
    token: &'a SecretBytes,
}

impl<'a> Cloudflare<'a> {
    fn new(token: &'a SecretBytes) -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(4))
            .build()
            .map_err(|error| format!("cannot create Cloudflare HTTP client: {error}"))?;
        Ok(Self { client, token })
    }

    fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(reqwest::StatusCode, Value), String> {
        let url = format!("{CLOUDFLARE_API}/{path}");
        let mut request = self
            .client
            .request(method, &url)
            .bearer_auth(self.token.expose()?);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().map_err(|error| {
            sanitize_error(&format!("Cloudflare request failed: {error}"), self.token)
        })?;
        let status = response.status();
        let payload: Value = response
            .json()
            .map_err(|_| format!("Cloudflare returned non-JSON HTTP {}", status.as_u16()))?;
        Ok((status, payload))
    }

    fn result(&self, status: reqwest::StatusCode, payload: Value) -> Result<Value, String> {
        let success = payload
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(status.is_success());
        if !status.is_success() || !success {
            let summary = cloudflare_error_summary(&payload);
            return Err(sanitize_error(
                &format!("Cloudflare API HTTP {}: {summary}", status.as_u16()),
                self.token,
            ));
        }
        Ok(payload.get("result").cloned().unwrap_or(Value::Null))
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let (status, payload) = self.send(method, path, body)?;
        self.result(status, payload)
    }

    fn accounts(&self) -> Result<Vec<Account>, String> {
        let result = self.request(reqwest::Method::GET, "accounts?per_page=100", None)?;
        let array = result
            .as_array()
            .ok_or_else(|| "Cloudflare account list returned an invalid result".to_owned())?;
        let mut accounts = Vec::new();
        for item in array {
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unnamed account");
            if valid_account_id(id) {
                accounts.push(Account {
                    id: id.to_owned(),
                    name: name.to_owned(),
                });
            }
        }
        Ok(accounts)
    }

    fn scripts(&self, account_id: &str) -> Result<Vec<Script>, String> {
        let result = self.request(
            reqwest::Method::GET,
            &format!("accounts/{account_id}/workers/scripts"),
            None,
        )?;
        let array = result
            .as_array()
            .ok_or_else(|| "Cloudflare Worker inventory returned an invalid result".to_owned())?;
        let mut scripts = Vec::new();
        for item in array {
            if let Some(name) = item
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| item.get("name").and_then(Value::as_str))
            {
                scripts.push(Script {
                    name: name.to_owned(),
                });
            }
        }
        Ok(scripts)
    }

    fn workers_subdomain(&self, account_id: &str) -> Result<Option<String>, String> {
        let (status, payload) = self.send(
            reqwest::Method::GET,
            &format!("accounts/{account_id}/workers/subdomain"),
            None,
        )?;
        if workers_subdomain_is_absent(status, &payload) {
            return Ok(None);
        }
        let result = self.result(status, payload)?;
        Ok(result
            .get("subdomain")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned))
    }

    fn create_workers_subdomain(
        &self,
        gate: &MutationGate,
        account_id: &str,
        candidate: &str,
    ) -> Result<String, String> {
        gate.require("create workers.dev subdomain")?;
        self.request(
            reqwest::Method::PUT,
            &format!("accounts/{account_id}/workers/subdomain"),
            Some(&json!({ "subdomain": candidate })),
        )?;
        self.workers_subdomain(account_id)?.ok_or_else(|| {
            "Cloudflare subdomain creation returned no verified subdomain".to_owned()
        })
    }

    fn upload_worker(
        &self,
        gate: &MutationGate,
        account_id: &str,
        worker_name: &str,
        bundle: &[u8],
        metadata: &Value,
    ) -> Result<(), String> {
        gate.require("deploy Worker")?;
        let module = reqwest::blocking::multipart::Part::bytes(bundle.to_vec())
            .file_name(EDGE_MAIN_MODULE)
            .mime_str("application/javascript+module")
            .map_err(|error| format!("cannot encode Edge module upload: {error}"))?;
        let form = reqwest::blocking::multipart::Form::new()
            .text("metadata", metadata.to_string())
            .part(EDGE_MAIN_MODULE.to_owned(), module);
        let url = format!("{CLOUDFLARE_API}/accounts/{account_id}/workers/scripts/{worker_name}");
        let response = self
            .client
            .put(url)
            .bearer_auth(self.token.expose()?)
            .multipart(form)
            .send()
            .map_err(|error| {
                sanitize_error(
                    &format!("Cloudflare Worker upload failed: {error}"),
                    self.token,
                )
            })?;
        let status = response.status();
        let payload: Value = response.json().map_err(|_| {
            format!(
                "Cloudflare Worker upload returned non-JSON HTTP {}",
                status.as_u16()
            )
        })?;
        let success = payload
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(status.is_success());
        if !status.is_success() || !success {
            return Err(sanitize_error(
                &format!(
                    "Cloudflare Worker upload HTTP {}: {}",
                    status.as_u16(),
                    cloudflare_error_summary(&payload)
                ),
                self.token,
            ));
        }
        Ok(())
    }

    fn enable_worker_subdomain(
        &self,
        gate: &MutationGate,
        account_id: &str,
        worker_name: &str,
    ) -> Result<(), String> {
        gate.require("enable Worker workers.dev subdomain")?;
        let result = self.request(
            reqwest::Method::POST,
            &format!("accounts/{account_id}/workers/scripts/{worker_name}/subdomain"),
            Some(&json!({ "enabled": true, "previews_enabled": false })),
        )?;
        if result.get("enabled").and_then(Value::as_bool) != Some(true) {
            return Err(
                "Cloudflare did not confirm the Worker workers.dev subdomain as enabled".to_owned(),
            );
        }
        Ok(())
    }

    fn set_worker_schedule(
        &self,
        gate: &MutationGate,
        account_id: &str,
        worker_name: &str,
    ) -> Result<(), String> {
        gate.require("configure Worker cron")?;
        self.request(
            reqwest::Method::PUT,
            &format!("accounts/{account_id}/workers/scripts/{worker_name}/schedules"),
            Some(&json!([{ "cron": EDGE_CRON }])),
        )?;
        Ok(())
    }

    fn put_secret(
        &self,
        gate: &MutationGate,
        account_id: &str,
        worker_name: &str,
        name: &str,
        value: &SecretBytes,
    ) -> Result<(), String> {
        gate.require(&format!("put Worker secret {name}"))?;
        self.request(
            reqwest::Method::PUT,
            &format!("accounts/{account_id}/workers/scripts/{worker_name}/secrets"),
            Some(&json!({
                "name": name,
                "text": value.expose()?,
                "type": "secret_text",
            })),
        )?;
        Ok(())
    }

    fn list_secret_names(
        &self,
        account_id: &str,
        worker_name: &str,
    ) -> Result<Vec<String>, String> {
        let result = self.request(
            reqwest::Method::GET,
            &format!("accounts/{account_id}/workers/scripts/{worker_name}/secrets"),
            None,
        )?;
        let array = result.as_array().ok_or_else(|| {
            "Cloudflare Worker secret inventory returned an invalid result".to_owned()
        })?;
        Ok(array
            .iter()
            .filter_map(|item| item.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect())
    }

    fn delete_secret(
        &self,
        gate: &MutationGate,
        account_id: &str,
        worker_name: &str,
        name: &str,
    ) -> Result<(), String> {
        gate.require(&format!("delete Worker secret {name}"))?;
        self.request(
            reqwest::Method::DELETE,
            &format!("accounts/{account_id}/workers/scripts/{worker_name}/secrets/{name}?url_encoded=true"),
            None,
        )?;
        Ok(())
    }
}

pub fn run(paths: &RuntimePaths) -> Result<ExitCode, String> {
    if !bootstrap_platform_supported(std::env::consts::OS) {
        return Err(
            "worker bootstrap currently supports macOS and Linux first-device installation"
                .to_owned(),
        );
    }
    if paths.instance.is_named() {
        return Err("worker bootstrap is available only on the default Herdr instance".to_owned());
    }
    run_inner(paths)
}

fn bootstrap_platform_supported(os: &str) -> bool {
    matches!(os, "macos" | "linux")
}

fn run_inner(paths: &RuntimePaths) -> Result<ExitCode, String> {
    let source_commit = crate::runtime_meta::compiled_source_commit()
        .filter(|value| valid_source_commit(value))
        .ok_or_else(|| {
            "worker bootstrap requires a release runtime with an exact source commit".to_owned()
        })?
        .to_owned();
    let runtime_version = crate::runtime_meta::runtime_version().to_owned();
    let journal_path = journal_path(paths);
    let existing_journal = read_journal(&journal_path)?;

    let local_config =
        crate::config::Config::load_for_instance(&paths.config_file, &paths.instance)?;
    if let Some(device_id) = local_config.edge_device_id.as_deref() {
        let resumable = existing_journal
            .as_ref()
            .and_then(|journal| journal.canonical_device_id.as_deref())
            == Some(device_id)
            && existing_journal
                .as_ref()
                .map(|journal| journal.phase < Phase::OperationalReady)
                .unwrap_or(false);
        if !resumable {
            return Err(format!(
                "this computer is already enrolled as {device_id}; use `herdr-mcp worker pair` to add another computer instead of first-Worker bootstrap"
            ));
        }
    }

    println!("Herdr first Worker bootstrap");
    println!("[1/7] Check — local first-device state is eligible.");
    let (token, refresh_token) = acquire_cloudflare_credential()?;
    let cloudflare = Cloudflare::new(&token)?;
    println!("[2/7] Cloudflare — temporary authorization acquired; it will not be persisted.");

    let account = select_account(&cloudflare)?;
    println!(
        "[3/7] Account — using {} ({}).",
        account.name,
        short_id(&account.id)
    );
    let scripts = cloudflare.scripts(&account.id)?;
    let existing_subdomain = cloudflare.workers_subdomain(&account.id)?;
    let worker_name = worker_name()?;
    let classification = classify_fleet(
        &scripts,
        existing_subdomain.as_deref(),
        &worker_name,
        existing_journal.as_ref(),
    )?;
    let mut gate = MutationGate::default();
    gate.mark_classified();

    match classification {
        FleetClassification::ExistingFleet { worker_name } => {
            return Err(format!(
                "existing Herdr Worker '{worker_name}' found in this Cloudflare account; do not deploy a second fleet. On any enrolled computer run `herdr-mcp worker pair`, then run `herdr-mcp worker connect <pairing-address>` here"
            ));
        }
        FleetClassification::AmbiguousTarget => {
            return Err(format!(
                "Cloudflare Worker name '{worker_name}' is already occupied but could not be proven to belong to this resumable Herdr bootstrap; no mutation was attempted"
            ));
        }
        FleetClassification::FirstFleet | FleetClassification::ResumeOwned => {}
    }

    let mut journal = match existing_journal {
        Some(journal) => {
            validate_journal(
                &journal,
                &source_commit,
                &runtime_version,
                &account.id,
                &worker_name,
            )?;
            journal
        }
        None => BootstrapJournal::new(
            source_commit.clone(),
            runtime_version.clone(),
            account.id.clone(),
            worker_name.clone(),
        ),
    };
    write_journal(&journal_path, &journal)?;

    let subdomain = match existing_subdomain {
        Some(value) => value,
        None => {
            let candidate = subdomain_candidate(&account.id);
            let value = cloudflare.create_workers_subdomain(&gate, &account.id, &candidate)?;
            println!("Cloudflare account workers.dev subdomain created and read back.");
            value
        }
    };
    let edge_origin = format!("https://{worker_name}.{subdomain}.workers.dev");
    journal.workers_dev_origin = Some(edge_origin.clone());
    journal.advance(Phase::SubdomainReady);
    write_journal(&journal_path, &journal)?;

    let script_exists = scripts.iter().any(|script| script.name == worker_name);
    let edge_http = if !script_exists || journal.phase < Phase::WorkerDeployed {
        journal.advance(Phase::WorkerDeploying);
        write_journal(&journal_path, &journal)?;
        let bundle = prepare_edge_bundle(&source_commit, &runtime_version)?;
        let metadata =
            worker_upload_metadata(&worker_name, &edge_origin, &runtime_version, !script_exists)?;
        cloudflare.upload_worker(&gate, &account.id, &worker_name, &bundle.bytes, &metadata)?;
        cloudflare.enable_worker_subdomain(&gate, &account.id, &worker_name)?;
        cloudflare.set_worker_schedule(&gate, &account.id, &worker_name)?;
        let edge_http = verify_health(&edge_origin, &worker_name, Some(&runtime_version))?;
        journal.advance(Phase::WorkerDeployed);
        write_journal(&journal_path, &journal)?;
        edge_http
    } else {
        let edge_http = verify_health(&edge_origin, &worker_name, None)?;
        println!("Existing Herdr Worker verified; resuming configuration.");
        edge_http
    };
    println!("[4/7] Worker — release-matched Worker is healthy at {edge_origin}.");

    if journal.phase < Phase::DeviceEnrolled {
        let pepper = random_secret(32)?;
        let operator = random_secret(32)?;
        let operator_expiry = SecretBytes::from_string(temporary_operator_expiry_ms().to_string())?;
        cloudflare.put_secret(
            &gate,
            &account.id,
            &worker_name,
            PAIRING_PEPPER_SECRET,
            &pepper,
        )?;
        cloudflare.put_secret(
            &gate,
            &account.id,
            &worker_name,
            TEMP_OPERATOR_EXPIRY_SECRET,
            &operator_expiry,
        )?;
        cloudflare.put_secret(
            &gate,
            &account.id,
            &worker_name,
            TEMP_OPERATOR_SECRET,
            &operator,
        )?;
        let names = cloudflare.list_secret_names(&account.id, &worker_name)?;
        if !names.iter().any(|name| name == PAIRING_PEPPER_SECRET)
            || !names.iter().any(|name| name == TEMP_OPERATOR_EXPIRY_SECRET)
            || !names.iter().any(|name| name == TEMP_OPERATOR_SECRET)
        {
            return Err(
                "Worker secret provisioning could not be verified; bootstrap remains resumable"
                    .to_owned(),
            );
        }
        journal.advance(Phase::SecretsProvisioned);
        write_journal(&journal_path, &journal)?;

        let current_devices = edge_devices_with_operator(&edge_http, &edge_origin, &operator)?;
        if !current_devices.is_empty() {
            return Err("first-fleet enrollment refused because the Worker device registry is no longer empty; switch to the existing-Worker pairing flow".to_owned());
        }
        let name = crate::device_name::system_device_display_name();
        let enrolled =
            create_and_consume_first_pairing(&edge_http, &edge_origin, &operator, name.as_deref())?;
        verify_device_fleet_admin(&edge_http, &edge_origin, &enrolled)?;
        let code =
            crate::worker::adopt_bootstrap_enrollment(paths, &edge_origin, enrolled.clone())?;
        if code != ExitCode::SUCCESS {
            return Err(
                "local canonical-device activation did not complete successfully".to_owned(),
            );
        }
        journal.canonical_device_id = Some(enrolled.device_id.clone());
        journal.advance(Phase::DeviceEnrolled);
        write_journal(&journal_path, &journal)?;
    }
    println!("[5/7] Computer — canonical device enrollment is active.");

    remove_temporary_operator(
        |name| cloudflare.delete_secret(&gate, &account.id, &worker_name, name),
        || cloudflare.list_secret_names(&account.id, &worker_name),
    )?;
    journal.advance(Phase::OperatorRemoved);
    write_journal(&journal_path, &journal)?;

    drop(cloudflare);
    drop(refresh_token);
    drop(token);

    verify_public_oauth(&edge_http, &edge_origin)?;
    verify_current_device_inventory(paths, journal.canonical_device_id.as_deref(), &edge_http)?;
    let link = crate::link::ownership::status_report()?;
    if link.get("operational_ready").and_then(Value::as_bool) != Some(true) {
        let safe = crate::status::sanitize_probe_token(&link.to_string());
        return Err(format!(
            "bootstrap reached Link reconciliation but `herdr-mcp link status` did not report operational_ready=true: {safe}"
        ));
    }
    journal.advance(Phase::OperationalReady);
    write_journal(&journal_path, &journal)?;
    println!("[6/7] Connection — `herdr-mcp link status` reports operational_ready=true.");
    println!("[7/7] Done — MCP URL: {edge_origin}/mcp");
    println!(
        "Next: create the ChatGPT Connector for this MCP URL and approve it from this enrolled computer."
    );
    println!("Cloudflare credential management: https://dash.cloudflare.com/profile/api-tokens");
    println!("If a Cloudflare credential appeared in any conversation or terminal, revoke it now.");
    Ok(ExitCode::SUCCESS)
}

fn classify_fleet(
    scripts: &[Script],
    subdomain: Option<&str>,
    target_worker: &str,
    journal: Option<&BootstrapJournal>,
) -> Result<FleetClassification, String> {
    let target_exists = scripts.iter().any(|script| script.name == target_worker);
    if target_exists {
        let journal_matches = journal
            .map(|value| value.worker_name == target_worker)
            .unwrap_or(false);
        let journal_proves_deployed = journal
            .map(|value| value.phase >= Phase::WorkerDeployed)
            .unwrap_or(false);
        let observed_matching_health = match (journal, subdomain) {
            (Some(value), Some(subdomain))
                if journal_matches && value.phase >= Phase::WorkerDeploying =>
            {
                let origin = format!("https://{target_worker}.{subdomain}.workers.dev");
                verify_health(&origin, target_worker, Some(&value.runtime_version)).is_ok()
            }
            _ => false,
        };
        if resumable_target_observation(
            journal_matches,
            journal_proves_deployed,
            observed_matching_health,
        ) {
            return Ok(FleetClassification::ResumeOwned);
        }
    }

    for script in scripts
        .iter()
        .filter(|script| script.name.starts_with("herdr-edge"))
    {
        if let Some(subdomain) = subdomain {
            let origin = format!("https://{}.{}.workers.dev", script.name, subdomain);
            if verify_health(&origin, &script.name, None).is_ok() {
                return Ok(FleetClassification::ExistingFleet {
                    worker_name: script.name.clone(),
                });
            }
        } else {
            return Ok(if script.name == target_worker {
                FleetClassification::AmbiguousTarget
            } else {
                FleetClassification::ExistingFleet {
                    worker_name: script.name.clone(),
                }
            });
        }
    }
    if target_exists {
        return Ok(FleetClassification::AmbiguousTarget);
    }
    Ok(FleetClassification::FirstFleet)
}

fn acquire_cloudflare_credential() -> Result<(SecretBytes, Option<SecretBytes>), String> {
    if let Ok(value) = std::env::var("CLOUDFLARE_API_TOKEN") {
        if !verify_api_token_shape(&value) {
            return Err("CLOUDFLARE_API_TOKEN has an invalid shape".to_owned());
        }
        let token = SecretBytes::from_string(value)?;
        verify_api_token(&token)?;
        return Ok((token, None));
    }
    match acquire_device_flow() {
        Ok(tokens) => Ok(tokens),
        Err(device_error) => {
            if !stdin_is_tty() {
                return Err(format!(
                    "Cloudflare device authorization failed: {device_error}. Set CLOUDFLARE_API_TOKEN in the current process to use the API-token fallback"
                ));
            }
            eprintln!("Cloudflare device authorization could not complete: {device_error}");
            eprintln!(
                "API-token fallback permissions: Workers Scripts -> Edit; Account Settings -> Read."
            );
            let value = read_hidden_line("Paste temporary Cloudflare API token (input hidden): ")?;
            if !verify_api_token_shape(&value) {
                return Err("Cloudflare API token has an invalid shape".to_owned());
            }
            let token = SecretBytes::from_string(value)?;
            verify_api_token(&token)?;
            Ok((token, None))
        }
    }
}

fn verify_api_token(token: &SecretBytes) -> Result<(), String> {
    let client = Cloudflare::new(token)?;
    let result = client.request(reqwest::Method::GET, "user/tokens/verify", None)?;
    let status = result.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "active" {
        return Err("Cloudflare API token is not active".to_owned());
    }
    Ok(())
}

fn acquire_device_flow() -> Result<(SecretBytes, Option<SecretBytes>), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(4))
        .build()
        .map_err(|error| format!("cannot create Cloudflare device-flow client: {error}"))?;
    let body = form_body(&[
        ("client_id", CLOUDFLARE_CLIENT_ID),
        ("scope", CLOUDFLARE_SCOPES),
    ]);
    let response = client
        .post(CLOUDFLARE_DEVICE_AUTH)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .map_err(|error| format!("device authorization request failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "device authorization returned HTTP {}",
            status.as_u16()
        ));
    }
    let payload: DeviceFlowAuthorizationResponse = response.json().map_err(|_| {
        format!(
            "device authorization returned invalid JSON HTTP {}",
            status.as_u16()
        )
    })?;
    let device_code = SecretBytes::from_string(payload.device_code)?;
    if payload.user_code.trim().is_empty() || payload.verification_uri.trim().is_empty() {
        return Err("device authorization response is missing required public fields".to_owned());
    }
    let user_code = payload.user_code;
    let verification_uri = payload.verification_uri;
    let expires = payload.expires_in.unwrap_or(300).min(300);
    let mut interval = payload.interval.unwrap_or(5).max(1);
    let verification_uri_complete = payload
        .verification_uri_complete
        .unwrap_or_else(|| verification_uri.clone());

    println!("Open this Cloudflare page and approve Herdr: {verification_uri}");
    println!("Verification code: {user_code}");
    println!("Code expires in at most {expires} seconds.");
    open_browser(&verification_uri_complete);

    let started = std::time::Instant::now();
    let deadline = Duration::from_secs(expires).min(DEVICE_FLOW_MAX);
    loop {
        if started.elapsed() >= deadline {
            return Err("Cloudflare device authorization expired".to_owned());
        }
        std::thread::sleep(Duration::from_secs(interval));
        let body = form_body(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code.expose()?),
            ("client_id", CLOUDFLARE_CLIENT_ID),
        ]);
        let response = match client
            .post(CLOUDFLARE_TOKEN)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
        {
            Ok(response) => response,
            Err(_) => continue,
        };
        let payload: DeviceFlowTokenResponse = match response.json() {
            Ok(payload) => payload,
            Err(_) => continue,
        };
        if let Some(token) = payload.access_token {
            let access = SecretBytes::from_string(token)?;
            let refresh = payload
                .refresh_token
                .map(SecretBytes::from_string)
                .transpose()?;
            return Ok((access, refresh));
        }
        match payload.error.as_deref().unwrap_or("unexpected_response") {
            "authorization_pending" => {}
            "slow_down" => interval = interval.saturating_add(5).min(30),
            "access_denied" => return Err("Cloudflare authorization was denied".to_owned()),
            "expired_token" => return Err("Cloudflare device authorization expired".to_owned()),
            other => return Err(format!("Cloudflare device authorization failed: {other}")),
        }
    }
}

fn resumable_target_observation(
    journal_matches: bool,
    journal_proves_deployed: bool,
    observed_matching_health: bool,
) -> bool {
    journal_matches && (journal_proves_deployed || observed_matching_health)
}

fn temporary_operator_expiry_ms() -> u64 {
    now_ms().saturating_add(TEMP_OPERATOR_TTL.as_millis().min(u128::from(u64::MAX)) as u64)
}

fn remove_temporary_operator<D, L>(mut delete: D, mut list: L) -> Result<(), String>
where
    D: FnMut(&str) -> Result<(), String>,
    L: FnMut() -> Result<Vec<String>, String>,
{
    let before = list()?;
    if before.iter().any(|name| name == TEMP_OPERATOR_SECRET) {
        delete(TEMP_OPERATOR_SECRET).map_err(|error| {
            format!(
                "failed to delete temporary Worker operator credential; bootstrap is NOT complete and the bounded TTL remains the safety fence: {error}"
            )
        })?;
    }
    if before
        .iter()
        .any(|name| name == TEMP_OPERATOR_EXPIRY_SECRET)
    {
        delete(TEMP_OPERATOR_EXPIRY_SECRET).map_err(|error| {
            format!("failed to delete temporary Worker operator expiry marker: {error}")
        })?;
    }
    let after = list()?;
    if after.iter().any(|name| name == TEMP_OPERATOR_SECRET) {
        return Err(
            "temporary Worker operator credential still exists after deletion attempt; bootstrap is NOT complete"
                .to_owned(),
        );
    }
    if after.iter().any(|name| name == TEMP_OPERATOR_EXPIRY_SECRET) {
        return Err(
            "temporary Worker operator expiry marker still exists after deletion attempt; bootstrap is NOT complete"
                .to_owned(),
        );
    }
    Ok(())
}

fn select_account(cloudflare: &Cloudflare<'_>) -> Result<Account, String> {
    let accounts = cloudflare.accounts()?;
    if accounts.is_empty() {
        return Err("Cloudflare authorization can access no accounts".to_owned());
    }
    if let Ok(selected) = std::env::var("CLOUDFLARE_ACCOUNT_ID") {
        return accounts
            .into_iter()
            .find(|account| account.id == selected)
            .ok_or_else(|| {
                "CLOUDFLARE_ACCOUNT_ID is not accessible with this temporary credential".to_owned()
            });
    }
    if accounts.len() == 1 {
        return Ok(accounts[0].clone());
    }
    if !stdin_is_tty() {
        return Err("multiple Cloudflare accounts are accessible; set non-secret CLOUDFLARE_ACCOUNT_ID and rerun".to_owned());
    }
    eprintln!("Choose Cloudflare account:");
    for (index, account) in accounts.iter().enumerate() {
        eprintln!(
            "  {}. {} ({})",
            index + 1,
            account.name,
            short_id(&account.id)
        );
    }
    let mut line = String::new();
    eprint!("Account number: ");
    io::stderr().flush().map_err(|error| error.to_string())?;
    io::stdin()
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    let index = line
        .trim()
        .parse::<usize>()
        .map_err(|_| "invalid Cloudflare account selection".to_owned())?;
    accounts
        .get(index.saturating_sub(1))
        .cloned()
        .ok_or_else(|| "invalid Cloudflare account selection".to_owned())
}

struct EdgeBundle {
    bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
struct EdgeReleaseDescriptor {
    name: String,
    size: usize,
    sha256: String,
    url: url::Url,
    identity: release_trust::ReleaseIdentity,
}

fn prepare_edge_bundle(source_commit: &str, runtime_version: &str) -> Result<EdgeBundle, String> {
    if let Some(path) = std::env::var_os("HERDR_MCP_EDGE_BUNDLE_PATH") {
        if crate::runtime_meta::runtime_channel() != "dev" {
            return Err("HERDR_MCP_EDGE_BUNDLE_PATH is allowed only in a DEV runtime".to_owned());
        }
        let path = PathBuf::from(path);
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("cannot inspect DEV Edge bundle: {error}"))?;
        if !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > EDGE_BUNDLE_MAX_BYTES as u64
        {
            return Err("DEV Edge bundle is missing, empty, or too large".to_owned());
        }
        return fs::read(&path)
            .map(|bytes| EdgeBundle { bytes })
            .map_err(|error| format!("cannot read DEV Edge bundle: {error}"));
    }

    let tag = format!("v{runtime_version}");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(format!("herdr-mcp-bootstrap/{runtime_version}"))
        .build()
        .map_err(|error| format!("cannot create Edge release download client: {error}"))?;
    let manifest_url = format!(
        "https://github.com/{}/releases/download/{tag}/release-manifest.json",
        release_trust::RELEASE_REPOSITORY
    );
    let manifest_bytes = read_http_bounded(
        client.get(&manifest_url),
        EDGE_MANIFEST_MAX_BYTES,
        "release manifest",
    )?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("release manifest is invalid JSON: {error}"))?;
    let descriptor = parse_edge_release_manifest(&manifest, source_commit, runtime_version)?;
    let bytes = read_http_bounded(
        client.get(descriptor.url.clone()),
        EDGE_BUNDLE_MAX_BYTES,
        "Edge bundle",
    )?;
    if bytes.len() != descriptor.size {
        return Err("Edge bundle size does not match the release manifest".to_owned());
    }
    let actual_sha256 = sha256_hex(&bytes);
    if actual_sha256 != descriptor.sha256 {
        return Err("Edge bundle sha256 does not match the release manifest".to_owned());
    }
    let attestation_url = release_trust::attestation_api_url(&descriptor.sha256)?;
    let attestation = read_http_bounded(
        client
            .get(attestation_url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28"),
        EDGE_ATTESTATION_MAX_BYTES,
        "Edge artifact attestation",
    )?;
    release_trust::verify_github_attestations(
        &attestation,
        &descriptor.name,
        &descriptor.sha256,
        &descriptor.identity,
    )?;
    Ok(EdgeBundle { bytes })
}

fn parse_edge_release_manifest(
    manifest: &Value,
    source_commit: &str,
    runtime_version: &str,
) -> Result<EdgeReleaseDescriptor, String> {
    if manifest.get("schema_version").and_then(Value::as_u64)
        != Some(release_trust::MANIFEST_SCHEMA_VERSION)
        || manifest.get("product").and_then(Value::as_str) != Some("herdr-mcp")
        || manifest.get("version").and_then(Value::as_str) != Some(runtime_version)
    {
        return Err("release manifest identity does not match this runtime".to_owned());
    }
    let tag = format!("v{runtime_version}");
    if manifest.get("tag").and_then(Value::as_str) != Some(tag.as_str()) {
        return Err("release manifest tag/version mismatch".to_owned());
    }
    let identity = release_trust::parse_manifest_identity(manifest, &tag)?;
    if identity.source_commit != source_commit {
        return Err("release manifest source commit does not match this runtime".to_owned());
    }
    let edge = manifest
        .get("edge")
        .and_then(Value::as_object)
        .ok_or_else(|| "release manifest is missing the Edge artifact".to_owned())?;
    let expected_name = format!("herdr-edge-{runtime_version}.mjs");
    let name = edge
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| *value == expected_name)
        .ok_or_else(|| "release manifest Edge artifact name is invalid".to_owned())?
        .to_owned();
    let size = edge
        .get("size")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0 && *value <= EDGE_BUNDLE_MAX_BYTES as u64)
        .ok_or_else(|| "release manifest Edge artifact size is invalid".to_owned())?
        as usize;
    let sha256 = edge
        .get("sha256")
        .and_then(Value::as_str)
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "release manifest Edge artifact sha256 is invalid".to_owned())?
        .to_ascii_lowercase();
    let url = edge
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| "release manifest Edge artifact URL is missing".to_owned())?
        .parse::<url::Url>()
        .map_err(|_| "release manifest Edge artifact URL is invalid".to_owned())?;
    let expected_url = format!(
        "https://github.com/{}/releases/download/{tag}/{expected_name}",
        release_trust::RELEASE_REPOSITORY
    )
    .parse::<url::Url>()
    .map_err(|_| "cannot construct trusted Edge release URL".to_owned())?;
    if url != expected_url {
        return Err(
            "release manifest Edge artifact URL does not match the trusted release".to_owned(),
        );
    }
    Ok(EdgeReleaseDescriptor {
        name,
        size,
        sha256,
        url,
        identity,
    })
}

fn read_http_bounded(
    request: reqwest::blocking::RequestBuilder,
    max_bytes: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    let mut response = request
        .send()
        .map_err(|error| format!("{label} download failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "{label} download returned HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(format!("{label} is too large"));
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {label}: {error}"))?;
    if bytes.len() > max_bytes {
        return Err(format!("{label} is too large"));
    }
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn worker_upload_metadata(
    worker_name: &str,
    edge_origin: &str,
    runtime_version: &str,
    include_migrations: bool,
) -> Result<Value, String> {
    if !valid_worker_name(worker_name) || !edge_origin.starts_with("https://") {
        return Err("invalid Worker identity for direct Edge upload".to_owned());
    }
    let mut metadata = json!({
        "main_module": EDGE_MAIN_MODULE,
        "compatibility_date": EDGE_COMPATIBILITY_DATE,
        "bindings": [
            { "type": "durable_object_namespace", "name": "WORKSTATION_DO", "class_name": "WorkstationDO" },
            { "type": "durable_object_namespace", "name": "OAUTH_STORE_DO", "class_name": "OAuthStoreDO" },
            { "type": "durable_object_namespace", "name": "DEVICE_REGISTRY_DO", "class_name": "DeviceRegistryDO" },
            { "type": "plain_text", "name": "EDGE_ENV", "text": "prod" },
            { "type": "plain_text", "name": "EDGE_PROJECT", "text": worker_name },
            { "type": "plain_text", "name": "EDGE_VERSION", "text": runtime_version },
            { "type": "plain_text", "name": "OAUTH_ISSUER", "text": edge_origin }
        ]
    });
    if include_migrations {
        metadata["migrations"] = json!({
            "new_tag": "v3",
            "steps": [
                { "new_sqlite_classes": ["WorkstationDO"] },
                { "new_sqlite_classes": ["OAuthStoreDO"] },
                { "new_sqlite_classes": ["DeviceRegistryDO"] }
            ]
        });
    }
    Ok(metadata)
}

fn create_and_consume_first_pairing(
    edge_http: &EdgeHttpClient,
    edge_origin: &str,
    operator: &SecretBytes,
    name: Option<&str>,
) -> Result<crate::worker::EnrolledCredential, String> {
    let mut body = json!({ "ttl_seconds": 600, "require_empty_fleet": true });
    if let Some(name) = name {
        body["name"] = Value::String(name.to_owned());
    }
    let authorization = operator.expose()?;
    let delays = [
        Duration::ZERO,
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(4),
        Duration::from_secs(8),
    ];
    let attempts = delays.len();
    let mut ready_pairing = None;
    for (attempt, delay) in delays.into_iter().enumerate() {
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        let response = edge_http
            .client
            .post(format!("{edge_origin}/devices/pairings"))
            .bearer_auth(authorization)
            .json(&body)
            .send()
            .map_err(|error| {
                sanitize_error(&format!("cannot create first pairing: {error}"), operator)
            })?;
        let status = response.status();
        let pairing: Value = response
            .json()
            .map_err(|_| format!("first pairing returned non-JSON HTTP {}", status.as_u16()))?;
        if status.is_success() && pairing.get("ok").and_then(Value::as_bool) == Some(true) {
            ready_pairing = Some(pairing);
            break;
        }
        let code = pairing
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("pairing_create_failed");
        if first_pairing_create_is_retryable(status, &pairing) && attempt + 1 < attempts {
            continue;
        }
        return Err(format!("first pairing refused: {code}"));
    }
    let pairing = ready_pairing.ok_or_else(|| "first pairing did not become ready".to_owned())?;
    let pairing_id = required_string(&pairing, "pairing_id")?;
    let code = required_string(&pairing, "code")?;
    let mut consume = json!({ "pairing_id": pairing_id, "code": code });
    if let Some(name) = name {
        consume["name"] = Value::String(name.to_owned());
    }
    let response = edge_http
        .client
        .post(format!("{edge_origin}/devices/pairings/consume"))
        .json(&consume)
        .send()
        .map_err(|error| format!("first pairing consume delivery is ambiguous; rerun bootstrap to read back fleet state before any further mutation: {error}"))?;
    let status = response.status();
    let payload: Value = response.json().map_err(|_| {
        format!(
            "first pairing consume returned non-JSON HTTP {}",
            status.as_u16()
        )
    })?;
    if !status.is_success() || payload.get("ok").and_then(Value::as_bool) != Some(true) {
        let code = payload
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("pairing_consume_failed");
        return Err(format!("first pairing consume failed: {code}"));
    }
    Ok(crate::worker::EnrolledCredential {
        device_id: required_string(&payload, "device_id")?,
        workstation_id: required_string(&payload, "workstation_id")?,
        device_secret: required_string(&payload, "device_secret")?,
        recovered_existing: false,
    })
}

fn first_pairing_create_is_retryable(status: reqwest::StatusCode, payload: &Value) -> bool {
    status == reqwest::StatusCode::SERVICE_UNAVAILABLE
        && payload.get("code").and_then(Value::as_str) == Some("pairing_unavailable")
}

fn edge_devices_with_operator(
    edge_http: &EdgeHttpClient,
    edge_origin: &str,
    operator: &SecretBytes,
) -> Result<Vec<Value>, String> {
    let delays = [
        Duration::ZERO,
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(4),
        Duration::from_secs(8),
    ];
    let authorization = operator.expose()?;
    let mut last_retryable_error = None;
    let attempts = delays.len();
    for (attempt, delay) in delays.into_iter().enumerate() {
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        let response = match edge_http
            .client
            .get(format!("{edge_origin}/devices"))
            .bearer_auth(authorization)
            .send()
        {
            Ok(response) => response,
            Err(error) => {
                last_retryable_error = Some(sanitize_error(
                    &format!("cannot inspect first-fleet registry: {error}"),
                    operator,
                ));
                if attempt + 1 < attempts {
                    continue;
                }
                break;
            }
        };
        let status = response.status();
        let payload: Value = match response.json() {
            Ok(payload) => payload,
            Err(_) if operator_registry_status_is_retryable(status) => {
                last_retryable_error = Some(format!(
                    "temporary operator is not ready yet (HTTP {})",
                    status.as_u16()
                ));
                if attempt + 1 < attempts {
                    continue;
                }
                break;
            }
            Err(_) => {
                return Err(format!(
                    "device inventory returned non-JSON HTTP {}",
                    status.as_u16()
                ));
            }
        };
        if status.is_success() && payload.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(payload
                .get("devices")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default());
        }
        if operator_registry_status_is_retryable(status) {
            last_retryable_error = Some(format!(
                "temporary operator is not ready yet (HTTP {})",
                status.as_u16()
            ));
            if attempt + 1 < attempts {
                continue;
            }
            break;
        }
        return Err(format!(
            "temporary operator could not read the Worker device registry (HTTP {})",
            status.as_u16()
        ));
    }
    Err(last_retryable_error.unwrap_or_else(|| {
        "temporary operator could not read the Worker device registry".to_owned()
    }))
}

fn operator_registry_status_is_retryable(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 403) || status.is_server_error()
}

fn verify_device_fleet_admin(
    edge_http: &EdgeHttpClient,
    edge_origin: &str,
    enrolled: &crate::worker::EnrolledCredential,
) -> Result<(), String> {
    let response = edge_http
        .client
        .get(format!("{edge_origin}/devices"))
        .bearer_auth(&enrolled.device_secret)
        .header("x-herdr-workstation", &enrolled.workstation_id)
        .send()
        .map_err(|error| {
            "canonical device credential could not be verified as fleet-admin".to_owned()
                + &format!(": {error}")
        })?;
    let status = response.status();
    let payload: Value = response.json().map_err(|_| {
        format!(
            "canonical device verification returned non-JSON HTTP {}",
            status.as_u16()
        )
    })?;
    if !status.is_success() || payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(
            "canonical device credential does not independently authenticate as fleet-admin"
                .to_owned(),
        );
    }
    Ok(())
}

fn verify_current_device_inventory(
    paths: &RuntimePaths,
    expected: Option<&str>,
    _edge_http: &EdgeHttpClient,
) -> Result<(), String> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let payload = crate::worker::extension_fleet_snapshot_with_client(paths, &_edge_http.client)?;
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("authenticated device inventory is unavailable after bootstrap".to_owned());
    }
    let local = payload.pointer("/local/device_id").and_then(Value::as_str);
    if expected.is_some() && local != expected {
        return Err(
            "authenticated device inventory does not match the bootstrap canonical device"
                .to_owned(),
        );
    }
    Ok(())
}

fn verify_health(
    edge_origin: &str,
    worker_name: &str,
    expected_version: Option<&str>,
) -> Result<EdgeHttpClient, String> {
    let direct = EdgeHttpClient::direct()?;
    match probe_health(&direct, edge_origin, worker_name, expected_version) {
        Ok(()) => Ok(direct),
        Err(error) if !error.may_retry_via_proxy() => Err(error.into_message()),
        Err(error) => {
            let direct_error = error.into_message();
            let mut last_transport_error = direct_error;

            if let Some(proxy) = crate::link::proxy::resolve_link_proxy() {
                let proxied = EdgeHttpClient::via_proxy(&proxy)?;
                match probe_health(&proxied, edge_origin, worker_name, expected_version) {
                    Ok(()) => return Ok(proxied),
                    Err(error) if !error.may_retry_via_proxy() => {
                        return Err(error.into_message());
                    }
                    Err(error) => last_transport_error = error.into_message(),
                }
            }

            match trusted_dns_direct_client(edge_origin) {
                Ok(Some(resolved)) => {
                    match probe_health(&resolved, edge_origin, worker_name, expected_version) {
                        Ok(()) => {
                            report_workers_dev_hosts_recovery(edge_origin);
                            return Ok(resolved);
                        }
                        Err(error) if !error.may_retry_via_proxy() => {
                            return Err(error.into_message());
                        }
                        Err(error) => last_transport_error = error.into_message(),
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    last_transport_error = format!(
                        "{last_transport_error}; direct trusted DNS fallback unavailable: {error}"
                    );
                }
            }

            if let Some(socks) = crate::link::proxy::resolve_link_socks_proxy() {
                let proxied = EdgeHttpClient::via_proxy(&socks)?;
                let host = edge_origin_host(edge_origin)?;
                if host.ends_with(".workers.dev") {
                    let ips = resolve_trusted_worker_ips(&proxied, &host).map_err(|error| {
                        format!("{last_transport_error}; trusted DNS fallback unavailable: {error}")
                    })?;
                    let resolved = EdgeHttpClient::via_socks_resolved(&socks, &host, &ips)?;
                    probe_health(&resolved, edge_origin, worker_name, expected_version)
                        .map_err(EdgeHealthProbeError::into_message)?;
                    return Ok(resolved);
                }
            }

            Err(last_transport_error)
        }
    }
}

pub(crate) fn client_for_edge_origin(
    edge_origin: &str,
) -> Result<reqwest::blocking::Client, String> {
    let direct = EdgeHttpClient::direct()?;
    match probe_edge_transport(&direct, edge_origin) {
        Ok(()) => Ok(direct.client),
        Err(error) if !error.may_retry_via_proxy() => Err(error.into_message()),
        Err(error) => {
            let mut last_transport_error = error.into_message();

            if let Some(proxy) = crate::link::proxy::resolve_link_proxy() {
                let proxied = EdgeHttpClient::via_proxy(&proxy)?;
                match probe_edge_transport(&proxied, edge_origin) {
                    Ok(()) => return Ok(proxied.client),
                    Err(error) if !error.may_retry_via_proxy() => {
                        return Err(error.into_message());
                    }
                    Err(error) => last_transport_error = error.into_message(),
                }
            }

            match trusted_dns_direct_client(edge_origin) {
                Ok(Some(resolved)) => match probe_edge_transport(&resolved, edge_origin) {
                    Ok(()) => {
                        report_workers_dev_hosts_recovery(edge_origin);
                        return Ok(resolved.client);
                    }
                    Err(error) if !error.may_retry_via_proxy() => {
                        return Err(error.into_message());
                    }
                    Err(error) => last_transport_error = error.into_message(),
                },
                Ok(None) => {}
                Err(error) => {
                    last_transport_error = format!(
                        "{last_transport_error}; direct trusted DNS fallback unavailable: {error}"
                    );
                }
            }

            if let Some(socks) = crate::link::proxy::resolve_link_socks_proxy() {
                let proxied = EdgeHttpClient::via_proxy(&socks)?;
                let host = edge_origin_host(edge_origin)?;
                if host.ends_with(".workers.dev") {
                    let ips = resolve_trusted_worker_ips(&proxied, &host).map_err(|error| {
                        format!("{last_transport_error}; trusted DNS fallback unavailable: {error}")
                    })?;
                    let resolved = EdgeHttpClient::via_socks_resolved(&socks, &host, &ips)?;
                    probe_edge_transport(&resolved, edge_origin)
                        .map_err(EdgeHealthProbeError::into_message)?;
                    return Ok(resolved.client);
                }
            }

            Err(last_transport_error)
        }
    }
}

fn probe_edge_transport(
    edge_http: &EdgeHttpClient,
    edge_origin: &str,
) -> Result<(), EdgeHealthProbeError> {
    let response = edge_http
        .client
        .get(format!("{edge_origin}/health"))
        .send()
        .map_err(|error| {
            EdgeHealthProbeError::Transport(format!("Worker HTTP preflight failed: {error}"))
        })?;
    if !response.status().is_success() {
        return Err(edge_health_http_error(
            "Worker HTTP preflight",
            response.status(),
        ));
    }
    let payload: Value = response.json().map_err(|_| {
        EdgeHealthProbeError::Validation("Worker HTTP preflight returned non-JSON".to_owned())
    })?;
    validate_edge_transport_payload(&payload, edge_origin).map_err(EdgeHealthProbeError::Validation)
}

fn validate_edge_transport_payload(payload: &Value, edge_origin: &str) -> Result<(), String> {
    let service = payload
        .get("service")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Worker HTTP preflight returned no service identity".to_owned())?;
    match expected_worker_service_from_origin(edge_origin)? {
        Some(expected) if service != expected => {
            return Err(
                "Worker HTTP preflight service identity does not match Worker origin".to_owned(),
            );
        }
        Some(_) => {}
        None if !valid_worker_name(service) || !service.starts_with("herdr-edge-") => {
            return Err("Worker HTTP preflight service identity is not a Herdr Worker".to_owned());
        }
        None => {}
    }
    validate_health_payload(payload, service, None)
}

fn expected_worker_service_from_origin(edge_origin: &str) -> Result<Option<String>, String> {
    let host = edge_origin_host(edge_origin)?;
    let Some(prefix) = host.strip_suffix(".workers.dev") else {
        return Ok(None);
    };
    let (worker, account_subdomain) = prefix
        .split_once('.')
        .ok_or_else(|| "workers.dev Edge origin is missing its account subdomain".to_owned())?;
    if !valid_worker_name(worker) || account_subdomain.is_empty() {
        return Err("workers.dev Edge origin has an invalid Worker identity".to_owned());
    }
    Ok(Some(worker.to_owned()))
}

fn trusted_dns_direct_client(edge_origin: &str) -> Result<Option<EdgeHttpClient>, String> {
    let host = edge_origin_host(edge_origin)?;
    if !host.ends_with(".workers.dev") {
        return Ok(None);
    }

    // Prefer ordinary HTTPS to trusted DoH hostnames. This handles selective workers.dev
    // poisoning without depending on a Herdr relay or a configured local proxy.
    let named_result =
        EdgeHttpClient::direct().and_then(|dns| resolve_trusted_worker_ips(&dns, &host));
    if let Ok(ips) = named_result.as_ref() {
        return EdgeHttpClient::direct_resolved(&host, ips).map(Some);
    }

    // If local DNS is unavailable more broadly, pin the DoH resolver itself to its public
    // anycast addresses while preserving TLS SNI and hostname validation.
    let pinned_result = resolve_pinned_trusted_worker_ips(&host);
    if let Ok(ips) = pinned_result.as_ref() {
        return EdgeHttpClient::direct_resolved(&host, ips).map(Some);
    }

    // A detected local proxy remains an optional final resolver path.
    if let Some(proxy) = crate::link::proxy::resolve_link_proxy() {
        let proxied = EdgeHttpClient::via_proxy(&proxy)?;
        if let Ok(ips) = resolve_trusted_worker_ips(&proxied, &host) {
            return EdgeHttpClient::direct_resolved(&host, &ips).map(Some);
        }
    }

    Err(format!(
        "trusted DNS fallback unavailable: hostname DoH failed: {}; pinned DoH failed: {}",
        named_result.unwrap_err(),
        pinned_result.unwrap_err()
    ))
}

fn cloudflare_doh_bootstrap_ips() -> [IpAddr; 2] {
    [
        IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
        IpAddr::V4(Ipv4Addr::new(1, 0, 0, 1)),
    ]
}

fn google_doh_bootstrap_ips() -> [IpAddr; 2] {
    [
        IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
        IpAddr::V4(Ipv4Addr::new(8, 8, 4, 4)),
    ]
}

fn resolve_pinned_trusted_worker_ips(host: &str) -> Result<Vec<IpAddr>, String> {
    let cloudflare = EdgeHttpClient::cloudflare_dns()
        .and_then(|dns| resolve_worker_ips_with_doh(&dns, CLOUDFLARE_DOH_HOST, "/dns-query", host));
    if let Ok(ips) = cloudflare.as_ref() {
        return Ok(ips.clone());
    }
    let google = EdgeHttpClient::google_dns()
        .and_then(|dns| resolve_worker_ips_with_doh(&dns, GOOGLE_DOH_HOST, "/resolve", host));
    if let Ok(ips) = google.as_ref() {
        return Ok(ips.clone());
    }
    Err(format!(
        "Cloudflare DNS failed: {}; Google DNS failed: {}",
        cloudflare.unwrap_err(),
        google.unwrap_err()
    ))
}

fn edge_origin_host(edge_origin: &str) -> Result<String, String> {
    let parsed = edge_origin
        .parse::<url::Url>()
        .map_err(|_| "bootstrap Edge origin is not a valid URL".to_owned())?;
    if parsed.scheme() != "https" {
        return Err("bootstrap Edge origin must use HTTPS".to_owned());
    }
    parsed
        .host_str()
        .map(str::to_owned)
        .ok_or_else(|| "bootstrap Edge origin has no hostname".to_owned())
}

fn resolve_trusted_worker_ips(
    edge_http: &EdgeHttpClient,
    host: &str,
) -> Result<Vec<IpAddr>, String> {
    let cloudflare =
        resolve_worker_ips_with_doh(edge_http, CLOUDFLARE_DOH_HOST, "/dns-query", host);
    if let Ok(ips) = cloudflare.as_ref() {
        return Ok(ips.clone());
    }
    let google = resolve_worker_ips_with_doh(edge_http, GOOGLE_DOH_HOST, "/resolve", host);
    if let Ok(ips) = google.as_ref() {
        return Ok(ips.clone());
    }
    Err(format!(
        "Cloudflare DNS failed: {}; Google DNS failed: {}",
        cloudflare.unwrap_err(),
        google.unwrap_err()
    ))
}

fn resolve_worker_ips_with_doh(
    edge_http: &EdgeHttpClient,
    doh_host: &str,
    doh_path: &str,
    host: &str,
) -> Result<Vec<IpAddr>, String> {
    let mut doh_url = format!("https://{doh_host}{doh_path}")
        .parse::<url::Url>()
        .map_err(|_| "trusted DNS endpoint is invalid".to_owned())?;
    doh_url
        .query_pairs_mut()
        .append_pair("name", host)
        .append_pair("type", "A");
    let response = edge_http
        .client
        .get(doh_url)
        .header(reqwest::header::ACCEPT, "application/dns-json")
        .send()
        .map_err(|error| format!("{doh_host} query failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "{doh_host} returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let payload: Value = response
        .json()
        .map_err(|_| format!("{doh_host} returned non-JSON"))?;
    let ips = parse_trusted_dns_ipv4_answers(&payload);
    if ips.is_empty() {
        return Err(format!(
            "{doh_host} returned no IPv4 address for Worker hostname"
        ));
    }
    Ok(ips)
}

fn report_workers_dev_hosts_recovery(edge_origin: &str) {
    match persist_workers_dev_hosts_mapping(edge_origin) {
        Ok(true) => eprintln!(
            "workers.dev DNS recovery: persisted a verified direct mapping in the system hosts file; subsequent Link attempts can stay on the direct route"
        ),
        Ok(false) => {}
        Err(error) => eprintln!(
            "workers.dev DNS recovery: trusted-DNS direct access works, but the verified hosts mapping could not be persisted: {error}"
        ),
    }
}

fn persist_workers_dev_hosts_mapping(edge_origin: &str) -> Result<bool, String> {
    let host = edge_origin_host(edge_origin)?;
    if !host.ends_with(".workers.dev") {
        return Ok(false);
    }
    let ips = EdgeHttpClient::direct()
        .and_then(|dns| resolve_trusted_worker_ips(&dns, &host))
        .or_else(|_| resolve_pinned_trusted_worker_ips(&host))?;
    let resolved = EdgeHttpClient::direct_resolved(&host, &ips)?;
    probe_edge_transport(&resolved, edge_origin).map_err(EdgeHealthProbeError::into_message)?;

    let hosts_path = system_hosts_path()?;
    let current = fs::read_to_string(&hosts_path)
        .map_err(|error| format!("cannot read {}: {error}", hosts_path.display()))?;
    let updated = rewrite_managed_hosts_content(&current, &host, &ips)?;
    if updated == current {
        return Ok(false);
    }
    write_system_hosts(&hosts_path, &updated)?;
    let verified = fs::read_to_string(&hosts_path).map_err(|error| {
        format!(
            "cannot verify {} after update: {error}",
            hosts_path.display()
        )
    })?;
    let marker = managed_hosts_marker(&host);
    if !verified
        .lines()
        .any(|line| line.trim_end().ends_with(&marker))
    {
        return Err(format!(
            "{} did not contain the managed mapping after update",
            hosts_path.display()
        ));
    }
    Ok(true)
}

fn managed_hosts_marker(host: &str) -> String {
    format!("{MANAGED_HOSTS_MARKER} {host}")
}

fn rewrite_managed_hosts_content(
    current: &str,
    host: &str,
    ips: &[IpAddr],
) -> Result<String, String> {
    let marker = managed_hosts_marker(host);
    let line_ending = if current.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut kept = Vec::new();
    for line in current.lines() {
        if line.trim_end().ends_with(&marker) {
            continue;
        }
        let data = line.split('#').next().unwrap_or_default();
        let mut fields = data.split_whitespace();
        let _address = fields.next();
        if fields.any(|field| field.eq_ignore_ascii_case(host)) {
            return Err(format!(
                "an unmanaged hosts entry already exists for {host}; refusing to overwrite it"
            ));
        }
        kept.push(line);
    }

    let mut addresses = Vec::new();
    for ip in ips.iter().copied().filter(IpAddr::is_ipv4) {
        if !addresses.contains(&ip) {
            addresses.push(ip);
        }
        if addresses.len() >= 4 {
            break;
        }
    }
    if addresses.is_empty() {
        return Err("trusted DNS returned no IPv4 address suitable for hosts recovery".to_owned());
    }

    while kept.last().is_some_and(|line| line.is_empty()) {
        kept.pop();
    }
    let mut updated = kept.join(line_ending);
    if !updated.is_empty() {
        updated.push_str(line_ending);
    }
    for ip in addresses {
        updated.push_str(&format!("{ip}\t{host}\t{marker}{line_ending}"));
    }
    Ok(updated)
}

#[cfg(unix)]
fn system_hosts_path() -> Result<PathBuf, String> {
    Ok(PathBuf::from("/etc/hosts"))
}

#[cfg(windows)]
fn system_hosts_path() -> Result<PathBuf, String> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    Ok(root
        .join("System32")
        .join("drivers")
        .join("etc")
        .join("hosts"))
}

#[cfg(not(any(unix, windows)))]
fn system_hosts_path() -> Result<PathBuf, String> {
    Err("system hosts recovery is unsupported on this platform".to_owned())
}

#[cfg(unix)]
fn write_system_hosts(path: &Path, content: &str) -> Result<(), String> {
    match fs::write(path, content) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
        Err(error) => return Err(format!("cannot write {}: {error}", path.display())),
    }

    let temp = std::env::temp_dir().join(format!(
        "herdr-mcp-hosts-{}-{}.tmp",
        std::process::id(),
        now_ms()
    ));
    fs::write(&temp, content)
        .map_err(|error| format!("cannot prepare hosts recovery file: {error}"))?;

    let try_sudo = |non_interactive: bool| -> Result<bool, String> {
        let mut command = Command::new("sudo");
        if non_interactive {
            command
                .arg("-n")
                .stdout(Stdio::null())
                .stderr(Stdio::null());
        }
        let status = command
            .arg("cp")
            .arg(&temp)
            .arg(path)
            .status()
            .map_err(|error| format!("cannot run sudo for hosts recovery: {error}"))?;
        Ok(status.success())
    };

    let mut written = try_sudo(true).unwrap_or(false);
    if !written && io::stdin().is_terminal() && io::stderr().is_terminal() {
        written = try_sudo(false)?;
    }
    let _ = fs::remove_file(&temp);
    if written {
        Ok(())
    } else {
        Err(format!(
            "administrator approval is required to update {}; rerun the Worker bootstrap/connect command in an interactive terminal and approve the sudo prompt",
            path.display()
        ))
    }
}

#[cfg(windows)]
fn write_system_hosts(path: &Path, content: &str) -> Result<(), String> {
    fs::write(path, content).map_err(|error| {
        format!(
            "cannot update {}: {error}; rerun Worker bootstrap/connect from an elevated PowerShell or Terminal",
            path.display()
        )
    })
}

#[cfg(not(any(unix, windows)))]
fn write_system_hosts(_path: &Path, _content: &str) -> Result<(), String> {
    Err("system hosts recovery is unsupported on this platform".to_owned())
}

fn parse_trusted_dns_ipv4_answers(payload: &Value) -> Vec<IpAddr> {
    if payload.get("Status").and_then(Value::as_i64) != Some(0) {
        return Vec::new();
    }
    let mut ips = Vec::new();
    let Some(answers) = payload.get("Answer").and_then(Value::as_array) else {
        return ips;
    };
    for answer in answers {
        if answer.get("type").and_then(Value::as_i64) != Some(1) {
            continue;
        }
        let Some(value) = answer.get("data").and_then(Value::as_str) else {
            continue;
        };
        let Ok(ip) = value.parse::<IpAddr>() else {
            continue;
        };
        if ip.is_ipv4() && !ips.contains(&ip) {
            ips.push(ip);
        }
        if ips.len() >= 8 {
            break;
        }
    }
    ips
}

fn probe_health(
    edge_http: &EdgeHttpClient,
    edge_origin: &str,
    worker_name: &str,
    expected_version: Option<&str>,
) -> Result<(), EdgeHealthProbeError> {
    let response = edge_http
        .client
        .get(format!("{edge_origin}/health"))
        .send()
        .map_err(|error| {
            EdgeHealthProbeError::Transport(format!("Worker health probe failed: {error}"))
        })?;
    if !response.status().is_success() {
        return Err(edge_health_http_error(
            "Worker health probe",
            response.status(),
        ));
    }
    let payload: Value = response.json().map_err(|_| {
        EdgeHealthProbeError::Validation("Worker health returned non-JSON".to_owned())
    })?;
    validate_health_payload(&payload, worker_name, expected_version)
        .map_err(EdgeHealthProbeError::Validation)
}

fn edge_health_http_error(context: &str, status: reqwest::StatusCode) -> EdgeHealthProbeError {
    let message = format!("{context} returned HTTP {}", status.as_u16());
    if status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
    {
        EdgeHealthProbeError::Transport(message)
    } else {
        EdgeHealthProbeError::Validation(message)
    }
}

fn validate_health_payload(
    payload: &Value,
    worker_name: &str,
    expected_version: Option<&str>,
) -> Result<(), String> {
    if payload.get("ok").and_then(Value::as_bool) != Some(true)
        || payload.get("service").and_then(Value::as_str) != Some(worker_name)
    {
        return Err("Worker health does not prove Herdr ownership".to_owned());
    }
    let contract = crate::link::edge_contract::parse_edge_health_contract(&payload.to_string())
        .map_err(|error| format!("Worker health runtime contract is invalid: {error}"))?;
    if !crate::link::edge_contract::rust_link_accepts_edge_contract(&contract) {
        return Err(crate::link::edge_contract::refuse_edge_for_rust_link(&contract).to_string());
    }
    if let Some(expected) = expected_version
        && payload.get("edgeVersion").and_then(Value::as_str) != Some(expected)
    {
        return Err(
            "Worker health version does not match the installed release runtime".to_owned(),
        );
    }
    Ok(())
}

fn verify_public_oauth(edge_http: &EdgeHttpClient, edge_origin: &str) -> Result<(), String> {
    for path in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/oauth-protected-resource",
    ] {
        let response = edge_http
            .client
            .get(format!("{edge_origin}{path}"))
            .send()
            .map_err(|error| format!("OAuth discovery probe failed for {path}: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "OAuth discovery {path} returned HTTP {}",
                response.status().as_u16()
            ));
        }
    }
    let response = edge_http
        .client
        .get(format!("{edge_origin}/mcp"))
        .send()
        .map_err(|error| format!("unauthenticated MCP probe failed: {error}"))?;
    if response.status().as_u16() != 401 {
        return Err(format!(
            "unauthenticated /mcp expected HTTP 401, got {}",
            response.status().as_u16()
        ));
    }
    Ok(())
}

fn worker_name() -> Result<String, String> {
    let output = Command::new("hostname")
        .output()
        .map_err(|error| format!("cannot read system hostname: {error}"))?;
    let raw = String::from_utf8_lossy(&output.stdout);
    let mut slug = String::new();
    let mut previous_dash = false;
    for ch in raw.trim().to_ascii_lowercase().chars() {
        let mapped = if ch.is_ascii_alphanumeric() { ch } else { '-' };
        if mapped == '-' {
            if previous_dash {
                continue;
            }
            previous_dash = true;
        } else {
            previous_dash = false;
        }
        slug.push(mapped);
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        return Err("cannot derive Cloudflare Worker name from hostname".to_owned());
    }
    let max_slug = 63usize.saturating_sub("herdr-edge-".len());
    let slug = &slug[..slug.len().min(max_slug)];
    let name = format!("herdr-edge-{}", slug.trim_matches('-'));
    if valid_worker_name(&name) {
        Ok(name)
    } else {
        Err("derived Cloudflare Worker name is invalid".to_owned())
    }
}

fn valid_worker_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn subdomain_candidate(account_id: &str) -> String {
    let short: String = account_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(12)
        .collect();
    format!("herdr-{}", short.to_ascii_lowercase())
}

fn random_secret(bytes: usize) -> Result<SecretBytes, String> {
    let mut raw = vec![0u8; bytes];
    getrandom::fill(&mut raw)
        .map_err(|error| format!("secure random generation failed: {error}"))?;
    let mut out = String::with_capacity(bytes * 2);
    for byte in &raw {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").map_err(|error| error.to_string())?;
    }
    raw.fill(0);
    SecretBytes::from_string(out)
}

fn journal_path(paths: &RuntimePaths) -> PathBuf {
    paths.config_dir.join("bootstrap").join(JOURNAL_FILE)
}

fn read_journal(path: &Path) -> Result<Option<BootstrapJournal>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read bootstrap journal: {error}"))?;
    let journal: BootstrapJournal = serde_json::from_slice(&bytes)
        .map_err(|error| format!("bootstrap journal is invalid: {error}"))?;
    if journal.schema != JOURNAL_SCHEMA {
        return Err("bootstrap journal schema is not supported by this runtime".to_owned());
    }
    Ok(Some(journal))
}

fn write_journal(path: &Path, journal: &BootstrapJournal) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create bootstrap journal directory: {error}"))?;
    }
    let mut bytes = serde_json::to_vec_pretty(journal).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    write_private_file(&tmp, &bytes)?;
    fs::rename(&tmp, path)
        .map_err(|error| format!("cannot atomically replace bootstrap journal: {error}"))?;
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("cannot sync {}: {error}", path.display()))?;
    Ok(())
}

fn validate_journal(
    journal: &BootstrapJournal,
    source_commit: &str,
    runtime_version: &str,
    account_id: &str,
    worker_name: &str,
) -> Result<(), String> {
    if journal.source_commit != source_commit
        || journal.runtime_version != runtime_version
        || journal.account_id != account_id
        || journal.worker_name != worker_name
    {
        return Err("existing bootstrap journal belongs to a different release/account/Worker; refusing to mutate until it is resolved".to_owned());
    }
    Ok(())
}

fn current_runtime_generation() -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    fs::read_link(home.join(".config/herdr-mcp/runtime/current"))
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|value| value.to_string_lossy().into_owned())
        })
}

fn form_body(values: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in values {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

fn required_string(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("response is missing required field {key}"))
}

fn cloudflare_error_summary(payload: &Value) -> String {
    let errors = payload.get("errors").and_then(Value::as_array);
    let mut parts = Vec::new();
    if let Some(errors) = errors {
        for error in errors.iter().take(3) {
            let code = error.get("code").and_then(Value::as_i64);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("request rejected");
            parts.push(match code {
                Some(code) => format!("{code}: {message}"),
                None => message.to_owned(),
            });
        }
    }
    if parts.is_empty() {
        "request rejected".to_owned()
    } else {
        parts.join("; ")
    }
}

fn workers_subdomain_is_absent(status: reqwest::StatusCode, payload: &Value) -> bool {
    let success = payload
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(status.is_success());
    if status.is_success() && success {
        return false;
    }
    payload
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|error| error.get("code").and_then(Value::as_i64) == Some(10007))
}

fn sanitize_error(value: &str, token: &SecretBytes) -> String {
    let without_exact = token
        .expose()
        .map(|secret| value.replace(secret, "[REDACTED]"))
        .unwrap_or_else(|_| "[REDACTED]".to_owned());
    let token_sanitized = crate::status::sanitize_probe_token(&without_exact);
    if token_sanitized == "redacted" {
        "redacted Cloudflare credential error".to_owned()
    } else {
        crate::runtime_meta::redact_command_summary(&token_sanitized)
    }
}

fn verify_api_token_shape(value: &str) -> bool {
    value.len() >= 20 && value.len() <= 4096 && !value.chars().any(char::is_whitespace)
}

fn valid_account_id(value: &str) -> bool {
    (16..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn valid_source_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn short_id(value: &str) -> String {
    value.chars().take(8).collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn open_browser(_url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("/usr/bin/open")
            .arg(_url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

fn stdin_is_tty() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn read_hidden_line(prompt: &str) -> Result<String, String> {
    if !stdin_is_tty() {
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_owned());
    }
    eprint!("{prompt}");
    io::stderr().flush().map_err(|error| error.to_string())?;
    #[cfg(unix)]
    unsafe {
        let fd = libc::STDIN_FILENO;
        let mut original: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut original) != 0 {
            return Err("cannot read terminal attributes for hidden credential input".to_owned());
        }
        let mut hidden = original;
        hidden.c_lflag &= !libc::ECHO;
        if libc::tcsetattr(fd, libc::TCSANOW, &hidden) != 0 {
            return Err("cannot disable terminal echo for credential input".to_owned());
        }
        let mut line = String::new();
        let read = io::stdin().read_line(&mut line);
        let restore = libc::tcsetattr(fd, libc::TCSANOW, &original);
        eprintln!();
        if restore != 0 {
            return Err("could not restore terminal echo after credential input".to_owned());
        }
        read.map_err(|error| error.to_string())?;
        Ok(line.trim_end_matches(['\r', '\n']).to_owned())
    }
    #[cfg(not(unix))]
    {
        Err("hidden API-token input is unavailable on this platform; set CLOUDFLARE_API_TOKEN in the current process".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_worker_bootstrap_supports_macos_and_linux() {
        assert!(bootstrap_platform_supported("macos"));
        assert!(bootstrap_platform_supported("linux"));
        assert!(!bootstrap_platform_supported("windows"));
    }

    #[test]
    fn mutation_gate_refuses_before_classification() {
        let gate = MutationGate::default();
        assert!(
            gate.require("deploy")
                .unwrap_err()
                .contains("before fleet classification")
        );
        let mut gate = gate;
        gate.mark_classified();
        assert!(gate.require("deploy").is_ok());
    }

    #[test]
    fn first_fleet_classification_is_conservative() {
        let scripts = vec![Script {
            name: "other-worker".to_owned(),
        }];
        assert_eq!(
            classify_fleet(&scripts, Some("example"), "herdr-edge-mac", None).unwrap(),
            FleetClassification::FirstFleet
        );
        let occupied = vec![Script {
            name: "herdr-edge-mac".to_owned(),
        }];
        assert_eq!(
            classify_fleet(&occupied, None, "herdr-edge-mac", None).unwrap(),
            FleetClassification::AmbiguousTarget
        );
    }

    #[test]
    fn matching_journal_allows_owned_resume() {
        let journal = BootstrapJournal {
            schema: 1,
            phase: Phase::WorkerDeployed,
            source_commit: "a".repeat(40),
            runtime_version: "0.4.6".to_owned(),
            account_id: "1234567890abcdef".to_owned(),
            worker_name: "herdr-edge-mac".to_owned(),
            workers_dev_origin: None,
            canonical_device_id: None,
            expected_runtime_generation: None,
            updated_at_ms: 1,
        };
        let scripts = vec![Script {
            name: "herdr-edge-mac".to_owned(),
        }];
        assert_eq!(
            classify_fleet(&scripts, None, "herdr-edge-mac", Some(&journal)).unwrap(),
            FleetClassification::ResumeOwned
        );
    }

    #[test]
    fn network_interruption_after_worker_deploy_resumes_only_after_readback_proof() {
        assert!(resumable_target_observation(true, false, true));
        assert!(!resumable_target_observation(true, false, false));
        assert!(!resumable_target_observation(false, false, true));
    }

    #[test]
    fn health_contract_accepts_public_epoch_three_with_runtime_epoch_two() {
        let payload = json!({
            "ok": true,
            "service": "herdr-edge-mac",
            "edgeVersion": "0.4.6-dev",
            "contractEpoch": 3,
            "contractHash": "sha256:public-v3",
            "runtimeContractEpoch": 2,
            "runtimeContractHash": crate::link::daemon::PUBLIC_CONTRACT_HASH,
        });
        assert!(validate_health_payload(&payload, "herdr-edge-mac", Some("0.4.6-dev")).is_ok());
    }

    #[test]
    fn only_transport_health_failure_may_fall_back_to_proxy() {
        assert!(EdgeHealthProbeError::Transport("timeout".to_owned()).may_retry_via_proxy());
        assert!(
            !EdgeHealthProbeError::Validation("wrong contract".to_owned()).may_retry_via_proxy()
        );

        for status in [408, 429, 500, 502, 503, 504, 524] {
            assert!(
                edge_health_http_error("health", reqwest::StatusCode::from_u16(status).unwrap())
                    .may_retry_via_proxy(),
                "HTTP {status} should remain route-retryable for a read-only health probe"
            );
        }
        for status in [400, 401, 403, 404, 409] {
            assert!(
                !edge_health_http_error("health", reqwest::StatusCode::from_u16(status).unwrap())
                    .may_retry_via_proxy(),
                "HTTP {status} should fail closed"
            );
        }
    }

    #[test]
    fn management_health_preflight_binds_workers_dev_service_and_fails_closed() {
        let payload = json!({
            "ok": true,
            "service": "herdr-edge-mac",
            "contractEpoch": 2,
            "contractHash": crate::link::daemon::PUBLIC_CONTRACT_HASH,
        });
        assert!(
            validate_edge_transport_payload(&payload, "https://herdr-edge-mac.example.workers.dev")
                .is_ok()
        );
        assert!(
            validate_edge_transport_payload(
                &payload,
                "https://herdr-edge-other.example.workers.dev"
            )
            .unwrap_err()
            .contains("does not match Worker origin")
        );
        assert_eq!(
            expected_worker_service_from_origin("https://herdr-edge-mac.example.workers.dev")
                .unwrap()
                .as_deref(),
            Some("herdr-edge-mac")
        );
    }

    #[test]
    fn management_health_preflight_rejects_non_herdr_custom_domain_service() {
        let payload = json!({
            "ok": true,
            "service": "other-service",
            "contractEpoch": 2,
            "contractHash": crate::link::daemon::PUBLIC_CONTRACT_HASH,
        });
        assert!(
            validate_edge_transport_payload(&payload, "https://mcp.example.com")
                .unwrap_err()
                .contains("not a Herdr Worker")
        );
    }

    #[test]
    fn trusted_dns_parser_accepts_only_unique_ipv4_answers() {
        let payload = json!({
            "Status": 0,
            "Answer": [
                {"type": 1, "data": "104.21.75.107"},
                {"type": 28, "data": "2606:4700:3030::6815:4b6b"},
                {"type": 1, "data": "172.67.221.89"},
                {"type": 1, "data": "104.21.75.107"},
                {"type": 5, "data": "example.invalid"}
            ]
        });
        assert_eq!(
            parse_trusted_dns_ipv4_answers(&payload),
            vec![
                "104.21.75.107".parse::<IpAddr>().unwrap(),
                "172.67.221.89".parse::<IpAddr>().unwrap()
            ]
        );
        assert!(parse_trusted_dns_ipv4_answers(&json!({"Status": 2})).is_empty());

        assert_eq!(
            cloudflare_doh_bootstrap_ips(),
            [
                "1.1.1.1".parse::<IpAddr>().unwrap(),
                "1.0.0.1".parse::<IpAddr>().unwrap(),
            ]
        );
        assert_eq!(
            google_doh_bootstrap_ips(),
            [
                "8.8.8.8".parse::<IpAddr>().unwrap(),
                "8.8.4.4".parse::<IpAddr>().unwrap(),
            ]
        );
        assert!(
            EdgeHttpClient::direct_resolved(
                "herdr-edge-dnsless.example.workers.dev",
                &["203.0.113.10".parse::<IpAddr>().unwrap()],
            )
            .is_ok()
        );
    }

    #[test]
    fn managed_hosts_recovery_replaces_only_herdr_owned_entries() {
        let host = "herdr-edge-dnsless.example.workers.dev";
        let current =
            format!("127.0.0.1 localhost\n203.0.113.9 {host} # herdr-mcp workers.dev {host}\n");
        let updated = rewrite_managed_hosts_content(
            &current,
            host,
            &[
                "104.21.75.107".parse::<IpAddr>().unwrap(),
                "172.67.221.89".parse::<IpAddr>().unwrap(),
            ],
        )
        .unwrap();
        assert!(updated.contains("127.0.0.1 localhost"));
        assert!(!updated.contains("203.0.113.9"));
        assert!(updated.contains(&format!("104.21.75.107\t{host}")));
        assert!(updated.contains(&format!("172.67.221.89\t{host}")));
    }

    #[test]
    fn managed_hosts_recovery_refuses_unowned_worker_entry() {
        let host = "herdr-edge-dnsless.example.workers.dev";
        let error = rewrite_managed_hosts_content(
            &format!("203.0.113.7 {host} # user managed\n"),
            host,
            &["104.21.75.107".parse::<IpAddr>().unwrap()],
        )
        .unwrap_err();
        assert!(error.contains("unmanaged hosts entry"));
    }

    #[test]
    fn operator_registry_retry_is_limited_to_readiness_failures() {
        for status in [401, 403, 500, 502, 503] {
            assert!(operator_registry_status_is_retryable(
                reqwest::StatusCode::from_u16(status).unwrap()
            ));
        }
        for status in [400, 404, 409, 429] {
            assert!(!operator_registry_status_is_retryable(
                reqwest::StatusCode::from_u16(status).unwrap()
            ));
        }
    }

    #[test]
    fn first_pairing_retry_is_only_for_pre_mutation_pepper_readiness() {
        assert!(first_pairing_create_is_retryable(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "code": "pairing_unavailable"})
        ));
        assert!(!first_pairing_create_is_retryable(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "code": "internal_error"})
        ));
        assert!(!first_pairing_create_is_retryable(
            reqwest::StatusCode::CONFLICT,
            &json!({"ok": false, "code": "first_fleet_not_empty"})
        ));
        assert!(!first_pairing_create_is_retryable(
            reqwest::StatusCode::UNAUTHORIZED,
            &json!({"ok": false, "code": "pairing_unavailable"})
        ));
    }

    #[test]
    fn same_name_worker_without_resume_proof_is_not_silently_skipped() {
        let occupied = vec![Script {
            name: "herdr-edge-mac".to_owned(),
        }];
        assert_eq!(
            classify_fleet(&occupied, None, "herdr-edge-mac", None).unwrap(),
            FleetClassification::AmbiguousTarget
        );
    }

    #[test]
    fn under_scoped_token_failure_cannot_open_mutation_gate() {
        let gate = MutationGate::default();
        let preflight: Result<(), String> =
            Err("Cloudflare API HTTP 403: missing Workers Scripts -> Edit permission".to_owned());
        assert!(preflight.is_err());
        assert!(gate.require("deploy Worker").is_err());
    }

    #[test]
    fn fresh_account_missing_workers_subdomain_is_createable_state() {
        let missing = json!({
            "success": false,
            "errors": [{
                "code": 10007,
                "message": "You do not have a workers.dev subdomain."
            }],
            "result": null
        });
        assert!(workers_subdomain_is_absent(
            reqwest::StatusCode::NOT_FOUND,
            &missing
        ));

        let forbidden = json!({
            "success": false,
            "errors": [{"code": 10000, "message": "Authentication error"}],
            "result": null
        });
        assert!(!workers_subdomain_is_absent(
            reqwest::StatusCode::FORBIDDEN,
            &forbidden
        ));
    }

    #[test]
    fn temporary_operator_delete_failure_is_terminal_and_ttl_is_bounded() {
        let names = vec![
            TEMP_OPERATOR_SECRET.to_owned(),
            TEMP_OPERATOR_EXPIRY_SECRET.to_owned(),
        ];
        let error = remove_temporary_operator(
            |name| {
                if name == TEMP_OPERATOR_SECRET {
                    Err("simulated Cloudflare delete failure".to_owned())
                } else {
                    Ok(())
                }
            },
            || Ok(names.clone()),
        )
        .unwrap_err();
        assert!(error.contains("NOT complete"));
        assert!(error.contains("bounded TTL"));
        let now = now_ms();
        let expiry = temporary_operator_expiry_ms();
        assert!(expiry > now);
        assert!(expiry.saturating_sub(now) <= TEMP_OPERATOR_TTL.as_millis() as u64 + 1_000);
    }

    #[test]
    fn journal_serialization_contains_no_secret_fields() {
        let journal = BootstrapJournal::new(
            "a".repeat(40),
            "0.4.6".to_owned(),
            "1234567890abcdef".to_owned(),
            "herdr-edge-mac".to_owned(),
        );
        let text = serde_json::to_string(&journal).unwrap();
        for forbidden in [
            "token",
            "secret",
            "pairing_code",
            "bearer",
            "LINK_SHARED_SECRET",
        ] {
            assert!(
                !text.contains(forbidden),
                "journal leaked forbidden field marker: {forbidden}"
            );
        }
    }

    #[test]
    fn credentials_are_redacted_from_debug_output_logs_status_and_journal() {
        let access_literal = "cf_access_012345678901234567890123456789";
        let refresh_literal = "cf_refresh_012345678901234567890123456789";
        let access = SecretBytes::from_string(access_literal.to_owned()).unwrap();
        let refresh = SecretBytes::from_string(refresh_literal.to_owned()).unwrap();

        let debug = format!("{access:?} {refresh:?}");
        assert!(!debug.contains(access_literal));
        assert!(!debug.contains(refresh_literal));

        let output = sanitize_error(
            &format!("request failed Authorization: Bearer {access_literal}"),
            &access,
        );
        assert!(!output.contains(access_literal));

        let log = crate::runtime_meta::redact_command_summary(&format!(
            "cloudflare worker upload --token {access_literal}"
        ));
        assert!(!log.contains(access_literal));

        let status = crate::status::sanitize_probe_token(&format!("Bearer {refresh_literal}"));
        assert!(!status.contains(refresh_literal));

        let journal = BootstrapJournal::new(
            "a".repeat(40),
            "0.4.6".to_owned(),
            "1234567890abcdef".to_owned(),
            "herdr-edge-mac".to_owned(),
        );
        let journal_text = serde_json::to_string(&journal).unwrap();
        assert!(!journal_text.contains(access_literal));
        assert!(!journal_text.contains(refresh_literal));
    }

    #[test]
    fn sanitizer_removes_exact_cloudflare_credential() {
        let secret = SecretBytes::from_string("012345678901234567890123456789".to_owned()).unwrap();
        let raw = format!(
            "request failed Authorization: Bearer {} --token {}",
            secret.expose().unwrap(),
            secret.expose().unwrap()
        );
        let sanitized = sanitize_error(&raw, &secret);
        assert!(!sanitized.contains(secret.expose().unwrap()));
    }

    #[test]
    fn direct_upload_metadata_is_core_only_and_has_no_fake_workstation() {
        let metadata = worker_upload_metadata(
            "herdr-edge-mac",
            "https://herdr-edge-mac.example.workers.dev",
            "0.4.6",
            true,
        )
        .unwrap();
        assert_eq!(
            metadata.get("main_module").and_then(Value::as_str),
            Some(EDGE_MAIN_MODULE)
        );
        let text = metadata.to_string();
        assert!(!text.contains("DEFAULT_WORKSTATION_ID"));
        assert!(!text.contains("ARTIFACT_BUCKET"));
        assert!(text.contains("WORKSTATION_DO"));
        assert!(text.contains("OAUTH_STORE_DO"));
        assert!(text.contains("DEVICE_REGISTRY_DO"));
        assert!(text.contains("\"EDGE_VERSION\",\"text\":\"0.4.6\""));
        assert_eq!(
            metadata
                .pointer("/migrations/new_tag")
                .and_then(Value::as_str),
            Some("v3")
        );
    }

    #[test]
    fn existing_worker_direct_upload_does_not_repeat_migrations() {
        let metadata = worker_upload_metadata(
            "herdr-edge-mac",
            "https://herdr-edge-mac.example.workers.dev",
            "0.4.6",
            false,
        )
        .unwrap();
        assert!(metadata.get("migrations").is_none());
    }

    #[test]
    fn oauth_client_and_scope_contract_matches_current_cloudflare_device_flow() {
        assert_eq!(CLOUDFLARE_CLIENT_ID, "54d11594-84e4-41aa-b438-e81b8fa78ee7");
        for scope in [
            "account:read",
            "user:read",
            "workers_scripts:write",
            "offline_access",
        ] {
            assert!(
                CLOUDFLARE_SCOPES
                    .split_whitespace()
                    .any(|candidate| candidate == scope)
            );
        }
        assert!(!CLOUDFLARE_SCOPES.contains("r2"));
    }

    #[test]
    fn token_shape_helper_is_strict_enough_for_fallback_prompt() {
        assert!(verify_api_token_shape("01234567890123456789"));
        assert!(!verify_api_token_shape("too-short"));
        assert!(!verify_api_token_shape("01234567890123456789 with-space"));
    }
}
