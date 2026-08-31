//! Staged runtime-generation manager for the workstation Link.
//!
//! This layer composes [`GenerationFence`] with per-generation
//! [`LocalMcpTransport`] instances. Activation switches the active loopback MCP
//! endpoint after `tools/list` contract-hash validation; in-flight requests keep
//! draining on their original owner; post-switch health observation rolls the
//! active pointer back on failure.
//!
//! Rust strengthens one Node check: `expected_runtime_version` is compared to
//! the version discovered from a live health probe, not to the configured
//! identity the transport would otherwise echo back.
//!
//! This module does not own `runtime-control.json` polling, CLI/daemon startup,
//! launchd/service mutation, or production Link cutover.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue, USER_AGENT};
use serde_json::{Number, Value, json};
use tokio::sync::Mutex as AsyncMutex;
use url::Url;

use super::generation_fence::{FenceError, GenerationFence, GenerationPhase};
use super::local_mcp::{
    LOCAL_MCP_CONTRACT_EPOCH, LinkRuntimeTransport, LocalMcpConfig, LocalMcpConfigError,
    LocalMcpTransport, ParsedBody, RuntimeHealth, RuntimeToolResult, is_loopback_host,
    parse_mcp_body, utf8_byte_len,
};
use super::request_core::RuntimeRequest;
use crate::relay::contract::compute_contract_hash;
use crate::relay::protocol::RuntimeContractInfo;

pub const RUNTIME_GENERATION_SCHEMA_VERSION: u64 = 1;
pub const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
const CATALOG_RPC_ID: &str = "generation-tools-list";
const CATALOG_USER_AGENT: &str = "herdr-runtime-generation-probe/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeGenerationSpec {
    pub generation: String,
    pub endpoint: String,
    pub expected_runtime_version: Option<String>,
    pub runtime_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecError {
    InvalidGenerationId,
    InvalidEndpoint,
    UnsupportedScheme,
    NonLoopbackEndpoint,
    InvalidExpectedRuntimeVersion,
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidGenerationId => write!(f, "runtime-generation: invalid generation id"),
            Self::InvalidEndpoint => {
                write!(f, "runtime-generation: endpoint must be a valid URL")
            }
            Self::UnsupportedScheme => {
                write!(f, "runtime-generation: endpoint must use http(s)")
            }
            Self::NonLoopbackEndpoint => {
                write!(f, "runtime-generation: endpoint must be loopback-only")
            }
            Self::InvalidExpectedRuntimeVersion => write!(
                f,
                "runtime-generation: expected_runtime_version must be null or a non-empty string",
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerConfigError {
    Spec(SpecError),
    MissingBearerToken,
    MissingContractHash,
    UnsupportedContractEpoch,
    Transport(LocalMcpConfigError),
    ClientBuildFailed,
}

impl From<SpecError> for ManagerConfigError {
    fn from(value: SpecError) -> Self {
        Self::Spec(value)
    }
}

impl From<LocalMcpConfigError> for ManagerConfigError {
    fn from(value: LocalMcpConfigError) -> Self {
        Self::Transport(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeGenerationValidation {
    pub ok: bool,
    pub code: String,
    pub runtime_version: Option<String>,
    pub contract_hash: Option<String>,
    pub tool_count: Option<usize>,
    pub checked_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeGenerationStatus {
    pub generation: String,
    pub endpoint: String,
    pub phase: GenerationPhase,
    pub in_flight: usize,
    pub validation: Option<RuntimeGenerationValidation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastTransition {
    pub from: String,
    pub to: String,
    pub outcome: &'static str,
    pub reason: Option<String>,
    pub at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeManagerStatus {
    pub active_generation: String,
    pub previous_generation: Option<String>,
    pub last_good_generation: String,
    pub transition_seq: u64,
    pub last_transition: Option<LastTransition>,
    pub generations: Vec<RuntimeGenerationStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationOutcome {
    pub ok: bool,
    pub code: String,
    pub active_generation: String,
    pub previous_generation: Option<String>,
    pub rolled_back: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveOutcome {
    pub ok: bool,
    pub code: String,
}

#[derive(Clone)]
struct GenerationRecord {
    spec: RuntimeGenerationSpec,
    transport: Arc<LocalMcpTransport>,
    validation: Option<RuntimeGenerationValidation>,
}

struct Inner {
    fence: GenerationFence,
    records: BTreeMap<String, GenerationRecord>,
    last_good_generation: String,
    transition_seq: u64,
    last_transition: Option<LastTransition>,
}

pub struct RuntimeGenerationManagerOptions {
    pub base: RuntimeGenerationSpec,
    pub bearer_token: String,
    pub contract_hash: String,
    pub contract_epoch: u64,
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
    pub observation_checks: u64,
    pub observation_interval_ms: u64,
    pub now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl RuntimeGenerationManagerOptions {
    pub fn new(
        base: RuntimeGenerationSpec,
        bearer_token: impl Into<String>,
        contract_hash: impl Into<String>,
    ) -> Self {
        Self {
            base,
            bearer_token: bearer_token.into(),
            contract_hash: contract_hash.into(),
            contract_epoch: LOCAL_MCP_CONTRACT_EPOCH,
            default_timeout_ms: 30_000,
            max_timeout_ms: 60_000,
            observation_checks: 3,
            observation_interval_ms: 500,
            now_ms: Arc::new(system_now_ms),
        }
    }
}

pub struct RuntimeGenerationManager {
    bearer_token: String,
    contract_hash: String,
    contract_epoch: u64,
    default_timeout_ms: u64,
    max_timeout_ms: u64,
    observation_checks: u64,
    observation_interval_ms: u64,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    catalog_client: reqwest::Client,
    mutation: AsyncMutex<()>,
    inner: Mutex<Inner>,
}

impl RuntimeGenerationManager {
    pub fn new(options: RuntimeGenerationManagerOptions) -> Result<Self, ManagerConfigError> {
        validate_runtime_generation_spec(&options.base)?;
        if options.bearer_token.is_empty() {
            return Err(ManagerConfigError::MissingBearerToken);
        }
        if options.contract_hash.is_empty() {
            return Err(ManagerConfigError::MissingContractHash);
        }
        if options.contract_epoch != LOCAL_MCP_CONTRACT_EPOCH {
            return Err(ManagerConfigError::UnsupportedContractEpoch);
        }
        let default_timeout_ms = bounded_positive(
            Some(options.default_timeout_ms as f64),
            30_000.0,
            1_000.0,
            60_000.0,
        );
        let max_timeout_ms = bounded_positive(
            Some(options.max_timeout_ms as f64),
            60_000.0,
            default_timeout_ms as f64,
            120_000.0,
        );
        let observation_checks =
            bounded_positive(Some(options.observation_checks as f64), 3.0, 1.0, 20.0);
        let observation_interval_ms = bounded_positive(
            Some(options.observation_interval_ms as f64),
            500.0,
            0.0,
            10_000.0,
        );
        let catalog_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ManagerConfigError::ClientBuildFailed)?;
        let transport = Arc::new(make_transport(
            &options.base,
            &options.bearer_token,
            &options.contract_hash,
            options.contract_epoch,
            default_timeout_ms,
            max_timeout_ms,
        )?);
        let mut records = BTreeMap::new();
        records.insert(
            options.base.generation.clone(),
            GenerationRecord {
                spec: options.base.clone(),
                transport,
                validation: None,
            },
        );
        Ok(Self {
            bearer_token: options.bearer_token,
            contract_hash: options.contract_hash,
            contract_epoch: options.contract_epoch,
            default_timeout_ms,
            max_timeout_ms,
            observation_checks,
            observation_interval_ms,
            now_ms: options.now_ms,
            catalog_client,
            mutation: AsyncMutex::new(()),
            inner: Mutex::new(Inner {
                fence: GenerationFence::new(options.base.generation.clone()),
                records,
                last_good_generation: options.base.generation,
                transition_seq: 0,
                last_transition: None,
            }),
        })
    }

    pub fn active_generation_id(&self) -> String {
        self.lock_inner().fence.active_generation().to_owned()
    }

    pub fn get_generation_spec(&self, generation: &str) -> Option<RuntimeGenerationSpec> {
        self.lock_inner()
            .records
            .get(generation)
            .map(|record| record.spec.clone())
    }

    pub fn get_status(&self) -> RuntimeManagerStatus {
        let inner = self.lock_inner();
        let generations = inner
            .fence
            .status()
            .into_iter()
            .map(|entry| {
                let record = inner.records.get(&entry.generation);
                RuntimeGenerationStatus {
                    generation: entry.generation.clone(),
                    endpoint: record
                        .map(|record| record.spec.endpoint.clone())
                        .unwrap_or_default(),
                    phase: entry.phase,
                    in_flight: entry.in_flight,
                    validation: record.and_then(|record| record.validation.clone()),
                }
            })
            .collect();
        RuntimeManagerStatus {
            active_generation: inner.fence.active_generation().to_owned(),
            previous_generation: inner.fence.previous_generation().map(str::to_owned),
            last_good_generation: inner.last_good_generation.clone(),
            transition_seq: inner.transition_seq,
            last_transition: inner.last_transition.clone(),
            generations,
        }
    }

    pub async fn register_generation(
        &self,
        spec: RuntimeGenerationSpec,
    ) -> Result<RuntimeGenerationValidation, SpecError> {
        validate_runtime_generation_spec(&spec)?;
        let _guard = self.mutation.lock().await;
        {
            let inner = self.lock_inner();
            if let Some(existing) = inner.records.get(&spec.generation) {
                let phase = inner.fence.phase_of(&spec.generation);
                if phase == Some(GenerationPhase::Active) && existing.spec.endpoint != spec.endpoint
                {
                    return Ok(self.validation_failure("active_generation_endpoint_immutable"));
                }
                if inner.fence.in_flight_count(&spec.generation) > 0
                    && existing.spec.endpoint != spec.endpoint
                {
                    return Ok(self.validation_failure("generation_has_in_flight_requests"));
                }
            }
        }

        let (transport, _reuse_existing) = {
            let inner = self.lock_inner();
            match inner.records.get(&spec.generation) {
                Some(existing) if existing.spec.endpoint == spec.endpoint => {
                    (Arc::clone(&existing.transport), true)
                }
                _ => (
                    Arc::new(
                        make_transport(
                            &spec,
                            &self.bearer_token,
                            &self.contract_hash,
                            self.contract_epoch,
                            self.default_timeout_ms,
                            self.max_timeout_ms,
                        )
                        .expect("spec validation already enforced loopback http(s)"),
                    ),
                    false,
                ),
            }
        };

        let validation = self.validate_record(&spec, &transport).await;
        {
            let mut inner = self.lock_inner();
            inner.fence.ensure_generation(&spec.generation);
            if !validation.ok {
                let _ = inner.fence.mark_rejected_if_idle(&spec.generation);
            } else {
                let _ = inner.fence.restore_standby_if_rejected(&spec.generation);
            }
            inner.records.insert(
                spec.generation.clone(),
                GenerationRecord {
                    spec,
                    transport,
                    validation: Some(validation.clone()),
                },
            );
        }
        Ok(validation)
    }

    pub async fn validate_generation(&self, generation: &str) -> RuntimeGenerationValidation {
        let (spec, transport) = {
            let inner = self.lock_inner();
            match inner.records.get(generation) {
                Some(record) => (record.spec.clone(), Arc::clone(&record.transport)),
                None => return self.validation_failure("generation_not_found"),
            }
        };
        let validation = self.validate_record(&spec, &transport).await;
        {
            let mut inner = self.lock_inner();
            if let Some(record) = inner.records.get_mut(generation) {
                record.validation = Some(validation.clone());
            }
            if !validation.ok {
                let _ = inner.fence.mark_rejected_if_idle(generation);
            } else {
                let _ = inner.fence.restore_standby_if_rejected(generation);
            }
        }
        validation
    }

    pub async fn activate_generation(
        &self,
        generation: &str,
        checks: Option<u64>,
        interval_ms: Option<u64>,
    ) -> ActivationOutcome {
        let _guard = self.mutation.lock().await;
        let current = self.active_generation_id();
        if generation == current {
            return ActivationOutcome {
                ok: true,
                code: "already_active".to_owned(),
                active_generation: current,
                previous_generation: self
                    .lock_inner()
                    .fence
                    .previous_generation()
                    .map(str::to_owned),
                rolled_back: false,
            };
        }
        if self.get_generation_spec(generation).is_none() {
            return ActivationOutcome {
                ok: false,
                code: "generation_not_found".to_owned(),
                active_generation: current,
                previous_generation: self
                    .lock_inner()
                    .fence
                    .previous_generation()
                    .map(str::to_owned),
                rolled_back: false,
            };
        }
        let validation = self.validate_generation(generation).await;
        if !validation.ok {
            return ActivationOutcome {
                ok: false,
                code: validation.code,
                active_generation: self.active_generation_id(),
                previous_generation: self
                    .lock_inner()
                    .fence
                    .previous_generation()
                    .map(str::to_owned),
                rolled_back: false,
            };
        }

        let previous_id = {
            let mut inner = self.lock_inner();
            match inner.fence.activate_generation(generation) {
                Ok(Some(transition)) => {
                    inner.transition_seq = inner.transition_seq.saturating_add(1);
                    transition.from
                }
                Ok(None) => {
                    return ActivationOutcome {
                        ok: true,
                        code: "already_active".to_owned(),
                        active_generation: generation.to_owned(),
                        previous_generation: inner.fence.previous_generation().map(str::to_owned),
                        rolled_back: false,
                    };
                }
                Err(FenceError::UnknownGeneration(_)) => {
                    return ActivationOutcome {
                        ok: false,
                        code: "generation_not_found".to_owned(),
                        active_generation: inner.fence.active_generation().to_owned(),
                        previous_generation: inner.fence.previous_generation().map(str::to_owned),
                        rolled_back: false,
                    };
                }
                Err(FenceError::RejectedGeneration(_)) => {
                    return ActivationOutcome {
                        ok: false,
                        code: "generation_rejected".to_owned(),
                        active_generation: inner.fence.active_generation().to_owned(),
                        previous_generation: inner.fence.previous_generation().map(str::to_owned),
                        rolled_back: false,
                    };
                }
                Err(_) => {
                    return ActivationOutcome {
                        ok: false,
                        code: "activation_refused".to_owned(),
                        active_generation: inner.fence.active_generation().to_owned(),
                        previous_generation: inner.fence.previous_generation().map(str::to_owned),
                        rolled_back: false,
                    };
                }
            }
        };

        let target = {
            let inner = self.lock_inner();
            inner
                .records
                .get(generation)
                .map(|record| Arc::clone(&record.transport))
        };
        let Some(target) = target else {
            return ActivationOutcome {
                ok: false,
                code: "generation_not_found".to_owned(),
                active_generation: self.active_generation_id(),
                previous_generation: Some(previous_id),
                rolled_back: false,
            };
        };

        let checks = bounded_positive(
            checks.map(|value| value as f64),
            self.observation_checks as f64,
            1.0,
            20.0,
        );
        let interval_ms = bounded_positive(
            interval_ms.map(|value| value as f64),
            self.observation_interval_ms as f64,
            0.0,
            10_000.0,
        );
        for index in 0..checks {
            let health = target.get_health().await;
            if !health.healthy {
                let reason = format!("health_{}", health.details.as_deref().unwrap_or("failed"));
                {
                    let mut inner = self.lock_inner();
                    let _ = inner.fence.rollback_active_generation(generation);
                    inner.last_transition = Some(LastTransition {
                        from: previous_id.clone(),
                        to: generation.to_owned(),
                        outcome: "rolled_back",
                        reason: Some(reason),
                        at_ms: self.now(),
                    });
                }
                return ActivationOutcome {
                    ok: false,
                    code: "activation_rolled_back".to_owned(),
                    active_generation: previous_id.clone(),
                    previous_generation: Some(generation.to_owned()),
                    rolled_back: true,
                };
            }
            if index + 1 < checks && interval_ms > 0 {
                tokio::time::sleep(Duration::from_millis(interval_ms)).await;
            }
        }

        {
            let mut inner = self.lock_inner();
            inner.last_good_generation = generation.to_owned();
            inner.last_transition = Some(LastTransition {
                from: previous_id.clone(),
                to: generation.to_owned(),
                outcome: "activated",
                reason: None,
                at_ms: self.now(),
            });
        }
        ActivationOutcome {
            ok: true,
            code: "activated".to_owned(),
            active_generation: generation.to_owned(),
            previous_generation: Some(previous_id),
            rolled_back: false,
        }
    }

    pub fn remove_generation(&self, generation: &str) -> RemoveOutcome {
        let mut inner = self.lock_inner();
        match inner.fence.remove_generation(generation) {
            Ok(false) => RemoveOutcome {
                ok: true,
                code: "already_absent".to_owned(),
            },
            Ok(true) => {
                inner.records.remove(generation);
                RemoveOutcome {
                    ok: true,
                    code: "removed".to_owned(),
                }
            }
            Err(FenceError::CannotRemoveActiveGeneration(_)) => RemoveOutcome {
                ok: false,
                code: "cannot_remove_active_generation".to_owned(),
            },
            Err(FenceError::GenerationHasInFlightRequests(_)) => RemoveOutcome {
                ok: false,
                code: "generation_draining".to_owned(),
            },
            Err(_) => RemoveOutcome {
                ok: false,
                code: "remove_refused".to_owned(),
            },
        }
    }

    fn now(&self) -> i64 {
        (self.now_ms)()
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn validation_failure(&self, code: &str) -> RuntimeGenerationValidation {
        RuntimeGenerationValidation {
            ok: false,
            code: code.to_owned(),
            runtime_version: None,
            contract_hash: None,
            tool_count: None,
            checked_at_ms: self.now(),
        }
    }

    async fn validate_record(
        &self,
        spec: &RuntimeGenerationSpec,
        transport: &LocalMcpTransport,
    ) -> RuntimeGenerationValidation {
        let health = transport.get_health().await;
        if !health.healthy {
            return self.validation_failure(&format!(
                "health_{}",
                health.details.as_deref().unwrap_or("failed")
            ));
        }
        let runtime = transport.runtime_info();
        if let Some(expected) = spec.expected_runtime_version.as_deref() {
            let observed = transport
                .observed_runtime_version()
                .unwrap_or_else(|| runtime.runtime_version.clone());
            if observed != expected {
                return RuntimeGenerationValidation {
                    ok: false,
                    code: "runtime_version_mismatch".to_owned(),
                    runtime_version: Some(observed),
                    contract_hash: None,
                    tool_count: None,
                    checked_at_ms: self.now(),
                };
            }
        }
        match self.fetch_catalog(&spec.endpoint).await {
            Err(code) => self.validation_failure(&code),
            Ok(tools) => match compute_contract_hash(&tools) {
                Err(_) => self.validation_failure("catalog_contract_invalid"),
                Ok(hash) if hash != self.contract_hash => RuntimeGenerationValidation {
                    ok: false,
                    code: "contract_mismatch".to_owned(),
                    runtime_version: Some(runtime.runtime_version),
                    contract_hash: Some(hash),
                    tool_count: Some(tools.len()),
                    checked_at_ms: self.now(),
                },
                Ok(hash) => RuntimeGenerationValidation {
                    ok: true,
                    code: "validated".to_owned(),
                    runtime_version: Some(runtime.runtime_version),
                    contract_hash: Some(hash),
                    tool_count: Some(tools.len()),
                    checked_at_ms: self.now(),
                },
            },
        }
    }

    async fn fetch_catalog(&self, endpoint: &str) -> Result<Vec<Value>, String> {
        let url = Url::parse(endpoint).map_err(|_| "catalog_unreachable".to_owned())?;
        let auth = HeaderValue::from_str(&format!("Bearer {}", self.bearer_token))
            .map_err(|_| "catalog_unreachable".to_owned())?;
        let body = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": CATALOG_RPC_ID,
            "method": "tools/list",
            "params": {},
        }))
        .map_err(|_| "catalog_malformed".to_owned())?;
        let request = self
            .catalog_client
            .post(url)
            .header(AUTHORIZATION, auth)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .header(USER_AGENT, CATALOG_USER_AGENT)
            .body(body);
        let outcome = tokio::time::timeout(
            Duration::from_millis(self.default_timeout_ms),
            request.send(),
        )
        .await;
        let response = match outcome {
            Err(_) => return Err("catalog_timeout".to_owned()),
            Ok(Err(_)) => return Err("catalog_unreachable".to_owned()),
            Ok(Ok(response)) => response,
        };
        let status = response.status().as_u16();
        if let Some(length) = response.content_length()
            && length as usize > MAX_CATALOG_BYTES
        {
            return Err("catalog_too_large".to_owned());
        }
        if !(200..300).contains(&status) {
            return Err(format!("catalog_http_{status}"));
        }
        let mut bytes = Vec::new();
        let mut response = response;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "catalog_malformed".to_owned())?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_CATALOG_BYTES {
                return Err("catalog_too_large".to_owned());
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if utf8_byte_len(&text) > MAX_CATALOG_BYTES {
            return Err("catalog_too_large".to_owned());
        }
        match parse_mcp_body(&text, &Value::String(CATALOG_RPC_ID.to_owned())) {
            ParsedBody::Result {
                result: Some(result),
            } => {
                let tools = result.get("tools").and_then(Value::as_array).cloned();
                tools.ok_or_else(|| "catalog_missing_tools".to_owned())
            }
            ParsedBody::Result { result: None } => Err("catalog_missing_tools".to_owned()),
            ParsedBody::Malformed | ParsedBody::IdMismatch { .. } | ParsedBody::RpcError { .. } => {
                Err("catalog_malformed".to_owned())
            }
        }
    }
}

impl LinkRuntimeTransport for RuntimeGenerationManager {
    fn name(&self) -> &str {
        "runtime-generation-manager"
    }

    fn runtime_info(&self) -> RuntimeContractInfo {
        let inner = self.lock_inner();
        let active = inner.fence.active_generation().to_owned();
        let mut info = inner
            .records
            .get(&active)
            .map(|record| record.transport.runtime_info())
            .unwrap_or(RuntimeContractInfo {
                runtime_version: "unknown".to_owned(),
                runtime_commit: None,
                runtime_generation: Some(active.clone()),
                contract_epoch: Number::from(LOCAL_MCP_CONTRACT_EPOCH),
                contract_hash: Some(self.contract_hash.clone()),
                herdr_version: None,
                herdr_protocol: None,
            });
        info.runtime_generation = Some(active);
        info
    }

    async fn dispatch_request(&self, request: RuntimeRequest) -> RuntimeToolResult {
        let request_id = request.request_id.clone();
        let (generation, transport) = {
            let mut inner = self.lock_inner();
            match inner.fence.begin_request(&request_id) {
                Ok(lease) => {
                    let transport = inner
                        .records
                        .get(&lease.generation)
                        .map(|record| Arc::clone(&record.transport));
                    (lease.generation, transport)
                }
                Err(FenceError::DuplicateRequest(_)) => {
                    return RuntimeToolResult::Failure {
                        code: "duplicate_request".to_owned(),
                        retryable: false,
                        message: "request_id is already owned by a generation".to_owned(),
                        details: None,
                    };
                }
                Err(_) => {
                    return RuntimeToolResult::Failure {
                        code: "runtime_generation_dispatch_rejected".to_owned(),
                        retryable: false,
                        message: "runtime generation refused dispatch".to_owned(),
                        details: None,
                    };
                }
            }
        };
        let Some(transport) = transport else {
            let mut inner = self.lock_inner();
            let _ = inner.fence.complete_request(&request_id, &generation);
            return RuntimeToolResult::Failure {
                code: "generation_not_found".to_owned(),
                retryable: false,
                message: "active runtime generation has no transport".to_owned(),
                details: None,
            };
        };
        let result = transport.dispatch_request(request).await;
        {
            let mut inner = self.lock_inner();
            let _ = inner.fence.complete_request(&request_id, &generation);
        }
        result
    }

    async fn cancel_request(&self, request_id: &str, reason: &str) {
        let transport = {
            let inner = self.lock_inner();
            let generation = inner.fence.cancel_target(request_id).to_owned();
            inner
                .records
                .get(&generation)
                .map(|record| Arc::clone(&record.transport))
        };
        if let Some(transport) = transport {
            transport.cancel_request(request_id, reason).await;
        }
    }

    async fn get_health(&self) -> RuntimeHealth {
        let transport = {
            let inner = self.lock_inner();
            let active = inner.fence.active_generation().to_owned();
            inner
                .records
                .get(&active)
                .map(|record| Arc::clone(&record.transport))
        };
        match transport {
            Some(transport) => transport.get_health().await,
            None => RuntimeHealth {
                healthy: false,
                details: Some("generation_not_found".to_owned()),
            },
        }
    }
}

pub fn validate_runtime_generation_spec(spec: &RuntimeGenerationSpec) -> Result<(), SpecError> {
    if !valid_generation_id(&spec.generation) {
        return Err(SpecError::InvalidGenerationId);
    }
    let url = Url::parse(&spec.endpoint).map_err(|_| SpecError::InvalidEndpoint)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(SpecError::UnsupportedScheme);
    }
    let host = url.host_str().ok_or(SpecError::InvalidEndpoint)?;
    if !is_loopback_host(host) {
        return Err(SpecError::NonLoopbackEndpoint);
    }
    if let Some(version) = spec.expected_runtime_version.as_deref()
        && version.is_empty()
    {
        return Err(SpecError::InvalidExpectedRuntimeVersion);
    }
    Ok(())
}

fn valid_generation_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.' || byte == b'-'
        })
}

fn bounded_positive(value: Option<f64>, fallback: f64, min: f64, max: f64) -> u64 {
    match value {
        Some(number) if number.is_finite() && number >= min && number <= max => {
            number.floor() as u64
        }
        _ => fallback.floor() as u64,
    }
}

fn system_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn make_transport(
    spec: &RuntimeGenerationSpec,
    bearer_token: &str,
    contract_hash: &str,
    contract_epoch: u64,
    default_timeout_ms: u64,
    max_timeout_ms: u64,
) -> Result<LocalMcpTransport, LocalMcpConfigError> {
    let mut config = LocalMcpConfig::new(bearer_token, contract_hash);
    config.endpoint = spec.endpoint.clone();
    config.contract_epoch = contract_epoch;
    config.runtime_version = spec.expected_runtime_version.clone();
    config.runtime_commit = spec.runtime_commit.clone();
    config.runtime_generation = Some(spec.generation.clone());
    config.default_timeout_ms = default_timeout_ms;
    config.max_timeout_ms = max_timeout_ms;
    LocalMcpTransport::new(config)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use serde_json::{Number, Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::{Mutex, oneshot};

    use super::{
        ActivationOutcome, LinkRuntimeTransport, RuntimeGenerationManager,
        RuntimeGenerationManagerOptions, RuntimeGenerationSpec, RuntimeToolResult,
        validate_runtime_generation_spec,
    };
    use crate::link::generation_fence::GenerationPhase;
    use crate::link::request_core::RuntimeRequest;
    use crate::relay::contract::compute_contract_hash;

    const TOKEN: &str = "runtime-generation-test-token";

    fn catalog() -> Vec<Value> {
        vec![json!({
            "name": "herdr_inspect",
            "description": "inspect",
            "inputSchema": { "type": "object", "properties": {} },
        })]
    }

    fn drifted_catalog() -> Vec<Value> {
        vec![json!({
            "name": "herdr_inspect",
            "description": "drift",
            "inputSchema": { "type": "object", "properties": {} },
        })]
    }

    fn request(id: &str) -> RuntimeRequest {
        RuntimeRequest {
            workstation_id: "w1".to_owned(),
            request_id: id.to_owned(),
            operation: "herdr_inspect".to_owned(),
            arguments: Some(serde_json::Map::new()),
            timeout_ms: Some(Number::from(5_000)),
            contract_epoch: None,
            contract_hash: None,
            idempotency_key: None,
            trace: None,
        }
    }

    struct MockState {
        version: String,
        port_label: String,
        catalog: Vec<Value>,
        fail_health_after: Option<u32>,
        health_calls: AtomicU32,
        defer_tools: bool,
        release: Mutex<Option<oneshot::Sender<()>>>,
    }

    async fn read_http_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream.read(&mut buffer).await.ok()?;
            if read == 0 {
                return if bytes.is_empty() { None } else { Some(bytes) };
            }
            bytes.extend_from_slice(&buffer[..read]);
            let header_end = bytes.windows(4).position(|window| window == b"\r\n\r\n")?;
            let headers = String::from_utf8_lossy(&bytes[..header_end + 4]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + content_length {
                return Some(bytes);
            }
        }
    }

    async fn write_json(stream: &mut TcpStream, status: u16, body: &str) {
        let reason = if (200..300).contains(&status) {
            "OK"
        } else {
            "ERR"
        };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    }

    async fn serve_mock(state: Arc<MockState>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let Some(bytes) = read_http_request(&mut stream).await else {
                        return;
                    };
                    let text = String::from_utf8_lossy(&bytes);
                    let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
                    let parsed: Value = serde_json::from_str(body).unwrap_or(json!({}));
                    let method = parsed.get("method").and_then(Value::as_str).unwrap_or("");
                    let id = parsed.get("id").cloned().unwrap_or(Value::Null);
                    match method {
                        "server/discover" => {
                            let n = state.health_calls.fetch_add(1, Ordering::SeqCst) + 1;
                            if state.fail_health_after.is_some_and(|limit| n > limit) {
                                write_json(&mut stream, 503, "{\"error\":\"down\"}").await;
                                return;
                            }
                            let body = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "serverInfo": {
                                        "name": "herdr",
                                        "version": state.version,
                                    }
                                }
                            });
                            write_json(&mut stream, 200, &body.to_string()).await;
                        }
                        "tools/list" => {
                            let body = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": { "tools": state.catalog },
                            });
                            write_json(&mut stream, 200, &body.to_string()).await;
                        }
                        "tools/call" => {
                            if state.defer_tools {
                                let (tx, rx) = oneshot::channel();
                                *state.release.lock().await = Some(tx);
                                let _ = rx.await;
                            }
                            let body = json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": { "port": state.port_label },
                            });
                            write_json(&mut stream, 200, &body.to_string()).await;
                        }
                        _ => {
                            write_json(&mut stream, 500, "{\"error\":\"unexpected\"}").await;
                        }
                    }
                });
            }
        });
        format!("http://{address}/mcp")
    }

    async fn manager_for(
        stable: &str,
        options: RuntimeGenerationManagerOptions,
    ) -> RuntimeGenerationManager {
        let _ = stable;
        RuntimeGenerationManager::new(options).expect("manager")
    }

    fn options(endpoint: &str, hash: &str) -> RuntimeGenerationManagerOptions {
        let mut options = RuntimeGenerationManagerOptions::new(
            RuntimeGenerationSpec {
                generation: "stable".to_owned(),
                endpoint: endpoint.to_owned(),
                expected_runtime_version: Some("0.3.23".to_owned()),
                runtime_commit: None,
            },
            TOKEN,
            hash,
        );
        options.observation_checks = 2;
        options.observation_interval_ms = 0;
        options
    }

    #[test]
    fn runtime_generation_specs_stay_loopback_only() {
        validate_runtime_generation_spec(&RuntimeGenerationSpec {
            generation: "candidate-1".to_owned(),
            endpoint: "http://127.0.0.1:8773/mcp".to_owned(),
            expected_runtime_version: None,
            runtime_commit: None,
        })
        .unwrap();
        let remote = validate_runtime_generation_spec(&RuntimeGenerationSpec {
            generation: "candidate".to_owned(),
            endpoint: "http://10.0.0.5:8773/mcp".to_owned(),
            expected_runtime_version: None,
            runtime_commit: None,
        })
        .unwrap_err();
        assert!(remote.to_string().contains("loopback-only"));
        let bad_id = validate_runtime_generation_spec(&RuntimeGenerationSpec {
            generation: "../bad".to_owned(),
            endpoint: "http://127.0.0.1:8773/mcp".to_owned(),
            expected_runtime_version: None,
            runtime_commit: None,
        })
        .unwrap_err();
        assert!(bad_id.to_string().contains("generation id"));
    }

    #[tokio::test]
    async fn candidate_registration_verifies_contract_hash_and_runtime_version() {
        let hash = compute_contract_hash(&catalog()).unwrap();
        let stable_state = Arc::new(MockState {
            version: "0.3.23".to_owned(),
            port_label: "stable".to_owned(),
            catalog: catalog(),
            fail_health_after: None,
            health_calls: AtomicU32::new(0),
            defer_tools: false,
            release: Mutex::new(None),
        });
        let candidate_state = Arc::new(MockState {
            version: "0.3.26".to_owned(),
            port_label: "candidate".to_owned(),
            catalog: catalog(),
            fail_health_after: None,
            health_calls: AtomicU32::new(0),
            defer_tools: false,
            release: Mutex::new(None),
        });
        let stable = serve_mock(Arc::clone(&stable_state)).await;
        let candidate = serve_mock(Arc::clone(&candidate_state)).await;
        let manager = manager_for(&stable, options(&stable, &hash)).await;
        let good = manager
            .register_generation(RuntimeGenerationSpec {
                generation: "candidate".to_owned(),
                endpoint: candidate,
                expected_runtime_version: Some("0.3.26".to_owned()),
                runtime_commit: None,
            })
            .await
            .unwrap();
        assert!(good.ok);
        assert_eq!(good.contract_hash.as_deref(), Some(hash.as_str()));
        assert_eq!(good.tool_count, Some(1));

        let drifted_state = Arc::new(MockState {
            version: "0.3.26".to_owned(),
            port_label: "candidate".to_owned(),
            catalog: drifted_catalog(),
            fail_health_after: None,
            health_calls: AtomicU32::new(0),
            defer_tools: false,
            release: Mutex::new(None),
        });
        let drifted_endpoint = serve_mock(drifted_state).await;
        let drifted_stable = serve_mock(Arc::clone(&stable_state)).await;
        let bad = manager_for(&drifted_stable, options(&drifted_stable, &hash)).await;
        let rejected = bad
            .register_generation(RuntimeGenerationSpec {
                generation: "candidate".to_owned(),
                endpoint: drifted_endpoint,
                expected_runtime_version: Some("0.3.26".to_owned()),
                runtime_commit: None,
            })
            .await
            .unwrap();
        assert!(!rejected.ok);
        assert_eq!(rejected.code, "contract_mismatch");
        assert_ne!(rejected.contract_hash.as_deref(), Some(hash.as_str()));
    }

    #[tokio::test]
    async fn activation_drains_old_in_flight_requests_on_the_old_generation() {
        let hash = compute_contract_hash(&catalog()).unwrap();
        let stable_state = Arc::new(MockState {
            version: "0.3.23".to_owned(),
            port_label: "8772".to_owned(),
            catalog: catalog(),
            fail_health_after: None,
            health_calls: AtomicU32::new(0),
            defer_tools: true,
            release: Mutex::new(None),
        });
        let candidate_state = Arc::new(MockState {
            version: "0.3.26".to_owned(),
            port_label: "8773".to_owned(),
            catalog: catalog(),
            fail_health_after: None,
            health_calls: AtomicU32::new(0),
            defer_tools: false,
            release: Mutex::new(None),
        });
        let stable = serve_mock(Arc::clone(&stable_state)).await;
        let candidate = serve_mock(Arc::clone(&candidate_state)).await;
        let manager = Arc::new(manager_for(&stable, options(&stable, &hash)).await);
        assert!(
            manager
                .register_generation(RuntimeGenerationSpec {
                    generation: "candidate".to_owned(),
                    endpoint: candidate,
                    expected_runtime_version: Some("0.3.26".to_owned()),
                    runtime_commit: None,
                })
                .await
                .unwrap()
                .ok
        );

        let old = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.dispatch_request(request("old-r1")).await }
        });
        wait_for_deferred(&stable_state).await;

        let activated = manager
            .activate_generation("candidate", Some(1), Some(0))
            .await;
        assert_eq!(
            activated,
            ActivationOutcome {
                ok: true,
                code: "activated".to_owned(),
                active_generation: "candidate".to_owned(),
                previous_generation: Some("stable".to_owned()),
                rolled_back: false,
            }
        );
        let status = manager.get_status();
        assert_eq!(status.active_generation, "candidate");
        let stable_status = status
            .generations
            .iter()
            .find(|entry| entry.generation == "stable")
            .expect("stable");
        assert_eq!(stable_status.phase, GenerationPhase::Draining);
        assert_eq!(stable_status.in_flight, 1);

        let fresh = manager.dispatch_request(request("new-r1")).await;
        assert_eq!(
            fresh,
            RuntimeToolResult::Success {
                result: Some(json!({ "port": "8773" })),
            }
        );
        release_deferred(&stable_state).await;
        let old = old.await.unwrap();
        assert_eq!(
            old,
            RuntimeToolResult::Success {
                result: Some(json!({ "port": "8772" })),
            }
        );
        let status = manager.get_status();
        let stable_status = status
            .generations
            .iter()
            .find(|entry| entry.generation == "stable")
            .expect("stable");
        assert_eq!(stable_status.phase, GenerationPhase::Standby);
        assert_eq!(stable_status.in_flight, 0);
    }

    #[tokio::test]
    async fn candidate_health_failure_during_observation_rolls_active_pointer_back() {
        let hash = compute_contract_hash(&catalog()).unwrap();
        let stable_state = Arc::new(MockState {
            version: "0.3.23".to_owned(),
            port_label: "8772".to_owned(),
            catalog: catalog(),
            fail_health_after: None,
            health_calls: AtomicU32::new(0),
            defer_tools: false,
            release: Mutex::new(None),
        });
        let candidate_state = Arc::new(MockState {
            version: "0.3.26".to_owned(),
            port_label: "8773".to_owned(),
            catalog: catalog(),
            fail_health_after: Some(2),
            health_calls: AtomicU32::new(0),
            defer_tools: false,
            release: Mutex::new(None),
        });
        let stable = serve_mock(Arc::clone(&stable_state)).await;
        let candidate = serve_mock(Arc::clone(&candidate_state)).await;
        let manager = manager_for(&stable, options(&stable, &hash)).await;
        assert!(
            manager
                .register_generation(RuntimeGenerationSpec {
                    generation: "candidate".to_owned(),
                    endpoint: candidate,
                    expected_runtime_version: Some("0.3.26".to_owned()),
                    runtime_commit: None,
                })
                .await
                .unwrap()
                .ok
        );
        let result = manager
            .activate_generation("candidate", Some(2), Some(0))
            .await;
        assert!(!result.ok);
        assert!(result.rolled_back);
        assert_eq!(manager.active_generation_id(), "stable");
        assert_eq!(
            manager.get_status().last_transition.unwrap().outcome,
            "rolled_back"
        );
    }

    #[tokio::test]
    async fn discovered_runtime_version_mismatch_fails_closed() {
        let hash = compute_contract_hash(&catalog()).unwrap();
        let stable_state = Arc::new(MockState {
            version: "0.3.23".to_owned(),
            port_label: "8772".to_owned(),
            catalog: catalog(),
            fail_health_after: None,
            health_calls: AtomicU32::new(0),
            defer_tools: false,
            release: Mutex::new(None),
        });
        let candidate_state = Arc::new(MockState {
            version: "0.9.99".to_owned(),
            port_label: "8773".to_owned(),
            catalog: catalog(),
            fail_health_after: None,
            health_calls: AtomicU32::new(0),
            defer_tools: false,
            release: Mutex::new(None),
        });
        let stable = serve_mock(Arc::clone(&stable_state)).await;
        let candidate = serve_mock(candidate_state).await;
        let manager = manager_for(&stable, options(&stable, &hash)).await;
        let rejected = manager
            .register_generation(RuntimeGenerationSpec {
                generation: "candidate".to_owned(),
                endpoint: candidate,
                expected_runtime_version: Some("0.3.26".to_owned()),
                runtime_commit: None,
            })
            .await
            .unwrap();
        assert!(!rejected.ok);
        assert_eq!(rejected.code, "runtime_version_mismatch");
        assert_eq!(rejected.runtime_version.as_deref(), Some("0.9.99"));
    }

    async fn wait_for_deferred(state: &MockState) {
        for _ in 0..200 {
            if state.release.lock().await.is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("deferred tools/call did not arrive");
    }

    async fn release_deferred(state: &MockState) {
        if let Some(tx) = state.release.lock().await.take() {
            let _ = tx.send(());
        }
    }
}
