use crate::paths::RuntimePaths;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CLOUDFLARE_API: &str = "https://api.cloudflare.com/client/v4";
const CLOUDFLARE_DEVICE_AUTH: &str = "https://dash.cloudflare.com/oauth2/device/auth";
const CLOUDFLARE_TOKEN: &str = "https://dash.cloudflare.com/oauth2/token";
const CLOUDFLARE_CLIENT_ID: &str = "54d11594-84e4-41aa-b438-e81b8fa78ee7";
const CLOUDFLARE_SCOPES: &str = "account:read user:read workers_scripts:write offline_access";
const WRANGLER_VERSION: &str = "4.129.0";
const JOURNAL_SCHEMA: u32 = 1;
const JOURNAL_FILE: &str = "first-worker-v1.json";
const TEMP_OPERATOR_SECRET: &str = "STATIC_MCP_BEARER_SECRET";
const TEMP_OPERATOR_EXPIRY_SECRET: &str = "STATIC_MCP_BEARER_EXPIRES_AT_MS";
const PAIRING_PEPPER_SECRET: &str = "LINK_SHARED_SECRET";
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
const DEVICE_FLOW_MAX: Duration = Duration::from_secs(300);
const TEMP_OPERATOR_TTL: Duration = Duration::from_secs(10 * 60);

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

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
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
        let result = self.request(
            reqwest::Method::GET,
            &format!("accounts/{account_id}/workers/subdomain"),
            None,
        )?;
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
    if !cfg!(target_os = "macos") {
        return Err(
            "worker bootstrap currently supports macOS first-device installation only".to_owned(),
        );
    }
    if paths.instance.is_named() {
        return Err("worker bootstrap is available only on the default Herdr instance".to_owned());
    }
    run_inner(paths)
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
    if !script_exists || journal.phase < Phase::WorkerDeployed {
        journal.advance(Phase::WorkerDeploying);
        write_journal(&journal_path, &journal)?;
        let bundle = prepare_edge_bundle(&source_commit)?;
        let deploy_result = deploy_worker(
            &gate,
            &token,
            &account.id,
            &worker_name,
            &edge_origin,
            &runtime_version,
            &bundle.edge_dir,
        );
        let cleanup_result = fs::remove_dir_all(&bundle.root);
        deploy_result?;
        if let Err(error) = cleanup_result {
            eprintln!("warning: could not remove non-secret bootstrap source directory: {error}");
        }
        verify_health(&edge_origin, &worker_name, Some(&runtime_version))?;
        journal.advance(Phase::WorkerDeployed);
        write_journal(&journal_path, &journal)?;
    } else {
        verify_health(&edge_origin, &worker_name, None)?;
        println!("Existing Herdr Worker verified; resuming configuration.");
    }
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

        let current_devices = edge_devices_with_operator(&edge_origin, &operator)?;
        if !current_devices.is_empty() {
            return Err("first-fleet enrollment refused because the Worker device registry is no longer empty; switch to the existing-Worker pairing flow".to_owned());
        }
        let name = crate::device_name::system_device_display_name();
        let enrolled = create_and_consume_first_pairing(&edge_origin, &operator, name.as_deref())?;
        verify_device_fleet_admin(&edge_origin, &enrolled)?;
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

    verify_public_oauth(&edge_origin)?;
    verify_current_device_inventory(paths, journal.canonical_device_id.as_deref())?;
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
    root: PathBuf,
    edge_dir: PathBuf,
}

fn prepare_edge_bundle(source_commit: &str) -> Result<EdgeBundle, String> {
    let root = std::env::temp_dir().join(format!(
        "herdr-bootstrap-{}-{}",
        std::process::id(),
        now_ms()
    ));
    fs::create_dir_all(&root)
        .map_err(|error| format!("cannot create bootstrap source directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure bootstrap source directory: {error}"))?;
    }
    let archive = root.join("source.tar.gz");
    let url = format!("https://codeload.github.com/whshang/herdr-mcp/tar.gz/{source_commit}");
    let mut response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|error| format!("cannot create source download client: {error}"))?
        .get(url)
        .send()
        .map_err(|error| format!("cannot download release-matched Edge source: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "release-matched Edge source download returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&archive)
        .map_err(|error| format!("cannot create bootstrap source archive: {error}"))?;
    io::copy(&mut response, &mut file)
        .map_err(|error| format!("cannot save bootstrap source archive: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("cannot sync bootstrap source archive: {error}"))?;
    let output = Command::new("/usr/bin/tar")
        .args(["-xzf"])
        .arg(&archive)
        .args(["-C"])
        .arg(&root)
        .output()
        .map_err(|error| format!("cannot extract release-matched Edge source: {error}"))?;
    if !output.status.success() {
        return Err("cannot extract release-matched Edge source".to_owned());
    }
    let checkout = fs::read_dir(&root)
        .map_err(|error| format!("cannot inspect extracted Edge source: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.is_dir())
        .ok_or_else(|| "release-matched Edge source archive contained no checkout".to_owned())?;
    let edge_dir = checkout.join("edge").join("cloudflare");
    if !edge_dir.join("src").join("index.ts").is_file()
        || !edge_dir.join("wrangler.user.example.toml").is_file()
    {
        return Err(
            "release-matched source archive is missing the Cloudflare Edge bundle".to_owned(),
        );
    }
    Ok(EdgeBundle { root, edge_dir })
}

fn deploy_worker(
    gate: &MutationGate,
    token: &SecretBytes,
    account_id: &str,
    worker_name: &str,
    edge_origin: &str,
    runtime_version: &str,
    edge_dir: &Path,
) -> Result<(), String> {
    gate.require("deploy Worker")?;
    let example = fs::read_to_string(edge_dir.join("wrangler.user.example.toml"))
        .map_err(|error| format!("cannot read release Wrangler template: {error}"))?;
    let config = render_wrangler_config(&example, worker_name, edge_origin, runtime_version)?;
    let config_path = edge_dir.join("wrangler.user.toml");
    write_private_file(&config_path, config.as_bytes())?;
    let mut command = Command::new("npx");
    command
        .arg("--yes")
        .arg(format!("wrangler@{WRANGLER_VERSION}"))
        .args(["deploy", "--config", "wrangler.user.toml"])
        .current_dir(edge_dir)
        .env("CLOUDFLARE_API_TOKEN", token.expose()?)
        .env("CLOUDFLARE_ACCOUNT_ID", account_id)
        .env("WRANGLER_SEND_METRICS", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let command_summary = crate::runtime_meta::redact_command_summary(&format!(
        "npx --yes wrangler@{WRANGLER_VERSION} deploy --config wrangler.user.toml"
    ));
    let output = command
        .output()
        .map_err(|error| format!("cannot run {command_summary}: {error}"))?;
    if !output.status.success() {
        let stderr = sanitize_error(&String::from_utf8_lossy(&output.stderr), token);
        let stdout = sanitize_error(&String::from_utf8_lossy(&output.stdout), token);
        return Err(format!(
            "pinned Wrangler deploy failed: {} {}",
            stdout.trim(),
            stderr.trim()
        ));
    }
    Ok(())
}

fn render_wrangler_config(
    example: &str,
    worker_name: &str,
    edge_origin: &str,
    runtime_version: &str,
) -> Result<String, String> {
    if !valid_worker_name(worker_name) || !edge_origin.starts_with("https://") {
        return Err("invalid Worker identity for Wrangler config".to_owned());
    }
    let mut out = String::new();
    for line in example.lines() {
        if line.trim_start().starts_with("DEFAULT_WORKSTATION_ID =") {
            continue;
        }
        let rendered = if line.starts_with("name = ") {
            format!("name = \"{worker_name}\"")
        } else if line.starts_with("EDGE_PROJECT = ") {
            format!("EDGE_PROJECT = \"{worker_name}\"")
        } else if line.starts_with("EDGE_VERSION = ") {
            format!("EDGE_VERSION = \"{runtime_version}\"")
        } else if line.starts_with("OAUTH_ISSUER = ") {
            format!("OAUTH_ISSUER = \"{edge_origin}\"")
        } else {
            line.to_owned()
        };
        out.push_str(&rendered);
        out.push('\n');
    }
    let r2_enabled = out
        .lines()
        .any(|line| line.trim_start().starts_with("[[r2_buckets]]"));
    if !out.contains("workers_dev = true") || r2_enabled {
        return Err(
            "release Wrangler template violates the core Workers Free bootstrap contract"
                .to_owned(),
        );
    }
    Ok(out)
}

fn create_and_consume_first_pairing(
    edge_origin: &str,
    operator: &SecretBytes,
    name: Option<&str>,
) -> Result<crate::worker::EnrolledCredential, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|error| format!("cannot create first-enrollment client: {error}"))?;
    let mut body = json!({ "ttl_seconds": 600, "require_empty_fleet": true });
    if let Some(name) = name {
        body["name"] = Value::String(name.to_owned());
    }
    let response = client
        .post(format!("{edge_origin}/devices/pairings"))
        .bearer_auth(operator.expose()?)
        .json(&body)
        .send()
        .map_err(|error| {
            sanitize_error(&format!("cannot create first pairing: {error}"), operator)
        })?;
    let status = response.status();
    let pairing: Value = response
        .json()
        .map_err(|_| format!("first pairing returned non-JSON HTTP {}", status.as_u16()))?;
    if !status.is_success() || pairing.get("ok").and_then(Value::as_bool) != Some(true) {
        let code = pairing
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("pairing_create_failed");
        return Err(format!("first pairing refused: {code}"));
    }
    let pairing_id = required_string(&pairing, "pairing_id")?;
    let code = required_string(&pairing, "code")?;
    let mut consume = json!({ "pairing_id": pairing_id, "code": code });
    if let Some(name) = name {
        consume["name"] = Value::String(name.to_owned());
    }
    let response = client
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
    })
}

fn edge_devices_with_operator(
    edge_origin: &str,
    operator: &SecretBytes,
) -> Result<Vec<Value>, String> {
    let response = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?
        .get(format!("{edge_origin}/devices"))
        .bearer_auth(operator.expose()?)
        .send()
        .map_err(|error| {
            sanitize_error(
                &format!("cannot inspect first-fleet registry: {error}"),
                operator,
            )
        })?;
    let status = response.status();
    let payload: Value = response.json().map_err(|_| {
        format!(
            "device inventory returned non-JSON HTTP {}",
            status.as_u16()
        )
    })?;
    if !status.is_success() || payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("temporary operator could not read the Worker device registry".to_owned());
    }
    Ok(payload
        .get("devices")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn verify_device_fleet_admin(
    edge_origin: &str,
    enrolled: &crate::worker::EnrolledCredential,
) -> Result<(), String> {
    let response = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?
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
) -> Result<(), String> {
    let payload = crate::worker::extension_fleet_snapshot(paths)?;
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
) -> Result<(), String> {
    let response = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?
        .get(format!("{edge_origin}/health"))
        .send()
        .map_err(|error| format!("Worker health probe failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Worker health probe returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let payload: Value = response
        .json()
        .map_err(|_| "Worker health returned non-JSON".to_owned())?;
    if payload.get("ok").and_then(Value::as_bool) != Some(true)
        || payload.get("service").and_then(Value::as_str) != Some(worker_name)
        || payload.get("contractEpoch").and_then(Value::as_u64) != Some(2)
    {
        return Err("Worker health does not prove Herdr ownership/epoch-2 identity".to_owned());
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

fn verify_public_oauth(edge_origin: &str) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?;
    for path in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/oauth-protected-resource",
    ] {
        let response = client
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
    let response = client
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
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
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
            "wrangler deploy --token {access_literal}"
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
    fn wrangler_config_is_workers_free_and_has_no_fake_workstation() {
        let example = r#"name = \"herdr-edge\"
workers_dev = true
routes = []
[vars]
EDGE_PROJECT = \"herdr-edge\"
EDGE_VERSION = \"0.1.0\"
DEFAULT_WORKSTATION_ID = \"my-workstation\"
OAUTH_ISSUER = \"https://old.example\"
# [[r2_buckets]]
# binding = \"ARTIFACT_BUCKET\"
"#;
        let rendered = render_wrangler_config(
            example,
            "herdr-edge-mac",
            "https://herdr-edge-mac.example.workers.dev",
            "0.4.6",
        )
        .unwrap();
        assert!(rendered.contains("workers_dev = true"));
        assert!(!rendered.contains("DEFAULT_WORKSTATION_ID"));
        assert!(!rendered.contains("\n[[r2_buckets]]\n"));
        assert!(rendered.contains("EDGE_VERSION = \"0.4.6\""));
    }

    #[test]
    fn wrangler_is_pinned_not_latest() {
        assert_eq!(WRANGLER_VERSION, "4.129.0");
        assert_ne!(WRANGLER_VERSION, "latest");
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
