use crate::cli::DevCommand;
use crate::paths::RuntimePaths;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::Read;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "macos")]
use std::thread;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

const STATE_SCHEMA_VERSION: u32 = 1;

#[cfg(target_os = "macos")]
const DEV_ACTIVATION_HEALTH_BUDGET: Duration = Duration::from_secs(10);
#[cfg(target_os = "macos")]
const DEV_ACTIVATION_HEALTH_POLL: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DevSyncPhase {
    Building,
    Activating,
    Reconciling,
    Succeeded,
    RolledBack,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DevSyncTransaction {
    transaction_id: String,
    phase: DevSyncPhase,
    target_version: String,
    source_repo: String,
    source_branch: Option<String>,
    source_commit: String,
    source_dirty: bool,
    active_generation_before: String,
    expected_generation: Option<String>,
    generation: Option<String>,
    started_at_ms: u128,
    updated_at_ms: u128,
    completed_at_ms: Option<u128>,
    error: Option<String>,
    activation_evidence: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DevRuntimeState {
    schema_version: u32,
    channel: String,
    target_version: String,
    source_repo: Option<String>,
    source_branch: Option<String>,
    source_commit: Option<String>,
    source_dirty: bool,
    dev_generation: Option<String>,
    prod_generation: String,
    prod_version: String,
    prod_snapshot_binary: String,
    prod_snapshot_sha256: String,
    updated_at_ms: u128,
    #[serde(default)]
    last_transaction: Option<DevSyncTransaction>,
}

struct DevPaths {
    state: PathBuf,
    prod_dir: PathBuf,
    prod_binary: PathBuf,
}

#[derive(Debug, Clone)]
struct SourceIdentity {
    branch: Option<String>,
    commit: String,
    dirty: bool,
}

#[derive(Clone, Copy)]
struct DevRecoveryIdentity<'a> {
    current_exe_is_active_runtime: bool,
    runtime_channel: &'a str,
    runtime_version: &'a str,
    compiled_source_commit: Option<&'a str>,
    compiled_source_dirty: bool,
    checkout_repo: &'a str,
    checkout_branch: Option<&'a str>,
    checkout_commit: &'a str,
    checkout_dirty: bool,
}

pub fn run(command: DevCommand) -> Result<ExitCode, String> {
    match command {
        DevCommand::Sync {
            dry_run,
            allow_dirty,
        } => sync(dry_run, allow_dirty),
        DevCommand::Status => status(),
        DevCommand::Rollback => rollback(),
    }
}

fn sync(dry_run: bool, allow_dirty: bool) -> Result<ExitCode, String> {
    ensure_default_instance()?;
    let cwd =
        env::current_dir().map_err(|error| format!("cannot read current directory: {error}"))?;
    let repo = find_repo_root(&cwd).ok_or_else(|| {
        "dev sync must run inside a herdr-mcp Rust source checkout containing Cargo.toml and crates/herdr-mcp/Cargo.toml"
            .to_owned()
    })?;
    let runtime = RuntimePaths::discover()?;
    let source = source_identity(&repo, &runtime)?;
    let target_version = source_dev_version(&repo, &runtime)?;
    if source.dirty && !allow_dirty {
        return Err(
            "dev sync refuses a dirty source tree by default; commit/stash the changes or rerun with --allow-dirty so the provenance is explicit"
                .to_owned(),
        );
    }

    let paths = dev_paths(&runtime);
    let active_before = current_generation(&runtime.config_dir)?.ok_or_else(|| {
        "dev sync requires an installed managed PROD runtime/current generation".to_owned()
    })?;
    let mut existing = read_state(&paths.state)?;
    if let Some(state) = existing.as_mut()
        && state.channel == "dev"
        && state.dev_generation.is_none()
        && interrupted_transaction_allows_recovery(state, &active_before)
        && interrupted_dev_state_matches_active_runtime(&runtime, state, &repo, &source)?
    {
        if !verify_snapshot(state)? {
            return Err(
                "refusing interrupted DEV sync recovery because the pinned PROD snapshot failed SHA-256 validation"
                    .to_owned(),
            );
        }
        let activation_evidence = verify_runtime_activation(&runtime, &active_before).map_err(|error| {
            format!(
                "refusing interrupted DEV sync recovery because the active runtime no longer satisfies the DEV activation gate: {error}"
            )
        })?;
        if !mark_interrupted_dev_recovery_committed(state, &active_before, now_ms(), dry_run) {
            print_json(&json!({
                "ok": true,
                "action": "dev_sync_recovery",
                "dry_run": true,
                "channel": "dev",
                "version": state.target_version,
                "source_commit": state.source_commit,
                "source_dirty": state.source_dirty,
                "generation": active_before,
                "prod_generation": state.prod_generation,
                "prod_snapshot_sha256": state.prod_snapshot_sha256,
                "activation_evidence": activation_evidence,
                "server_link_generation_reconciled": activation_evidence
                    .get("server_link_generation_reconciled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }))?;
            return Ok(ExitCode::SUCCESS);
        }
        mark_transaction_succeeded(state, &active_before, activation_evidence.clone(), now_ms());
        write_state(&paths.state, state)?;
        crate::local_agent_skill::sync_after_install_best_effort();
        print_json(&json!({
            "ok": true,
            "action": "dev_sync_recovered",
            "channel": "dev",
            "version": state.target_version,
            "source_commit": state.source_commit,
            "source_dirty": state.source_dirty,
            "generation": active_before,
            "prod_generation": state.prod_generation,
            "prod_snapshot_sha256": state.prod_snapshot_sha256,
            "activation_evidence": activation_evidence,
            "server_link_generation_reconciled": activation_evidence
                .get("server_link_generation_reconciled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            "transaction": state.last_transaction.as_ref(),
        }))?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut recovered_from_stale_dev = false;
    let mut accepted_verified_dev_drift = false;
    if let Some(state) = existing.as_mut()
        && state.channel == "dev"
        && state.dev_generation.as_deref() != Some(active_before.as_str())
    {
        let active_binary = runtime.config_dir.join("runtime/current/herdr-mcp");
        let active_version = binary_version(&active_binary)?;
        verify_runtime_activation(&runtime, &active_before).map_err(|error| {
            format!(
                "dev runtime state drift: state expects {:?} but runtime/current is {active_before}; active runtime fails verification: {error}",
                state.dev_generation
            )
        })?;

        if is_dev_runtime_version(&active_version) {
            // A verified managed DEV generation can be the immediate transactional
            // rollback point for the next DEV sync. Keep stale source provenance
            // untouched and never redefine the pinned PROD snapshot from DEV bytes.
            validate_stale_dev_resync_snapshot(state, &paths)?;
            accepted_verified_dev_drift = true;
        } else if dry_run {
            transition_state_to_prod(
                state,
                &active_before,
                &active_version,
                &paths.prod_binary,
                state.prod_snapshot_sha256.clone(),
                now_ms(),
            );
            recovered_from_stale_dev = true;
        } else {
            refuse_managed_exec_mutation("dev sync")?;
            let prod_sha = refresh_prod_snapshot(&runtime, &paths)?;
            transition_state_to_prod(
                state,
                &active_before,
                &active_version,
                &paths.prod_binary,
                prod_sha,
                now_ms(),
            );
            write_state(&paths.state, state)?;
            recovered_from_stale_dev = true;
        }
    }

    let prod_generation = match existing.as_ref() {
        Some(state) if state.channel == "dev" => state.prod_generation.clone(),
        _ => active_before.clone(),
    };
    let prod_version = match existing.as_ref() {
        Some(state) if state.channel == "dev" => state.prod_version.clone(),
        _ => binary_version(&runtime.config_dir.join("runtime/current/herdr-mcp"))?,
    };
    if is_dev_runtime_version(&prod_version) {
        return Err(format!(
            "refusing to treat active runtime as PROD source because its version '{prod_version}' is a DEV version"
        ));
    }
    let mut plan = json!({
        "ok": true,
        "action": "dev_sync",
        "channel_from": existing.as_ref().map(|state| state.channel.as_str()).unwrap_or("prod"),
        "channel_to": "dev",
        "target_version": target_version,
        "source_repo": repo,
        "source_branch": source.branch,
        "source_commit": source.commit,
        "source_dirty": source.dirty,
        "active_generation_before": active_before,
        "prod_generation": prod_generation,
        "prod_version": prod_version,
        "prod_snapshot_binary": paths.prod_binary,
        "build": "cargo build --release --locked -p herdr-mcp",
        "activation": "built-binary service install + production Link generation reconcile",
        "edge_deploy": false,
        "dns_mutation": false,
        "oauth_mutation": false,
    });
    if recovered_from_stale_dev {
        plan["recovered_from_stale_dev"] = json!(true);
    }
    if accepted_verified_dev_drift {
        plan["accepted_verified_dev_drift"] = json!(true);
    }
    if dry_run {
        print_json(&plan)?;
        return Ok(ExitCode::SUCCESS);
    }

    refuse_managed_exec_mutation("dev sync")?;
    ensure_prod_snapshot(
        &runtime.config_dir,
        &paths,
        existing.as_ref(),
        &prod_generation,
    )?;
    let prod_sha = file_sha256(&paths.prod_binary)?;
    let started_at_ms = now_ms();
    let base_state = dev_sync_base_state(
        existing.as_ref(),
        &target_version,
        &prod_generation,
        &prod_version,
        &paths.prod_binary,
        &prod_sha,
        started_at_ms,
    );
    let mut transaction = new_dev_sync_transaction(
        &repo,
        &source,
        &target_version,
        &active_before,
        started_at_ms,
    );
    write_state(
        &paths.state,
        &state_with_transaction(&base_state, &transaction),
    )?;

    let built_binary = repo
        .join("target")
        .join("release")
        .join(executable_name("herdr-mcp"));
    let expected_dev_generation = match (|| -> Result<String, String> {
        build_dev_binary(&repo, &source, &target_version)?;
        verify_dev_binary(&built_binary, &target_version)?;
        let built_sha = file_sha256(&built_binary)?;
        generation_from_sha256(&built_sha)
    })() {
        Ok(generation) => generation,
        Err(error) => {
            return Err(record_failed_dev_sync_transaction(
                &paths.state,
                &base_state,
                &mut transaction,
                error,
                now_ms(),
            ));
        }
    };
    transaction.phase = DevSyncPhase::Activating;
    transaction.expected_generation = Some(expected_dev_generation.clone());
    transaction.updated_at_ms = now_ms();

    // Persist the PROD recovery source before the service transaction. If the
    // independent terminal disappears during activation, `dev rollback` still
    // has a verified immutable PROD binary instead of relying on "previous",
    // which may already be another DEV generation after repeated syncs.
    let mut state = DevRuntimeState {
        schema_version: STATE_SCHEMA_VERSION,
        channel: "dev".to_owned(),
        target_version: target_version.clone(),
        source_repo: Some(repo.to_string_lossy().into_owned()),
        source_branch: source.branch.clone(),
        source_commit: Some(source.commit.clone()),
        source_dirty: source.dirty,
        dev_generation: None,
        prod_generation,
        prod_version,
        prod_snapshot_binary: paths.prod_binary.to_string_lossy().into_owned(),
        prod_snapshot_sha256: prod_sha,
        updated_at_ms: now_ms(),
        last_transaction: Some(transaction.clone()),
    };
    if let Err(error) = write_state(&paths.state, &state) {
        return Err(record_failed_dev_sync_transaction(
            &paths.state,
            &base_state,
            &mut transaction,
            format!("cannot persist DEV activating transaction: {error}"),
            now_ms(),
        ));
    }
    if let Err(error) = run_service_install(&built_binary) {
        return Err(record_failed_dev_sync_transaction(
            &paths.state,
            &base_state,
            &mut transaction,
            error,
            now_ms(),
        ));
    }

    let active_after = match current_generation(&runtime.config_dir) {
        Ok(Some(generation)) => generation,
        Ok(None) => {
            return Err(compensate_and_record_dev_sync_transaction(
                &runtime.config_dir,
                &paths.state,
                &base_state,
                &mut transaction,
                "DEV post-install generation read failed: dev service activation succeeded but runtime/current is missing".to_owned(),
                || run_service_rollback(&built_binary),
            ));
        }
        Err(error) => {
            return Err(compensate_and_record_dev_sync_transaction(
                &runtime.config_dir,
                &paths.state,
                &base_state,
                &mut transaction,
                format!("DEV post-install generation read failed: {error}"),
                || run_service_rollback(&built_binary),
            ));
        }
    };
    transaction.phase = DevSyncPhase::Reconciling;
    transaction.generation = Some(active_after.clone());
    transaction.updated_at_ms = now_ms();
    state.last_transaction = Some(transaction.clone());
    state.updated_at_ms = transaction.updated_at_ms;
    if let Err(error) = write_state(&paths.state, &state) {
        return Err(compensate_and_record_dev_sync_transaction(
            &runtime.config_dir,
            &paths.state,
            &base_state,
            &mut transaction,
            format!("DEV reconcile-state commit failed: {error}"),
            || run_service_rollback(&built_binary),
        ));
    }
    let activation_evidence = match verify_runtime_activation(&runtime, &active_after) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Err(compensate_and_record_dev_sync_transaction(
                &runtime.config_dir,
                &paths.state,
                &base_state,
                &mut transaction,
                format!("DEV post-activation gate failed: {error}"),
                || run_service_rollback(&built_binary),
            ));
        }
    };
    state.dev_generation = Some(active_after.clone());
    transaction.phase = DevSyncPhase::Succeeded;
    transaction.generation = Some(active_after.clone());
    transaction.activation_evidence = Some(activation_evidence.clone());
    transaction.error = None;
    transaction.completed_at_ms = Some(now_ms());
    transaction.updated_at_ms = transaction.completed_at_ms.unwrap_or_else(now_ms);
    state.last_transaction = Some(transaction.clone());
    state.updated_at_ms = transaction.updated_at_ms;
    if let Err(error) = write_state(&paths.state, &state) {
        return Err(compensate_and_record_dev_sync_transaction(
            &runtime.config_dir,
            &paths.state,
            &base_state,
            &mut transaction,
            format!("DEV channel-state commit failed after activation: {error}"),
            || run_service_rollback(&built_binary),
        ));
    }
    crate::local_agent_skill::sync_after_install_best_effort();

    print_json(&json!({
        "ok": true,
        "action": "dev_sync",
        "channel": "dev",
        "version": target_version,
        "source_commit": source.commit,
        "source_dirty": source.dirty,
        "generation": active_after,
        "prod_generation": state.prod_generation,
        "prod_snapshot_sha256": state.prod_snapshot_sha256,
        "activation_evidence": activation_evidence,
        "server_link_generation_reconciled": activation_evidence
            .get("server_link_generation_reconciled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "transaction": state.last_transaction.as_ref(),
    }))?;
    Ok(ExitCode::SUCCESS)
}

fn interrupted_dev_state_matches_active_runtime(
    runtime: &RuntimePaths,
    state: &DevRuntimeState,
    repo: &Path,
    source: &SourceIdentity,
) -> Result<bool, String> {
    let current_exe = env::current_exe()
        .map_err(|error| format!("cannot resolve current executable for DEV recovery: {error}"))?;
    let active_binary = runtime.config_dir.join("runtime/current/herdr-mcp");
    let current_exe = fs::canonicalize(&current_exe).map_err(|error| {
        format!(
            "cannot canonicalize current executable {} for DEV recovery: {error}",
            current_exe.display()
        )
    })?;
    let active_binary = fs::canonicalize(&active_binary).map_err(|error| {
        format!(
            "cannot canonicalize active runtime binary {} for DEV recovery: {error}",
            active_binary.display()
        )
    })?;
    let repo_text = repo.to_string_lossy();
    Ok(interrupted_dev_state_matches_identity(
        state,
        &DevRecoveryIdentity {
            current_exe_is_active_runtime: current_exe == active_binary,
            runtime_channel: crate::runtime_meta::runtime_channel(),
            runtime_version: crate::runtime_meta::runtime_version(),
            compiled_source_commit: crate::runtime_meta::compiled_source_commit(),
            compiled_source_dirty: crate::runtime_meta::compiled_source_dirty(),
            checkout_repo: repo_text.as_ref(),
            checkout_branch: source.branch.as_deref(),
            checkout_commit: source.commit.as_str(),
            checkout_dirty: source.dirty,
        },
    ))
}

fn new_dev_sync_transaction(
    repo: &Path,
    source: &SourceIdentity,
    target_version: &str,
    active_generation_before: &str,
    started_at_ms: u128,
) -> DevSyncTransaction {
    DevSyncTransaction {
        transaction_id: format!("dstx-{started_at_ms:032x}-{:x}", std::process::id()),
        phase: DevSyncPhase::Building,
        target_version: target_version.to_owned(),
        source_repo: repo.to_string_lossy().into_owned(),
        source_branch: source.branch.clone(),
        source_commit: source.commit.clone(),
        source_dirty: source.dirty,
        active_generation_before: active_generation_before.to_owned(),
        expected_generation: None,
        generation: None,
        started_at_ms,
        updated_at_ms: started_at_ms,
        completed_at_ms: None,
        error: None,
        activation_evidence: None,
    }
}

fn dev_sync_base_state(
    existing: Option<&DevRuntimeState>,
    target_version: &str,
    prod_generation: &str,
    prod_version: &str,
    prod_snapshot_binary: &Path,
    prod_snapshot_sha256: &str,
    updated_at_ms: u128,
) -> DevRuntimeState {
    let mut state = existing.cloned().unwrap_or_else(|| DevRuntimeState {
        schema_version: STATE_SCHEMA_VERSION,
        channel: "prod".to_owned(),
        target_version: target_version.to_owned(),
        source_repo: None,
        source_branch: None,
        source_commit: None,
        source_dirty: false,
        dev_generation: None,
        prod_generation: prod_generation.to_owned(),
        prod_version: prod_version.to_owned(),
        prod_snapshot_binary: prod_snapshot_binary.to_string_lossy().into_owned(),
        prod_snapshot_sha256: prod_snapshot_sha256.to_owned(),
        updated_at_ms,
        last_transaction: None,
    });
    state.prod_generation = prod_generation.to_owned();
    state.prod_version = prod_version.to_owned();
    state.prod_snapshot_binary = prod_snapshot_binary.to_string_lossy().into_owned();
    state.prod_snapshot_sha256 = prod_snapshot_sha256.to_owned();
    state.updated_at_ms = updated_at_ms;
    state
}

fn state_with_transaction(
    base_state: &DevRuntimeState,
    transaction: &DevSyncTransaction,
) -> DevRuntimeState {
    let mut state = base_state.clone();
    state.last_transaction = Some(transaction.clone());
    state.updated_at_ms = transaction.updated_at_ms;
    state
}

fn record_failed_dev_sync_transaction(
    state_path: &Path,
    base_state: &DevRuntimeState,
    transaction: &mut DevSyncTransaction,
    error: String,
    completed_at_ms: u128,
) -> String {
    transaction.phase = DevSyncPhase::Failed;
    transaction.error = Some(error.clone());
    transaction.completed_at_ms = Some(completed_at_ms);
    transaction.updated_at_ms = completed_at_ms;
    let terminal_state = state_with_transaction(base_state, transaction);
    match write_state(state_path, &terminal_state) {
        Ok(()) => error,
        Err(state_error) => {
            format!("{error}; cannot persist failed DEV sync transaction: {state_error}")
        }
    }
}

fn compensate_and_record_dev_sync_transaction<Rollback>(
    config_dir: &Path,
    state_path: &Path,
    base_state: &DevRuntimeState,
    transaction: &mut DevSyncTransaction,
    failure: String,
    rollback: Rollback,
) -> String
where
    Rollback: FnMut() -> Result<(), String>,
{
    compensate_and_record_dev_sync_transaction_with(
        state_path,
        base_state,
        transaction,
        failure,
        rollback,
        || current_generation(config_dir),
    )
}

fn compensate_and_record_dev_sync_transaction_with<Rollback, ReadGeneration>(
    state_path: &Path,
    base_state: &DevRuntimeState,
    transaction: &mut DevSyncTransaction,
    failure: String,
    mut rollback: Rollback,
    mut read_generation: ReadGeneration,
) -> String
where
    Rollback: FnMut() -> Result<(), String>,
    ReadGeneration: FnMut() -> Result<Option<String>, String>,
{
    let rollback_generation = transaction.active_generation_before.clone();
    let runtime_changed = transaction
        .generation
        .as_deref()
        .or(transaction.expected_generation.as_deref())
        .is_some_and(|generation| generation != rollback_generation);
    let mut rollback_error = None;
    if runtime_changed {
        if let Err(error) = rollback() {
            rollback_error = Some(format!("service rollback failed: {error}"));
        } else {
            match read_generation() {
                Ok(Some(generation)) if generation == rollback_generation => {
                    transaction.phase = DevSyncPhase::RolledBack;
                    transaction.generation = Some(generation);
                }
                Ok(Some(generation)) => {
                    rollback_error = Some(format!(
                        "service rollback returned success but runtime/current is {generation}, expected {rollback_generation}"
                    ));
                }
                Ok(None) => {
                    rollback_error = Some(
                        "service rollback returned success but runtime/current is missing"
                            .to_owned(),
                    );
                }
                Err(error) => {
                    rollback_error = Some(format!(
                        "service rollback returned success but runtime/current verification failed: {error}"
                    ));
                }
            }
        }
    }
    if !matches!(transaction.phase, DevSyncPhase::RolledBack) {
        transaction.phase = DevSyncPhase::Failed;
    }
    let combined = format!(
        "{failure}{}",
        rollback_error
            .as_ref()
            .map(|error| format!("; {error}"))
            .unwrap_or_default(),
    );
    let completed_at_ms = now_ms();
    transaction.error = Some(combined.clone());
    transaction.completed_at_ms = Some(completed_at_ms);
    transaction.updated_at_ms = completed_at_ms;
    let terminal_state = state_with_transaction(base_state, transaction);
    match write_state(state_path, &terminal_state) {
        Ok(()) => combined,
        Err(state_error) => {
            format!("{combined}; cannot persist compensated DEV sync transaction: {state_error}")
        }
    }
}

fn interrupted_transaction_allows_recovery(
    state: &DevRuntimeState,
    active_generation: &str,
) -> bool {
    let Some(transaction) = state.last_transaction.as_ref() else {
        // Schema-1 states written before durable DEV sync transactions remain
        // recoverable through the existing exact runtime/source identity gate.
        return true;
    };
    matches!(
        transaction.phase,
        DevSyncPhase::Activating | DevSyncPhase::Reconciling
    ) && transaction.expected_generation.as_deref() == Some(active_generation)
        && transaction.target_version == state.target_version
        && state.source_repo.as_deref() == Some(transaction.source_repo.as_str())
        && state.source_branch.as_deref() == transaction.source_branch.as_deref()
        && state.source_commit.as_deref() == Some(transaction.source_commit.as_str())
        && state.source_dirty == transaction.source_dirty
}

fn mark_transaction_succeeded(
    state: &mut DevRuntimeState,
    generation: &str,
    activation_evidence: Value,
    completed_at_ms: u128,
) {
    let Some(transaction) = state.last_transaction.as_mut() else {
        return;
    };
    transaction.phase = DevSyncPhase::Succeeded;
    transaction.generation = Some(generation.to_owned());
    transaction.activation_evidence = Some(activation_evidence);
    transaction.error = None;
    transaction.completed_at_ms = Some(completed_at_ms);
    transaction.updated_at_ms = completed_at_ms;
}

fn interrupted_dev_state_matches_identity(
    state: &DevRuntimeState,
    identity: &DevRecoveryIdentity<'_>,
) -> bool {
    state.channel == "dev"
        && state.dev_generation.is_none()
        && identity.current_exe_is_active_runtime
        && identity.runtime_channel == "dev"
        && identity.runtime_version == state.target_version
        && identity.compiled_source_commit == state.source_commit.as_deref()
        && identity.compiled_source_dirty == state.source_dirty
        && state.source_repo.as_deref() == Some(identity.checkout_repo)
        && state.source_branch.as_deref() == identity.checkout_branch
        && state.source_commit.as_deref() == Some(identity.checkout_commit)
        && state.source_dirty == identity.checkout_dirty
}

fn mark_interrupted_dev_recovery_committed(
    state: &mut DevRuntimeState,
    generation: &str,
    updated_at_ms: u128,
    dry_run: bool,
) -> bool {
    if dry_run {
        return false;
    }
    state.dev_generation = Some(generation.to_owned());
    state.updated_at_ms = updated_at_ms;
    true
}

fn status() -> Result<ExitCode, String> {
    let runtime = RuntimePaths::discover()?;
    let paths = dev_paths(&runtime);
    let state = read_state(&paths.state)?;
    let active = current_generation(&runtime.config_dir)?;
    let prod_snapshot_ok = state
        .as_ref()
        .map(|state| verify_snapshot(state).unwrap_or(false))
        .unwrap_or(false);
    let runtime_matches_state = match state.as_ref() {
        Some(state) if state.channel == "dev" => {
            active.as_deref() == state.dev_generation.as_deref()
        }
        Some(state) if state.channel == "prod" => {
            active.as_deref() == Some(state.prod_generation.as_str())
        }
        Some(_) => false,
        None => active.is_some(),
    };
    let current_binary = runtime.config_dir.join("runtime/current/herdr-mcp");
    let fallback_version = if current_binary.is_file() {
        binary_version(&current_binary).unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned())
    } else {
        env!("CARGO_PKG_VERSION").to_owned()
    };
    print_json(&json!({
        "ok": runtime_matches_state,
        "channel": state.as_ref().map(|state| state.channel.as_str()).unwrap_or("prod"),
        "version": state.as_ref().map(|state| {
            if state.channel == "dev" { state.target_version.as_str() } else { state.prod_version.as_str() }
        }).unwrap_or(fallback_version.as_str()),
        "active_generation": active,
        "runtime_matches_state": runtime_matches_state,
        "source_repo": state.as_ref().and_then(|state| state.source_repo.as_deref()),
        "source_branch": state.as_ref().and_then(|state| state.source_branch.as_deref()),
        "source_commit": state.as_ref().and_then(|state| state.source_commit.as_deref()),
        "source_dirty": state.as_ref().map(|state| state.source_dirty).unwrap_or(false),
        "dev_generation": state.as_ref().and_then(|state| state.dev_generation.as_deref()),
        "prod_generation": state.as_ref().map(|state| state.prod_generation.as_str()),
        "prod_snapshot_binary": state.as_ref().map(|state| state.prod_snapshot_binary.as_str()),
        "prod_snapshot_ok": prod_snapshot_ok,
        "last_transaction": state.as_ref().and_then(|state| state.last_transaction.as_ref()),
        "state_path": paths.state,
    }))?;
    Ok(if runtime_matches_state {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn rollback() -> Result<ExitCode, String> {
    ensure_default_instance()?;
    refuse_managed_exec_mutation("dev rollback")?;
    let runtime = RuntimePaths::discover()?;
    let paths = dev_paths(&runtime);
    let mut state = read_state(&paths.state)?
        .ok_or_else(|| "dev rollback has no recorded DEV/PROD channel state".to_owned())?;
    if state.channel != "dev" {
        return Err("dev rollback is only valid while the runtime channel is dev".to_owned());
    }
    if Path::new(&state.prod_snapshot_binary) != paths.prod_binary {
        return Err(
            "refusing dev rollback because PROD snapshot path is not the managed channel path"
                .to_owned(),
        );
    }
    if !verify_snapshot(&state)? {
        return Err(
            "refusing dev rollback because the pinned PROD snapshot failed SHA-256 validation"
                .to_owned(),
        );
    }

    unsafe { std::env::set_var("HERDR_MCP_GENERATION_TRIGGER", "dev_rollback") };
    let install_code = crate::service_lifecycle::run_install_from_payload(
        false,
        Path::new(&state.prod_snapshot_binary),
    )?;
    if install_code != ExitCode::SUCCESS {
        return Err(format!(
            "transactional PROD payload install returned non-success status {install_code:?}"
        ));
    }
    let active_after = current_generation(&runtime.config_dir)?
        .ok_or_else(|| "PROD rollback succeeded but runtime/current is missing".to_owned())?;
    state.channel = "prod".to_owned();
    state.prod_generation = active_after.clone();
    state.updated_at_ms = now_ms();
    write_state(&paths.state, &state)?;
    crate::local_agent_skill::sync_after_install_best_effort();
    print_json(&json!({
        "ok": true,
        "action": "dev_rollback",
        "channel": "prod",
        "generation": active_after,
        "prod_snapshot_sha256": state.prod_snapshot_sha256,
        "server_link_generation_reconciled": true,
    }))?;
    Ok(ExitCode::SUCCESS)
}

fn source_identity(repo: &Path, runtime: &RuntimePaths) -> Result<SourceIdentity, String> {
    #[cfg(not(target_os = "macos"))]
    let _ = runtime;
    #[cfg(target_os = "macos")]
    if source_identity_needs_stable_broker(repo) {
        return source_identity_via_stable_broker(repo, runtime);
    }
    let commit = git(repo, &["rev-parse", "HEAD"])?;
    let branch = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|value| value != "HEAD");
    let dirty = !git(repo, &["status", "--porcelain"])?.is_empty();
    Ok(SourceIdentity {
        branch,
        commit,
        dirty,
    })
}

/// Whether this checkout's Git metadata sits inside a macOS privacy-protected
/// folder, where reading it directly would make the rotating runtime the TCC
/// responsible client for Documents/Desktop/Downloads. A Herdr linked worktree
/// is covered through its `.git` marker, which points into the protected main
/// repository.
#[cfg(target_os = "macos")]
fn source_identity_needs_stable_broker(repo: &Path) -> bool {
    crate::macos_permissions::project_path_needs_protected_transport(repo)
}

/// Protected source identity is broker-only: the runtime never shells `git`
/// here and never names a `gitdir` path, so a linked worktree whose metadata
/// lives outside the snapshot's managed roots is resolved by Git itself at the
/// worktree root.
#[cfg(target_os = "macos")]
fn source_identity_via_stable_broker(
    repo: &Path,
    runtime: &RuntimePaths,
) -> Result<SourceIdentity, String> {
    if !crate::tcc_broker::installed_supports_git_identity(&runtime.config_dir) {
        return Err(stable_broker_upgrade_hint(&runtime.config_dir));
    }
    let snapshot = stable_broker_snapshot(runtime)?;

    let status = crate::tcc_broker::git_status_via_stable_broker(&snapshot, repo)
        .map_err(|error| format!("dev sync stable Git status failed: {error}"))?;
    if status.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(format!(
            "dev sync stable Git status failed: {}",
            broker_error(&status)
        ));
    }
    let dirty = broker_git_dirty(&status).ok_or_else(|| {
        format!(
            "dev sync stable Git status returned no file counts: {}",
            broker_error(&status)
        )
    })?;

    let identity = crate::tcc_broker::git_identity_via_stable_broker(&snapshot, repo)
        .map_err(|error| format!("dev sync stable Git identity failed: {error}"))?;
    let (branch, commit) = git_identity_from_broker(&identity)?;
    Ok(SourceIdentity {
        branch,
        commit,
        dirty,
    })
}

/// Fail closed with the established macOS broker-upgrade flow. `dev sync`
/// never installs or authorizes the broker itself.
#[cfg(target_os = "macos")]
fn stable_broker_upgrade_hint(config_dir: &Path) -> String {
    let installed = crate::tcc_broker::installed_compat_revision(config_dir)
        .map(|revision| revision.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    format!(
        "dev sync needs a stable TCC broker that implements the read-only Git identity action (broker revision {} or newer) but the installed broker revision is {installed}; run `herdr-mcp permissions setup --upgrade-broker`, grant Full Disk Access to the stable broker if macOS asks, complete `herdr-mcp permissions verify`, then re-run dev sync",
        crate::tcc_broker::GIT_IDENTITY_MIN_COMPAT_REVISION
    )
}

/// Live Herdr snapshot whose managed roots and secret-path gates the stable
/// broker re-validates for every read. An unavailable socket or snapshot fails
/// closed instead of falling back to direct `git`.
#[cfg(target_os = "macos")]
fn stable_broker_snapshot(runtime: &RuntimePaths) -> Result<Value, String> {
    let socket = runtime.herdr_socket.as_ref().ok_or_else(|| {
        "dev sync needs a live Herdr socket to read protected Git metadata through the stable TCC broker"
            .to_owned()
    })?;
    let envelope = crate::herdr::HerdrClient::new(socket)
        .call_with_timeout("session.snapshot", json!({}), Duration::from_secs(8))
        .map_err(|error| {
            format!(
                "dev sync cannot read the Herdr snapshot required for stable TCC Git access ({}): {}",
                error.code, error.message
            )
        })?;
    Ok(envelope.get("snapshot").cloned().unwrap_or(envelope))
}

/// Read one bounded repository file through the stable broker. Callers must
/// pass a live snapshot so managed-root and secret-path gates stay identical
/// to an ordinary `herdr_fs_read`.
#[cfg(target_os = "macos")]
fn broker_read_text(snapshot: &Value, path: &Path, max_bytes: usize) -> Result<String, String> {
    let value = crate::tcc_broker::fs_read_via_stable_broker(snapshot, path, max_bytes)
        .map_err(|error| format!("stable broker fs_read {} failed: {error}", path.display()))?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(format!(
            "stable broker fs_read {} failed: {}",
            path.display(),
            broker_error(&value)
        ));
    }
    if value.get("truncated").and_then(Value::as_bool) == Some(true) {
        return Err(format!(
            "stable broker metadata read was truncated for {}",
            path.display()
        ));
    }
    value
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "stable broker metadata read returned no content for {}",
                path.display()
            )
        })
}

#[cfg(target_os = "macos")]
fn broker_error(value: &Value) -> String {
    ["message", "reason", "code"]
        .into_iter()
        .find_map(|field| value.get(field).and_then(Value::as_str))
        .unwrap_or("unknown stable broker failure")
        .to_owned()
}

#[cfg(target_os = "macos")]
fn broker_git_dirty(status: &Value) -> Option<bool> {
    if let Some(files) = status
        .get("counts")
        .and_then(|counts| counts.get("files"))
        .and_then(Value::as_u64)
    {
        return Some(files > 0);
    }
    let output = status.get("output").and_then(Value::as_str)?;
    Some(
        output
            .lines()
            .any(|line| !line.trim().is_empty() && !line.starts_with("##")),
    )
}

/// Validate the stable broker's identity response: the commit must be a full
/// Git object id, and a missing branch is only accepted for a detached HEAD.
#[cfg(target_os = "macos")]
fn git_identity_from_broker(value: &Value) -> Result<(Option<String>, String), String> {
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(format!(
            "dev sync stable Git identity failed: {}",
            broker_error(value)
        ));
    }
    let commit = value
        .get("commit")
        .and_then(Value::as_str)
        .ok_or_else(|| "dev sync stable Git identity returned no commit".to_owned())?
        .to_ascii_lowercase();
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "dev sync stable Git identity returned invalid commit '{commit}'"
        ));
    }
    let branch = value
        .get("branch")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    Ok((branch, commit))
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|error| format!("failed to run git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            bounded_text(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn source_dev_version(repo: &Path, runtime: &RuntimePaths) -> Result<String, String> {
    #[cfg(not(target_os = "macos"))]
    let _ = runtime;
    #[cfg(target_os = "macos")]
    if source_identity_needs_stable_broker(repo) {
        return source_dev_version_via_stable_broker(repo, runtime);
    }
    let output = Command::new("cargo")
        .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
        .current_dir(repo)
        .output()
        .map_err(|error| format!("failed to run cargo metadata for DEV target version: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed while resolving DEV target version: {}",
            bounded_text(&output.stderr)
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cannot decode cargo metadata for DEV target version: {error}"))?;
    dev_version_from_metadata(&metadata)
}

/// Resolve the DEV target version from the checkout's own manifests through
/// the stable broker. `cargo metadata` would read privacy-protected files as
/// the rotating runtime's TCC client. The read stays bounded to the two
/// manifests that can carry the `herdr-mcp` package version, and the caller has
/// already gated this path on an identity-capable broker.
#[cfg(target_os = "macos")]
fn source_dev_version_via_stable_broker(
    repo: &Path,
    runtime: &RuntimePaths,
) -> Result<String, String> {
    let snapshot = stable_broker_snapshot(runtime)?;
    let manifest = repo.join("crates").join("herdr-mcp").join("Cargo.toml");
    let text = broker_read_text(&snapshot, &manifest, 64 * 1024)?;
    let version = match toml_section_version(&text, "[package]") {
        Some(version) => version,
        None => {
            // `version.workspace = true`: the workspace root owns the value.
            let root = repo.join("Cargo.toml");
            let text = broker_read_text(&snapshot, &root, 64 * 1024)?;
            toml_section_version(&text, "[workspace.package]").ok_or_else(|| {
                format!(
                    "cannot resolve the herdr-mcp package version through the stable broker from {} or {}",
                    manifest.display(),
                    root.display()
                )
            })?
        }
    };
    dev_version_from_str(&version)
}

/// Bounded `version = "..."` lookup inside one TOML table. A
/// `version.workspace = true` line is deliberately not a value.
#[cfg(target_os = "macos")]
fn toml_section_version(text: &str, section: &str) -> Option<String> {
    let mut active = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            active = line == section;
            continue;
        }
        if !active {
            continue;
        }
        let Some(value) = line
            .strip_prefix("version")
            .map(str::trim_start)
            .and_then(|rest| rest.strip_prefix('='))
        else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches(|character| character == '"' || character == '\'');
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

fn dev_version_from_str(version: &str) -> Result<String, String> {
    semver::Version::parse(version)
        .map_err(|error| format!("invalid herdr-mcp package version '{version}': {error}"))?;
    Ok(format!("{version}-dev"))
}

fn dev_version_from_metadata(metadata: &Value) -> Result<String, String> {
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| "cargo metadata missing packages for DEV target version".to_owned())?;
    let package = packages
        .iter()
        .find(|package| package.get("name").and_then(Value::as_str) == Some("herdr-mcp"))
        .ok_or_else(|| {
            "cargo metadata missing herdr-mcp package for DEV target version".to_owned()
        })?;
    let version = package
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| "cargo metadata missing herdr-mcp package version".to_owned())?;
    dev_version_from_str(version)
}

fn build_dev_binary(
    repo: &Path,
    source: &SourceIdentity,
    target_version: &str,
) -> Result<(), String> {
    let status = Command::new("cargo")
        .args(["build", "--release", "--locked", "-p", "herdr-mcp"])
        .current_dir(repo)
        .env("HERDR_MCP_BUILD_CHANNEL", "dev")
        .env("HERDR_MCP_BUILD_VERSION", target_version)
        .env("HERDR_MCP_BUILD_COMMIT", &source.commit)
        .env(
            "HERDR_MCP_BUILD_DIRTY",
            if source.dirty { "1" } else { "0" },
        )
        .status()
        .map_err(|error| format!("failed to start cargo build: {error}"))?;
    if !status.success() {
        return Err(format!("DEV Rust build failed with status {status}"));
    }
    Ok(())
}

fn verify_dev_binary(binary: &Path, expected_version: &str) -> Result<(), String> {
    let version = binary_version(binary)?;
    if version != expected_version {
        return Err(format!(
            "DEV binary identity mismatch: expected {expected_version}, observed {version}"
        ));
    }
    Ok(())
}

fn binary_version(binary: &Path) -> Result<String, String> {
    let output = Command::new(binary)
        .arg("version")
        .output()
        .map_err(|error| format!("cannot execute DEV binary {}: {error}", binary.display()))?;
    if !output.status.success() {
        return Err(format!(
            "DEV binary version probe failed: {}",
            bounded_text(&output.stderr)
        ));
    }
    parse_version_output(&String::from_utf8_lossy(&output.stdout))
}

fn parse_version_output(text: &str) -> Result<String, String> {
    let first = text.lines().next().unwrap_or("").trim();
    first
        .strip_prefix("herdr-mcp ")
        .map(str::to_owned)
        .ok_or_else(|| format!("unexpected runtime version output: {first}"))
}

fn run_service_install(binary: &Path) -> Result<(), String> {
    let output = Command::new(binary)
        .args(["service", "install"])
        .env_remove("HERDR_MCP_EXEC_ID")
        .env("HERDR_MCP_GENERATION_TRIGGER", "dev_sync")
        .output()
        .map_err(|error| format!("cannot execute transactional service install: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "transactional service install failed: {}{}",
            bounded_text(&output.stderr),
            bounded_text(&output.stdout)
        ));
    }
    Ok(())
}

fn run_service_rollback(binary: &Path) -> Result<(), String> {
    let output = Command::new(binary)
        .args(["service", "rollback"])
        .env_remove("HERDR_MCP_EXEC_ID")
        .env("HERDR_MCP_GENERATION_TRIGGER", "dev_sync_compensation")
        .env("HERDR_MCP_INTERNAL_GENERATION_RECOVERY", "1")
        .output()
        .map_err(|error| format!("cannot execute transactional service rollback: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "transactional service rollback failed: {}{}",
            bounded_text(&output.stderr),
            bounded_text(&output.stdout)
        ));
    }
    Ok(())
}

fn is_dev_runtime_version(version: &str) -> bool {
    version.ends_with("-dev")
}

fn transition_state_to_prod(
    state: &mut DevRuntimeState,
    active_generation: &str,
    active_version: &str,
    prod_snapshot_binary: &Path,
    prod_snapshot_sha256: String,
    now_ms: u128,
) {
    state.channel = "prod".to_owned();
    state.prod_generation = active_generation.to_owned();
    state.prod_version = active_version.to_owned();
    state.prod_snapshot_binary = prod_snapshot_binary.to_string_lossy().into_owned();
    state.prod_snapshot_sha256 = prod_snapshot_sha256;
    state.updated_at_ms = now_ms;
}

fn refresh_prod_snapshot(runtime: &RuntimePaths, paths: &DevPaths) -> Result<String, String> {
    let current_binary = runtime.config_dir.join("runtime/current/herdr-mcp");
    let metadata = fs::metadata(&current_binary)
        .map_err(|error| format!("cannot inspect current PROD binary: {error}"))?;
    if !metadata.is_file() {
        return Err("current PROD runtime binary is not a regular file".to_owned());
    }
    secure_dir(&paths.prod_dir)?;
    atomic_copy_executable(&current_binary, &paths.prod_binary)?;
    file_sha256(&paths.prod_binary)
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn reconcile_after_public_prod_install() -> Result<(), String> {
    if crate::runtime_meta::runtime_channel() != "prod" {
        return Ok(());
    }
    let runtime = RuntimePaths::discover()?;
    if runtime.instance.is_named() {
        return Ok(());
    }
    let paths = dev_paths(&runtime);
    let mut state = match read_state(&paths.state)? {
        Some(state) => state,
        None => return Ok(()),
    };
    let active_generation = current_generation(&runtime.config_dir)?.ok_or_else(|| {
        "service install succeeded but runtime/current generation is missing during channel state reconcile"
            .to_owned()
    })?;
    let active_binary = runtime.config_dir.join("runtime/current/herdr-mcp");
    let active_version = binary_version(&active_binary)?;
    if is_dev_runtime_version(&active_version) {
        return Err(format!(
            "refusing to reconcile channel state to PROD because active runtime version '{active_version}' is a DEV version"
        ));
    }
    verify_runtime_activation(&runtime, &active_generation).map_err(|error| {
        format!(
            "refusing to reconcile channel state to PROD because active runtime fails verification: {error}"
        )
    })?;
    let prod_sha = refresh_prod_snapshot(&runtime, &paths)?;
    transition_state_to_prod(
        &mut state,
        &active_generation,
        &active_version,
        &paths.prod_binary,
        prod_sha,
        now_ms(),
    );
    write_state(&paths.state, &state)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_runtime_activation(runtime: &RuntimePaths, generation: &str) -> Result<Value, String> {
    // `service install` proves health before it returns, but post-commit
    // integration work can still overlap a short launchd/watchdog convergence
    // window. Re-measure the exact expected generation for a bounded period
    // instead of turning one transient unhealthy sample into a compensating
    // rollback. Wrong/missing generation evidence remains fail-closed below.
    let deadline = Instant::now() + DEV_ACTIVATION_HEALTH_BUDGET;
    let service = wait_for_dev_service_activation_with(
        generation,
        crate::service_manager::doctor_status,
        || Instant::now() < deadline,
        || thread::sleep(DEV_ACTIVATION_HEALTH_POLL),
    )?;
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is required for runtime activation verification".to_owned())?;
    let link = crate::link::ownership::collect_status_report(&home, &runtime.config_dir);
    let native_host = crate::native_host_install::doctor_status()?;
    validate_dev_activation_evidence(generation, &service, &link, Some(&native_host))
}

#[cfg(any(target_os = "macos", test))]
fn service_activation_ready(generation: &str, service: &Value) -> Result<bool, String> {
    if service.get("implementation").and_then(Value::as_str) != Some("rust") {
        return Err("service implementation is not Rust after DEV activation".to_owned());
    }
    let observed_generation = service
        .get("generation")
        .and_then(Value::as_str)
        .ok_or_else(|| "service status is missing generation evidence".to_owned())?;
    if observed_generation != generation {
        return Err(format!(
            "service generation mismatch after DEV activation: expected {generation}, observed {observed_generation}"
        ));
    }
    Ok(service.get("ok").and_then(Value::as_bool) == Some(true)
        && service.get("healthy").and_then(Value::as_bool) == Some(true))
}

#[cfg(any(target_os = "macos", test))]
fn wait_for_dev_service_activation_with<Probe, CanRetry, Pause>(
    generation: &str,
    mut probe: Probe,
    mut can_retry: CanRetry,
    mut pause: Pause,
) -> Result<Value, String>
where
    Probe: FnMut() -> Result<Value, String>,
    CanRetry: FnMut() -> bool,
    Pause: FnMut(),
{
    loop {
        let service = probe()?;
        if service_activation_ready(generation, &service)? {
            return Ok(service);
        }
        if !can_retry() {
            return Err(format!(
                "service status did not become healthy for DEV generation {generation} within the post-activation convergence budget"
            ));
        }
        pause();
    }
}

#[cfg(not(target_os = "macos"))]
fn verify_runtime_activation(_runtime: &RuntimePaths, _generation: &str) -> Result<Value, String> {
    Err(
        "DEV sync activation verification currently depends on macOS launchd ownership evidence; production Linux install/update uses the Linux service/Link health path"
            .to_owned(),
    )
}

#[cfg(any(target_os = "macos", test))]
fn validate_dev_activation_evidence(
    generation: &str,
    service: &Value,
    link: &Value,
    native_host: Option<&Value>,
) -> Result<Value, String> {
    if service.get("ok").and_then(Value::as_bool) != Some(true)
        || service.get("healthy").and_then(Value::as_bool) != Some(true)
    {
        return Err("service status is not healthy after DEV activation".to_owned());
    }
    let service_generation = service
        .get("generation")
        .and_then(Value::as_str)
        .ok_or_else(|| "service status is missing generation evidence".to_owned())?;
    if service_generation != generation {
        return Err(format!(
            "service generation mismatch after DEV activation: expected {generation}, observed {service_generation}"
        ));
    }

    if link.get("ok").and_then(Value::as_bool) != Some(true)
        || link.get("production_owner").and_then(Value::as_str) != Some("rust")
    {
        return Err(
            "production Link ownership/status is not healthy after DEV activation".to_owned(),
        );
    }
    let prod = link
        .get("agents")
        .and_then(Value::as_array)
        .and_then(|agents| {
            agents.iter().find(|agent| {
                agent.get("label").and_then(Value::as_str) == Some("dev.herdr-mcp.link-prod")
            })
        })
        .ok_or_else(|| "production Link status is missing link-prod evidence".to_owned())?;
    if prod.get("loaded").and_then(Value::as_bool) != Some(true)
        || prod.get("implementation").and_then(Value::as_str) != Some("rust")
        || prod
            .get("points_at_managed_runtime")
            .and_then(Value::as_bool)
            != Some(true)
        || prod.get("points_at_repo_checkout").and_then(Value::as_bool) == Some(true)
    {
        return Err(
            "production Link is not a loaded managed Rust Link after DEV activation".to_owned(),
        );
    }
    let alignment = link
        .get("production_runtime_alignment")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            "production Link status is missing generation alignment evidence".to_owned()
        })?;
    let current = alignment
        .get("current_generation")
        .and_then(Value::as_str)
        .unwrap_or("");
    let active = alignment
        .get("active_generation")
        .and_then(Value::as_str)
        .unwrap_or("");
    let control_matches = alignment
        .get("runtime_control_active_matches_current")
        .and_then(Value::as_bool)
        == Some(true);
    let loaded_matches = alignment
        .get("loaded_matches_current")
        .and_then(Value::as_bool)
        == Some(true);
    if current != generation || active != generation || !control_matches || !loaded_matches {
        return Err(format!(
            "production Link generation mismatch after DEV activation: expected={generation} current={current} active={active} control_matches={control_matches} loaded_matches={loaded_matches}"
        ));
    }

    let native_host_state = match native_host {
        Some(view)
            if view.get("runtime_matches_current").and_then(Value::as_bool) == Some(true)
                && view.get("ok").and_then(Value::as_bool) == Some(true) =>
        {
            "current"
        }
        Some(view)
            if view
                .get("owned_manifest_count")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                == 0
                && view.get("wrapper_ok").and_then(Value::as_bool) == Some(false)
                && view.get("runtime_binary_ok").and_then(Value::as_bool) == Some(false) =>
        {
            "absent"
        }
        Some(view)
            if view.get("reason").and_then(Value::as_str) == Some("native_host_not_owned") =>
        {
            // Retain compatibility with the mutation helper's explicit skip shape
            // for pure tests/older callers, while live DEV verification uses the
            // read-only doctor status above.
            "not_owned"
        }
        Some(_) => {
            return Err(
                "Native Messaging state is partial/foreign/stale after DEV activation".to_owned(),
            );
        }
        None => "not_applicable",
    };

    Ok(json!({
        "server_link_generation_reconciled": true,
        "service_generation": service_generation,
        "link_current_generation": current,
        "link_active_generation": active,
        "link_loaded_environment_stale": alignment
            .get("loaded_environment_stale")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "native_host": native_host_state,
    }))
}

fn ensure_prod_snapshot(
    config_dir: &Path,
    paths: &DevPaths,
    existing: Option<&DevRuntimeState>,
    prod_generation: &str,
) -> Result<(), String> {
    if let Some(state) = existing
        && state.channel == "dev"
    {
        if Path::new(&state.prod_snapshot_binary) != paths.prod_binary {
            return Err("existing DEV state points at a non-managed PROD snapshot path".to_owned());
        }
        if !verify_snapshot(state)? {
            return Err("existing PROD snapshot is missing or fails SHA-256 validation".to_owned());
        }
        return Ok(());
    }
    let active = current_generation(config_dir)?
        .ok_or_else(|| "cannot snapshot PROD because runtime/current is missing".to_owned())?;
    if active != prod_generation {
        return Err(format!(
            "refusing PROD snapshot: expected active {prod_generation}, observed {active}"
        ));
    }
    let current_binary = config_dir.join("runtime/current/herdr-mcp");
    let version = binary_version(&current_binary)?;
    if is_dev_runtime_version(&version) {
        return Err(format!(
            "refusing PROD snapshot: current runtime version '{version}' is a DEV version and cannot be snapshotted as PROD"
        ));
    }
    let metadata = fs::metadata(&current_binary)
        .map_err(|error| format!("cannot inspect current PROD binary: {error}"))?;
    if !metadata.is_file() {
        return Err("current PROD runtime binary is not a regular file".to_owned());
    }
    secure_dir(&paths.prod_dir)?;
    atomic_copy_executable(&current_binary, &paths.prod_binary)
}

fn verify_snapshot(state: &DevRuntimeState) -> Result<bool, String> {
    let path = Path::new(&state.prod_snapshot_binary);
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => return Ok(false),
        Ok(metadata) if !metadata.is_file() => return Ok(false),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("cannot inspect PROD snapshot: {error}")),
    }
    Ok(file_sha256(path)? == state.prod_snapshot_sha256)
}

fn validate_stale_dev_resync_snapshot(
    state: &DevRuntimeState,
    paths: &DevPaths,
) -> Result<(), String> {
    if Path::new(&state.prod_snapshot_binary) != paths.prod_binary {
        return Err(
            "dev runtime state drift: existing DEV state points at a non-managed PROD snapshot path"
                .to_owned(),
        );
    }
    if !verify_snapshot(state)? {
        return Err(
            "dev runtime state drift: existing PROD snapshot is missing or fails SHA-256 validation"
                .to_owned(),
        );
    }
    Ok(())
}

fn dev_paths(runtime: &RuntimePaths) -> DevPaths {
    let runtime_root = runtime.config_dir.join("runtime");
    let prod_dir = runtime_root.join("channels").join("prod");
    DevPaths {
        state: runtime_root.join("channel.json"),
        prod_binary: prod_dir.join(executable_name("herdr-mcp")),
        prod_dir,
    }
}

/// Return the repository recorded as the source of the currently active clean
/// DEV runtime when it matches the exact source commit requested by the local
/// Agent Skill installer. The caller must still read content from that commit's
/// Git objects rather than from the mutable working tree.
pub(crate) fn local_agent_skill_source_repo(
    source_commit: &str,
) -> Result<Option<PathBuf>, String> {
    let runtime = RuntimePaths::discover()?;
    let paths = dev_paths(&runtime);
    let state = read_state(&paths.state)?;
    let Some(repo_text) = state
        .as_ref()
        .and_then(|state| matching_clean_dev_source_repo(state, source_commit))
    else {
        return Ok(None);
    };
    let repo = PathBuf::from(repo_text);
    match fs::canonicalize(&repo) {
        Ok(repo) if repo.is_dir() => Ok(Some(repo)),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot resolve recorded DEV source repository {}: {error}",
            repo.display()
        )),
    }
}

fn matching_clean_dev_source_repo<'a>(
    state: &'a DevRuntimeState,
    source_commit: &str,
) -> Option<&'a str> {
    (state.channel == "dev"
        && !state.source_dirty
        && state.source_commit.as_deref() == Some(source_commit))
    .then_some(state.source_repo.as_deref())
    .flatten()
}

fn current_generation(config_dir: &Path) -> Result<Option<String>, String> {
    let current = config_dir.join("runtime/current");
    match fs::symlink_metadata(&current) {
        Ok(metadata) if metadata.file_type().is_symlink() => {}
        Ok(_) => return Err("runtime/current exists but is not a managed symlink".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect runtime/current: {error}")),
    }
    let target = fs::read_link(&current)
        .map_err(|error| format!("cannot read runtime/current symlink: {error}"))?;
    generation_from_target(&target).map(Some)
}

fn generation_from_target(target: &Path) -> Result<String, String> {
    if target.is_absolute() {
        return Err(
            "runtime/current target must be relative to the managed runtime root".to_owned(),
        );
    }
    let mut parts = target.components();
    if !matches!(parts.next(), Some(Component::Normal(value)) if value == "generations") {
        return Err(format!(
            "runtime/current points outside managed generations: {}",
            target.display()
        ));
    }
    let Some(Component::Normal(id)) = parts.next() else {
        return Err("runtime/current target has no generation id".to_owned());
    };
    let id = id.to_string_lossy().into_owned();
    if !id.starts_with("rust-") || parts.next().is_some() {
        return Err(format!(
            "runtime/current target is not a managed rust generation: {id}"
        ));
    }
    Ok(id)
}

fn read_state(path: &Path) -> Result<Option<DevRuntimeState>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("DEV runtime state must not be a symlink".to_owned());
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err("DEV runtime state must be a regular file".to_owned());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect DEV runtime state: {error}")),
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(format!(
                "cannot read DEV runtime state {}: {error}",
                path.display()
            ));
        }
    };
    if bytes.len() > 64 * 1024 {
        return Err("DEV runtime state exceeds 64 KiB".to_owned());
    }
    let state: DevRuntimeState = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid DEV runtime state: {error}"))?;
    if state.schema_version != STATE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported DEV runtime state schema {}",
            state.schema_version
        ));
    }
    if !matches!(state.channel.as_str(), "dev" | "prod") {
        return Err("DEV runtime state has an invalid channel".to_owned());
    }
    Ok(Some(state))
}

fn write_state(path: &Path, state: &DevRuntimeState) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "DEV runtime state path has no parent".to_owned())?;
    secure_dir(parent)?;
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("cannot encode DEV runtime state: {error}"))?;
    let temp = parent.join(format!(".channel.{}.tmp", std::process::id()));
    fs::write(&temp, &bytes).map_err(|error| format!("cannot stage DEV runtime state: {error}"))?;
    set_mode(&temp, 0o600)?;
    fs::rename(&temp, path).map_err(|error| format!("cannot commit DEV runtime state: {error}"))
}

fn atomic_copy_executable(source: &Path, destination: &Path) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "PROD snapshot destination has no parent".to_owned())?;
    secure_dir(parent)?;
    let temp = parent.join(format!(".herdr-mcp.{}.tmp", std::process::id()));
    fs::copy(source, &temp).map_err(|error| format!("cannot stage PROD snapshot: {error}"))?;
    set_mode(&temp, 0o755)?;
    fs::rename(&temp, destination).map_err(|error| format!("cannot commit PROD snapshot: {error}"))
}

fn secure_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    set_mode(path, 0o700)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot chmod {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("cannot open {} for SHA-256: {error}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn generation_from_sha256(sha256: &str) -> Result<String, String> {
    if sha256.len() < 16 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("DEV runtime SHA-256 is malformed".to_owned());
    }
    Ok(format!("rust-{}", &sha256[..16]))
}

fn refuse_managed_exec_mutation(action: &str) -> Result<(), String> {
    if env::var_os("HERDR_MCP_EXEC_ID").is_some() {
        return Err(format!(
            "{action} must run from an independent terminal, not a managed herdr_exec session, because activating runtime/current restarts dev.herdr-mcp.server"
        ));
    }
    Ok(())
}

fn ensure_default_instance() -> Result<(), String> {
    let paths = RuntimePaths::discover()?;
    if paths.instance.is_named() {
        return Err(
            "DEV/PROD runtime switching is only supported for the default workstation instance"
                .to_owned(),
        );
    }
    Ok(())
}

fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(path) = current {
        if path.join("Cargo.toml").is_file()
            && path
                .join("crates")
                .join("herdr-mcp")
                .join("Cargo.toml")
                .is_file()
        {
            return Some(path.to_path_buf());
        }
        current = path.parent();
    }
    None
}

fn executable_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_owned()
    }
}

fn bounded_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(2048)])
        .trim()
        .to_owned()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn print_json(value: &Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|error| format!("cannot encode DEV runtime result: {error}"))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn stable_broker_identity_and_manifest_version_are_validated() {
        let oid = "d2388010d253e34622cc727c88e7f23aba25758c";
        let (branch, commit) = git_identity_from_broker(&json!({
            "ok": true,
            "commit": oid,
            "branch": "fix/v1-dev-source-identity-broker-20260914",
            "detached": false,
        }))
        .unwrap();
        assert_eq!(
            branch.as_deref(),
            Some("fix/v1-dev-source-identity-broker-20260914")
        );
        assert_eq!(commit, oid);

        let (branch, commit) =
            git_identity_from_broker(&json!({"ok": true, "commit": oid, "branch": null})).unwrap();
        assert_eq!(branch, None);
        assert_eq!(commit, oid);

        // A short object id is exactly what the existing broker `log` action
        // returns; it must never be accepted as exact provenance.
        assert!(git_identity_from_broker(&json!({"ok": true, "commit": "d2388010"})).is_err());
        assert!(
            git_identity_from_broker(&json!({"ok": false, "reason": "not_a_git_repo"})).is_err()
        );

        assert_eq!(
            broker_git_dirty(&json!({"counts": {"files": 0}})),
            Some(false)
        );
        assert_eq!(
            broker_git_dirty(&json!({"counts": {"files": 3}})),
            Some(true)
        );

        assert_eq!(
            toml_section_version(
                "[package]\nname = \"herdr-mcp\"\nversion = \"1.0.0-alpha.8\"\n",
                "[package]"
            )
            .as_deref(),
            Some("1.0.0-alpha.8")
        );
        assert_eq!(
            toml_section_version(
                "[workspace.package]\nversion = \"9.9.9\"\n",
                "[workspace.package]"
            )
            .as_deref(),
            Some("9.9.9")
        );
        assert_eq!(
            toml_section_version("[package]\nversion.workspace = true\n", "[package]"),
            None
        );
        assert_eq!(
            dev_version_from_str("1.0.0-alpha.8").unwrap(),
            "1.0.0-alpha.8-dev"
        );
        assert!(dev_version_from_str("not-semver").is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn protected_source_identity_fails_closed_until_the_broker_is_upgraded() {
        let home = env::var_os("HOME").map(PathBuf::from).unwrap();
        assert!(source_identity_needs_stable_broker(
            &home.join("Documents").join("herdr-mcp")
        ));

        // A linked worktree whose `.git` marker points into the protected main
        // repository must take the broker path too, and never shell `git`.
        let root = env::temp_dir().join(format!("herdr-mcp-dev-gitdir-test-{}", now_ms()));
        let worktree = root.join("checkout");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!(
                "gitdir: {}\n",
                home.join("Documents")
                    .join("herdr-mcp/.git/worktrees/example")
                    .display()
            ),
        )
        .unwrap();
        assert!(source_identity_needs_stable_broker(&worktree));
        assert!(!source_identity_needs_stable_broker(&root));

        let config = root.join("config");
        let mut runtime = RuntimePaths::discover().unwrap();
        runtime.config_dir = config.clone();
        let error = source_identity(&worktree, &runtime).unwrap_err();
        assert!(
            error.contains("herdr-mcp permissions setup --upgrade-broker"),
            "unexpected error: {error}"
        );

        // Once an identity-capable broker is installed the next missing
        // requirement is the live snapshot; neither step may fall back to a
        // direct `git` read.
        let broker_dir = config.join("tcc-broker");
        fs::create_dir_all(&broker_dir).unwrap();
        fs::write(broker_dir.join("herdr-mcp-broker"), b"fixture").unwrap();
        fs::write(
            broker_dir.join("metadata.json"),
            format!(
                "{{\"schema_version\":1,\"compat_revision\":{},\"preferred_signing_identifier\":\"{}\"}}",
                crate::tcc_broker::GIT_IDENTITY_MIN_COMPAT_REVISION,
                crate::tcc_broker::BROKER_SIGNING_IDENTIFIER
            ),
        )
        .unwrap();
        assert!(crate::tcc_broker::installed_supports_git_identity(&config));
        runtime.herdr_socket = None;
        let error = source_identity(&worktree, &runtime).unwrap_err();
        assert!(error.contains("Herdr socket"), "unexpected error: {error}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_native_checkout_from_nested_directory() {
        let root = env::temp_dir().join(format!("herdr-mcp-dev-test-{}", now_ms()));
        let nested = root.join("a").join("b");
        fs::create_dir_all(root.join("crates/herdr-mcp")).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(
            root.join("crates/herdr-mcp/Cargo.toml"),
            "[package]\nname='herdr-mcp'\nversion='0.0.0'\n",
        )
        .unwrap();

        assert_eq!(find_repo_root(&nested), Some(root.clone()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_generation_target_is_strict() {
        assert_eq!(
            generation_from_target(Path::new("generations/rust-abc123")).unwrap(),
            "rust-abc123"
        );
        assert!(generation_from_target(Path::new("../rust-abc123")).is_err());
        assert!(generation_from_target(Path::new("generations/dev-abc123")).is_err());
        assert!(generation_from_target(Path::new("generations/rust-abc123/extra")).is_err());
    }

    #[test]
    fn version_probe_uses_only_the_first_version_line() {
        assert_eq!(
            parse_version_output("herdr-mcp 0.4.2\ncontract epoch 2 / 18 tools\nstate schema 5\n")
                .unwrap(),
            "0.4.2"
        );
    }

    #[test]
    fn dev_target_version_comes_from_target_checkout_metadata() {
        let metadata = json!({
            "packages": [
                { "name": "some-sibling", "version": "1.0.0" },
                { "name": "herdr-mcp", "version": "9.8.7" }
            ]
        });
        assert_eq!(dev_version_from_metadata(&metadata).unwrap(), "9.8.7-dev");

        let missing = json!({ "packages": [{ "name": "some-sibling", "version": "1.0.0" }] });
        assert!(dev_version_from_metadata(&missing).is_err());
        let malformed = json!({ "packages": [{ "name": "herdr-mcp", "version": "not-semver" }] });
        assert!(dev_version_from_metadata(&malformed).is_err());
    }

    #[test]
    fn dev_activation_service_probe_waits_for_transient_health_but_fails_wrong_generation() {
        let generation = "rust-new";
        let mut probes = vec![
            json!({
                "ok": false,
                "healthy": false,
                "implementation": "rust",
                "generation": generation,
            }),
            json!({
                "ok": true,
                "healthy": true,
                "implementation": "rust",
                "generation": generation,
            }),
        ]
        .into_iter();
        let mut pauses = 0;
        let service = wait_for_dev_service_activation_with(
            generation,
            || Ok(probes.next().expect("two probes expected")),
            || true,
            || pauses += 1,
        )
        .unwrap();
        assert_eq!(service["healthy"], true);
        assert_eq!(pauses, 1);

        let wrong = json!({
            "ok": true,
            "healthy": true,
            "implementation": "rust",
            "generation": "rust-old",
        });
        assert!(
            service_activation_ready(generation, &wrong)
                .unwrap_err()
                .contains("service generation mismatch")
        );
    }

    #[test]
    fn dev_activation_service_probe_stops_after_bounded_unhealthy_window() {
        let generation = "rust-new";
        let unhealthy = json!({
            "ok": false,
            "healthy": false,
            "implementation": "rust",
            "generation": generation,
        });
        let mut retries = 1_u8;
        let error = wait_for_dev_service_activation_with(
            generation,
            || Ok(unhealthy.clone()),
            || {
                let retry = retries > 0;
                retries = retries.saturating_sub(1);
                retry
            },
            || {},
        )
        .unwrap_err();
        assert!(error.contains("post-activation convergence budget"));
    }

    #[test]
    fn dev_activation_gate_requires_measured_service_and_link_alignment() {
        let generation = "rust-new";
        let service = json!({ "ok": true, "healthy": true, "generation": generation });
        let link = json!({
            "ok": true,
            "production_owner": "rust",
            "agents": [{
                "label": "dev.herdr-mcp.link-prod",
                "loaded": true,
                "implementation": "rust",
                "points_at_managed_runtime": true,
                "points_at_repo_checkout": false,
            }],
            "production_runtime_alignment": {
                "current_generation": generation,
                "active_generation": generation,
                "runtime_control_active_matches_current": true,
                "loaded_matches_current": false,
                "loaded_environment_stale": true,
            }
        });
        let native = json!({ "ok": true, "runtime_matches_current": true });
        let error = validate_dev_activation_evidence(generation, &service, &link, Some(&native))
            .unwrap_err();
        assert!(error.contains("loaded_matches=false"));

        let stale = json!({
            "ok": true,
            "production_owner": "rust",
            "agents": [{
                "label": "dev.herdr-mcp.link-prod",
                "loaded": true,
                "implementation": "rust",
                "points_at_managed_runtime": true,
                "points_at_repo_checkout": false,
            }],
            "production_runtime_alignment": {
                "current_generation": generation,
                "active_generation": "rust-old",
                "runtime_control_active_matches_current": false,
            }
        });
        assert!(
            validate_dev_activation_evidence(generation, &service, &stale, Some(&native))
                .unwrap_err()
                .contains("production Link generation mismatch")
        );
    }

    #[test]
    fn dev_activation_gate_accepts_absent_native_host_but_rejects_partial_state() {
        let generation = "rust-new";
        let service = json!({ "ok": true, "healthy": true, "generation": generation });
        let link = json!({
            "ok": true,
            "production_owner": "rust",
            "agents": [{
                "label": "dev.herdr-mcp.link-prod",
                "loaded": true,
                "implementation": "rust",
                "points_at_managed_runtime": true,
                "points_at_repo_checkout": false,
            }],
            "production_runtime_alignment": {
                "current_generation": generation,
                "active_generation": generation,
                "runtime_control_active_matches_current": true,
                "loaded_matches_current": true,
            }
        });
        let absent = json!({
            "ok": false,
            "owned_manifest_count": 0,
            "wrapper_ok": false,
            "runtime_binary_ok": false,
            "runtime_matches_current": false,
        });
        let evidence =
            validate_dev_activation_evidence(generation, &service, &link, Some(&absent)).unwrap();
        assert_eq!(evidence["native_host"], "absent");

        let partial = json!({
            "ok": false,
            "owned_manifest_count": 0,
            "wrapper_ok": true,
            "runtime_binary_ok": false,
            "runtime_matches_current": false,
        });
        assert!(
            validate_dev_activation_evidence(generation, &service, &link, Some(&partial))
                .unwrap_err()
                .contains("partial/foreign/stale")
        );
    }

    #[test]
    fn interrupted_dev_recovery_requires_exact_active_runtime_identity() {
        let state = DevRuntimeState {
            schema_version: STATE_SCHEMA_VERSION,
            channel: "dev".to_owned(),
            target_version: "0.4.3-dev".to_owned(),
            source_repo: Some("/tmp/herdr-mcp".to_owned()),
            source_branch: Some("main".to_owned()),
            source_commit: Some("abc123".to_owned()),
            source_dirty: false,
            dev_generation: None,
            prod_generation: "rust-prod".to_owned(),
            prod_version: "0.4.2".to_owned(),
            prod_snapshot_binary: "/tmp/prod/herdr-mcp".to_owned(),
            prod_snapshot_sha256: "0".repeat(64),
            updated_at_ms: 1,
            last_transaction: None,
        };
        let identity = DevRecoveryIdentity {
            current_exe_is_active_runtime: true,
            runtime_channel: "dev",
            runtime_version: "0.4.3-dev",
            compiled_source_commit: Some("abc123"),
            compiled_source_dirty: false,
            checkout_repo: "/tmp/herdr-mcp",
            checkout_branch: Some("main"),
            checkout_commit: "abc123",
            checkout_dirty: false,
        };

        assert!(interrupted_dev_state_matches_identity(&state, &identity));
        assert!(!interrupted_dev_state_matches_identity(
            &state,
            &DevRecoveryIdentity {
                current_exe_is_active_runtime: false,
                ..identity
            },
        ));
        assert!(!interrupted_dev_state_matches_identity(
            &state,
            &DevRecoveryIdentity {
                runtime_channel: "prod",
                ..identity
            },
        ));
        assert!(!interrupted_dev_state_matches_identity(
            &state,
            &DevRecoveryIdentity {
                compiled_source_commit: Some("different"),
                ..identity
            },
        ));
        assert!(!interrupted_dev_state_matches_identity(
            &state,
            &DevRecoveryIdentity {
                compiled_source_dirty: true,
                ..identity
            },
        ));
        assert!(!interrupted_dev_state_matches_identity(
            &state,
            &DevRecoveryIdentity {
                checkout_commit: "different",
                ..identity
            },
        ));
        assert!(!interrupted_dev_state_matches_identity(
            &state,
            &DevRecoveryIdentity {
                checkout_repo: "/tmp/other-checkout",
                ..identity
            },
        ));

        let mut committed = state.clone();
        committed.dev_generation = Some("rust-dev".to_owned());
        assert!(!interrupted_dev_state_matches_identity(
            &committed, &identity,
        ));
    }

    #[test]
    fn interrupted_dev_recovery_dry_run_never_mutates_state() {
        let mut state = DevRuntimeState {
            schema_version: STATE_SCHEMA_VERSION,
            channel: "dev".to_owned(),
            target_version: "0.4.3-dev".to_owned(),
            source_repo: Some("/tmp/herdr-mcp".to_owned()),
            source_branch: Some("main".to_owned()),
            source_commit: Some("abc123".to_owned()),
            source_dirty: false,
            dev_generation: None,
            prod_generation: "rust-prod".to_owned(),
            prod_version: "0.4.2".to_owned(),
            prod_snapshot_binary: "/tmp/prod/herdr-mcp".to_owned(),
            prod_snapshot_sha256: "0".repeat(64),
            updated_at_ms: 1,
            last_transaction: None,
        };
        let before = state.clone();
        assert!(!mark_interrupted_dev_recovery_committed(
            &mut state, "rust-dev", 2, true,
        ));
        assert_eq!(state, before);

        assert!(mark_interrupted_dev_recovery_committed(
            &mut state, "rust-dev", 2, false,
        ));
        assert_eq!(state.dev_generation.as_deref(), Some("rust-dev"));
        assert_eq!(state.updated_at_ms, 2);
    }

    #[test]
    fn local_agent_skill_source_requires_clean_exact_dev_provenance() {
        let mut state = DevRuntimeState {
            schema_version: STATE_SCHEMA_VERSION,
            channel: "dev".to_owned(),
            target_version: "1.0.0-dev".to_owned(),
            source_repo: Some("/tmp/herdr-mcp".to_owned()),
            source_branch: Some("main".to_owned()),
            source_commit: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            source_dirty: false,
            dev_generation: Some("rust-dev".to_owned()),
            prod_generation: "rust-prod".to_owned(),
            prod_version: "0.4.8".to_owned(),
            prod_snapshot_binary: "/tmp/prod/herdr-mcp".to_owned(),
            prod_snapshot_sha256: "0".repeat(64),
            updated_at_ms: 1,
            last_transaction: None,
        };
        let commit = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            matching_clean_dev_source_repo(&state, commit),
            Some("/tmp/herdr-mcp")
        );
        assert_eq!(
            matching_clean_dev_source_repo(&state, &"f".repeat(40)),
            None
        );

        state.source_dirty = true;
        assert_eq!(matching_clean_dev_source_repo(&state, commit), None);
        state.source_dirty = false;
        state.channel = "prod".to_owned();
        assert_eq!(matching_clean_dev_source_repo(&state, commit), None);
    }

    #[test]
    fn ordinary_release_build_defaults_to_prod_identity() {
        assert_eq!(crate::runtime_meta::runtime_channel(), "prod");
        assert_eq!(
            crate::runtime_meta::runtime_version(),
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn legacy_schema_one_state_without_transaction_remains_readable() {
        let root = env::temp_dir().join(format!("herdr-mcp-dev-legacy-state-{}", now_ms()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("channel.json");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "channel": "dev",
                "target_version": "1.0.0-dev",
                "source_repo": "/tmp/repo",
                "source_branch": "main",
                "source_commit": "abc123",
                "source_dirty": false,
                "dev_generation": "rust-dev",
                "prod_generation": "rust-prod",
                "prod_version": "0.4.8",
                "prod_snapshot_binary": "/tmp/prod/herdr-mcp",
                "prod_snapshot_sha256": "0".repeat(64),
                "updated_at_ms": 1
            }))
            .unwrap(),
        )
        .unwrap();

        let state = read_state(&path).unwrap().unwrap();
        assert_eq!(state.schema_version, 1);
        assert_eq!(state.dev_generation.as_deref(), Some("rust-dev"));
        assert_eq!(state.last_transaction, None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn durable_transaction_phase_fences_interrupted_recovery() {
        let mut state = DevRuntimeState {
            schema_version: STATE_SCHEMA_VERSION,
            channel: "dev".to_owned(),
            target_version: "1.0.0-dev".to_owned(),
            source_repo: Some("/tmp/repo".to_owned()),
            source_branch: Some("main".to_owned()),
            source_commit: Some("abc123".to_owned()),
            source_dirty: false,
            dev_generation: None,
            prod_generation: "rust-prod".to_owned(),
            prod_version: "0.4.8".to_owned(),
            prod_snapshot_binary: "/tmp/prod/herdr-mcp".to_owned(),
            prod_snapshot_sha256: "0".repeat(64),
            updated_at_ms: 10,
            last_transaction: None,
        };
        let mut transaction = DevSyncTransaction {
            transaction_id: "dstx-test".to_owned(),
            phase: DevSyncPhase::Building,
            target_version: state.target_version.clone(),
            source_repo: state.source_repo.clone().unwrap(),
            source_branch: state.source_branch.clone(),
            source_commit: state.source_commit.clone().unwrap(),
            source_dirty: state.source_dirty,
            active_generation_before: "rust-old".to_owned(),
            expected_generation: Some("rust-new".to_owned()),
            generation: None,
            started_at_ms: 10,
            updated_at_ms: 10,
            completed_at_ms: None,
            error: None,
            activation_evidence: None,
        };

        state.last_transaction = Some(transaction.clone());
        assert!(!interrupted_transaction_allows_recovery(&state, "rust-new"));

        transaction.phase = DevSyncPhase::Activating;
        state.last_transaction = Some(transaction.clone());
        assert!(interrupted_transaction_allows_recovery(&state, "rust-new"));
        assert!(!interrupted_transaction_allows_recovery(
            &state,
            "rust-other"
        ));

        transaction.phase = DevSyncPhase::Reconciling;
        state.last_transaction = Some(transaction.clone());
        assert!(interrupted_transaction_allows_recovery(&state, "rust-new"));

        transaction.phase = DevSyncPhase::Succeeded;
        state.last_transaction = Some(transaction);
        assert!(!interrupted_transaction_allows_recovery(&state, "rust-new"));

        state.last_transaction = None;
        assert!(interrupted_transaction_allows_recovery(&state, "rust-new"));
    }

    #[test]
    fn building_transaction_preserves_active_channel_until_activation() {
        let existing = DevRuntimeState {
            schema_version: STATE_SCHEMA_VERSION,
            channel: "dev".to_owned(),
            target_version: "0.9.0-dev".to_owned(),
            source_repo: Some("/tmp/old".to_owned()),
            source_branch: Some("old".to_owned()),
            source_commit: Some("old-commit".to_owned()),
            source_dirty: false,
            dev_generation: Some("rust-old-dev".to_owned()),
            prod_generation: "rust-prod".to_owned(),
            prod_version: "0.4.8".to_owned(),
            prod_snapshot_binary: "/tmp/prod/herdr-mcp".to_owned(),
            prod_snapshot_sha256: "a".repeat(64),
            updated_at_ms: 1,
            last_transaction: None,
        };
        let base = dev_sync_base_state(
            Some(&existing),
            "1.0.0-dev",
            "rust-prod",
            "0.4.8",
            Path::new("/tmp/prod/herdr-mcp"),
            &"b".repeat(64),
            2,
        );
        assert_eq!(base.channel, "dev");
        assert_eq!(base.dev_generation.as_deref(), Some("rust-old-dev"));
        assert_eq!(base.target_version, "0.9.0-dev");

        let fresh = dev_sync_base_state(
            None,
            "1.0.0-dev",
            "rust-prod",
            "0.4.8",
            Path::new("/tmp/prod/herdr-mcp"),
            &"b".repeat(64),
            2,
        );
        assert_eq!(fresh.channel, "prod");
        assert_eq!(fresh.dev_generation, None);
        assert_eq!(fresh.prod_generation, "rust-prod");
    }

    #[test]
    fn compensation_is_rolled_back_only_after_exact_generation_readback() {
        use std::cell::Cell;

        let root = env::temp_dir().join(format!("herdr-mcp-dev-tx-test-{}", now_ms()));
        let state_path = root.join("runtime/channel.json");
        let base = DevRuntimeState {
            schema_version: STATE_SCHEMA_VERSION,
            channel: "dev".to_owned(),
            target_version: "0.9.0-dev".to_owned(),
            source_repo: Some("/tmp/old".to_owned()),
            source_branch: Some("old".to_owned()),
            source_commit: Some("old-commit".to_owned()),
            source_dirty: false,
            dev_generation: Some("rust-old".to_owned()),
            prod_generation: "rust-prod".to_owned(),
            prod_version: "0.4.8".to_owned(),
            prod_snapshot_binary: "/tmp/prod/herdr-mcp".to_owned(),
            prod_snapshot_sha256: "0".repeat(64),
            updated_at_ms: 1,
            last_transaction: None,
        };
        let source = SourceIdentity {
            branch: Some("main".to_owned()),
            commit: "new-commit".to_owned(),
            dirty: false,
        };
        let mut transaction =
            new_dev_sync_transaction(Path::new("/tmp/new"), &source, "1.0.0-dev", "rust-old", 10);
        transaction.phase = DevSyncPhase::Reconciling;
        transaction.expected_generation = Some("rust-new".to_owned());
        transaction.generation = Some("rust-new".to_owned());
        let rollback_calls = Cell::new(0usize);

        let error = compensate_and_record_dev_sync_transaction_with(
            &state_path,
            &base,
            &mut transaction,
            "DEV post-activation gate failed: synthetic failure".to_owned(),
            || {
                rollback_calls.set(rollback_calls.get() + 1);
                Ok(())
            },
            || Ok(Some("rust-old".to_owned())),
        );
        assert!(error.contains("synthetic failure"));
        assert_eq!(rollback_calls.get(), 1);
        let persisted = read_state(&state_path).unwrap().unwrap();
        assert_eq!(persisted.channel, "dev");
        assert_eq!(persisted.dev_generation.as_deref(), Some("rust-old"));
        let persisted_tx = persisted.last_transaction.unwrap();
        assert_eq!(persisted_tx.phase, DevSyncPhase::RolledBack);
        assert_eq!(
            persisted_tx.expected_generation.as_deref(),
            Some("rust-new")
        );
        assert_eq!(persisted_tx.generation.as_deref(), Some("rust-old"));
        assert!(persisted_tx.completed_at_ms.is_some());

        let mismatch_path = root.join("runtime/channel-mismatch.json");
        let mut mismatch =
            new_dev_sync_transaction(Path::new("/tmp/new"), &source, "1.0.0-dev", "rust-old", 30);
        mismatch.phase = DevSyncPhase::Reconciling;
        mismatch.expected_generation = Some("rust-new".to_owned());
        let error = compensate_and_record_dev_sync_transaction_with(
            &mismatch_path,
            &base,
            &mut mismatch,
            "DEV post-activation gate failed: synthetic mismatch".to_owned(),
            || Ok(()),
            || Ok(Some("rust-unexpected".to_owned())),
        );
        assert!(error.contains("expected rust-old"));
        assert_eq!(
            read_state(&mismatch_path)
                .unwrap()
                .unwrap()
                .last_transaction
                .unwrap()
                .phase,
            DevSyncPhase::Failed
        );

        assert_eq!(
            generation_from_sha256(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            )
            .unwrap(),
            "rust-0123456789abcdef"
        );
        assert!(generation_from_sha256("bad").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dev_runtime_version_predicate_matches_established_convention() {
        assert!(is_dev_runtime_version("0.4.3-dev"));
        assert!(is_dev_runtime_version("0.4.4-dev"));
        assert!(is_dev_runtime_version("1.0.0-dev"));
        assert!(!is_dev_runtime_version("0.4.3"));
        assert!(!is_dev_runtime_version("0.4.4"));
        assert!(!is_dev_runtime_version("0.4.3-beta.1"));
    }

    #[test]
    fn stale_dev_resync_requires_managed_valid_prod_snapshot_without_rewriting_provenance() {
        let root = env::temp_dir().join(format!("herdr-mcp-dev-drift-test-{}", now_ms()));
        let prod_dir = root.join("channels/prod");
        fs::create_dir_all(&prod_dir).unwrap();
        let prod_binary = prod_dir.join(executable_name("herdr-mcp"));
        fs::write(&prod_binary, b"pinned-prod-bytes").unwrap();
        let prod_sha = file_sha256(&prod_binary).unwrap();
        let paths = DevPaths {
            state: root.join("channel.json"),
            prod_binary: prod_binary.clone(),
            prod_dir,
        };
        let state = DevRuntimeState {
            schema_version: STATE_SCHEMA_VERSION,
            channel: "dev".to_owned(),
            target_version: "1.0.0-dev".to_owned(),
            source_repo: Some("/tmp/old-checkout".to_owned()),
            source_branch: Some("old-dev".to_owned()),
            source_commit: Some("old-commit".to_owned()),
            source_dirty: false,
            dev_generation: Some("rust-old-dev".to_owned()),
            prod_generation: "rust-prod".to_owned(),
            prod_version: "0.4.8".to_owned(),
            prod_snapshot_binary: prod_binary.to_string_lossy().into_owned(),
            prod_snapshot_sha256: prod_sha,
            updated_at_ms: 100,
            last_transaction: None,
        };
        let before = state.clone();

        validate_stale_dev_resync_snapshot(&state, &paths).unwrap();
        assert_eq!(
            state, before,
            "verified DEV drift must not rewrite stale provenance"
        );

        let mut wrong_path = state.clone();
        wrong_path.prod_snapshot_binary =
            root.join("other/herdr-mcp").to_string_lossy().into_owned();
        assert!(
            validate_stale_dev_resync_snapshot(&wrong_path, &paths)
                .unwrap_err()
                .contains("non-managed PROD snapshot path")
        );

        let mut wrong_sha = state.clone();
        wrong_sha.prod_snapshot_sha256 = "0".repeat(64);
        assert!(
            validate_stale_dev_resync_snapshot(&wrong_sha, &paths)
                .unwrap_err()
                .contains("fails SHA-256 validation")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_dev_state_reconciles_to_verified_prod_while_preserving_provenance() {
        let generation = "rust-9d973285cc085040";
        let service = json!({ "ok": true, "healthy": true, "generation": generation });
        let link = json!({
            "ok": true,
            "production_owner": "rust",
            "agents": [{
                "label": "dev.herdr-mcp.link-prod",
                "loaded": true,
                "implementation": "rust",
                "points_at_managed_runtime": true,
                "points_at_repo_checkout": false,
            }],
            "production_runtime_alignment": {
                "current_generation": generation,
                "active_generation": generation,
                "runtime_control_active_matches_current": true,
                "loaded_matches_current": true,
            }
        });
        let native = json!({ "ok": true, "runtime_matches_current": true });
        assert!(
            validate_dev_activation_evidence(generation, &service, &link, Some(&native)).is_ok()
        );

        let mut state = DevRuntimeState {
            schema_version: STATE_SCHEMA_VERSION,
            channel: "dev".to_owned(),
            target_version: "0.4.3-dev".to_owned(),
            source_repo: Some("/tmp/repo".to_owned()),
            source_branch: Some("main".to_owned()),
            source_commit: Some("abc123".to_owned()),
            source_dirty: false,
            dev_generation: Some("rust-3979663c8cc7e4f7".to_owned()),
            prod_generation: "rust-old-prod".to_owned(),
            prod_version: "0.4.3-dev".to_owned(),
            prod_snapshot_binary: "/tmp/old".to_owned(),
            prod_snapshot_sha256: "old-sha".to_owned(),
            updated_at_ms: 100,
            last_transaction: None,
        };

        transition_state_to_prod(
            &mut state,
            generation,
            "0.4.3",
            Path::new("/tmp/channels/prod/herdr-mcp"),
            "new-sha".to_owned(),
            200,
        );

        assert_eq!(state.channel, "prod");
        assert_eq!(state.prod_generation, generation);
        assert_eq!(state.prod_version, "0.4.3");
        assert_eq!(state.prod_snapshot_binary, "/tmp/channels/prod/herdr-mcp");
        assert_eq!(state.prod_snapshot_sha256, "new-sha");
        assert_eq!(state.updated_at_ms, 200);
        assert_eq!(state.target_version, "0.4.3-dev");
        assert_eq!(state.source_commit.as_deref(), Some("abc123"));
        assert_eq!(
            state.dev_generation.as_deref(),
            Some("rust-3979663c8cc7e4f7")
        );
    }
}
