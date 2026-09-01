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

const DEV_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-dev");
const STATE_SCHEMA_VERSION: u32 = 1;

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
    let source = source_identity(&repo)?;
    if source.dirty && !allow_dirty {
        return Err(
            "dev sync refuses a dirty source tree by default; commit/stash the changes or rerun with --allow-dirty so the provenance is explicit"
                .to_owned(),
        );
    }

    let runtime = RuntimePaths::discover()?;
    let paths = dev_paths(&runtime);
    let active_before = current_generation(&runtime.config_dir)?.ok_or_else(|| {
        "dev sync requires an installed managed PROD runtime/current generation".to_owned()
    })?;
    let existing = read_state(&paths.state)?;
    if let Some(state) = existing.as_ref()
        && state.channel == "dev"
        && state.dev_generation.as_deref() != Some(active_before.as_str())
    {
        return Err(format!(
            "dev runtime state drift: state expects {:?} but runtime/current is {active_before}; run `herdr-mcp dev status` before another sync",
            state.dev_generation
        ));
    }

    let prod_generation = match existing.as_ref() {
        Some(state) if state.channel == "dev" => state.prod_generation.clone(),
        _ => active_before.clone(),
    };
    let prod_version = match existing.as_ref() {
        Some(state) if state.channel == "dev" => state.prod_version.clone(),
        _ => binary_version(&runtime.config_dir.join("runtime/current/herdr-mcp"))?,
    };
    let plan = json!({
        "ok": true,
        "action": "dev_sync",
        "channel_from": existing.as_ref().map(|state| state.channel.as_str()).unwrap_or("prod"),
        "channel_to": "dev",
        "target_version": DEV_VERSION,
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
    build_dev_binary(&repo, &source)?;
    let built_binary = repo
        .join("target")
        .join("release")
        .join(executable_name("herdr-mcp"));
    verify_dev_binary(&built_binary)?;
    let built_sha = file_sha256(&built_binary)?;
    let expected_dev_generation = generation_from_sha256(&built_sha)?;

    // Persist the PROD recovery source before the service transaction. If the
    // independent terminal disappears during activation, `dev rollback` still
    // has a verified immutable PROD binary instead of relying on "previous",
    // which may already be another DEV generation after repeated syncs.
    let previous_state = existing.clone();
    let mut state = DevRuntimeState {
        schema_version: STATE_SCHEMA_VERSION,
        channel: "dev".to_owned(),
        target_version: DEV_VERSION.to_owned(),
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
    };
    write_state(&paths.state, &state)?;
    if let Err(error) = run_service_install(&built_binary) {
        let restore_error = restore_state(&paths.state, previous_state.as_ref()).err();
        return Err(match restore_error {
            Some(restore_error) => {
                format!("{error}; DEV channel-state restore also failed: {restore_error}")
            }
            None => error,
        });
    }

    let active_after = match current_generation(&runtime.config_dir) {
        Ok(Some(generation)) => generation,
        Ok(None) => {
            return Err(compensate_post_install_failure(
                "DEV post-install generation read failed",
                "dev service activation succeeded but runtime/current is missing",
                expected_dev_generation != active_before,
                || run_service_rollback(&built_binary),
                || restore_state(&paths.state, previous_state.as_ref()),
            ));
        }
        Err(error) => {
            return Err(compensate_post_install_failure(
                "DEV post-install generation read failed",
                &error,
                expected_dev_generation != active_before,
                || run_service_rollback(&built_binary),
                || restore_state(&paths.state, previous_state.as_ref()),
            ));
        }
    };
    let activation_evidence = match verify_dev_activation(&runtime, &active_after) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Err(compensate_post_install_failure(
                "DEV post-activation gate failed",
                &error,
                active_after != active_before,
                || run_service_rollback(&built_binary),
                || restore_state(&paths.state, previous_state.as_ref()),
            ));
        }
    };
    state.dev_generation = Some(active_after.clone());
    state.updated_at_ms = now_ms();
    persist_committed_dev_state(
        || write_state(&paths.state, &state),
        active_after != active_before,
        || run_service_rollback(&built_binary),
        || restore_state(&paths.state, previous_state.as_ref()),
    )?;

    print_json(&json!({
        "ok": true,
        "action": "dev_sync",
        "channel": "dev",
        "version": DEV_VERSION,
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
    }))?;
    Ok(ExitCode::SUCCESS)
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

fn source_identity(repo: &Path) -> Result<SourceIdentity, String> {
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

fn build_dev_binary(repo: &Path, source: &SourceIdentity) -> Result<(), String> {
    let status = Command::new("cargo")
        .args(["build", "--release", "--locked", "-p", "herdr-mcp"])
        .current_dir(repo)
        .env("HERDR_MCP_BUILD_CHANNEL", "dev")
        .env("HERDR_MCP_BUILD_VERSION", DEV_VERSION)
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

fn verify_dev_binary(binary: &Path) -> Result<(), String> {
    let version = binary_version(binary)?;
    if version != DEV_VERSION {
        return Err(format!(
            "DEV binary identity mismatch: expected {DEV_VERSION}, observed {version}"
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

fn compensate_post_install_failure<Rollback, Restore>(
    context: &str,
    error: &str,
    runtime_changed: bool,
    mut rollback: Rollback,
    mut restore: Restore,
) -> String
where
    Rollback: FnMut() -> Result<(), String>,
    Restore: FnMut() -> Result<(), String>,
{
    let rollback_error = runtime_changed.then(|| rollback().err()).flatten();
    let restore_error = restore().err();
    format!(
        "{context}: {error}{}{}",
        rollback_error
            .map(|error| format!("; service rollback failed: {error}"))
            .unwrap_or_default(),
        restore_error
            .map(|error| format!("; DEV channel-state restore failed: {error}"))
            .unwrap_or_default(),
    )
}

fn persist_committed_dev_state<Write, Rollback, Restore>(
    mut write: Write,
    runtime_changed: bool,
    mut rollback: Rollback,
    mut restore: Restore,
) -> Result<(), String>
where
    Write: FnMut() -> Result<(), String>,
    Rollback: FnMut() -> Result<(), String>,
    Restore: FnMut() -> Result<(), String>,
{
    let Err(state_error) = write() else {
        return Ok(());
    };
    let rollback_error = runtime_changed.then(|| rollback().err()).flatten();
    let restore_error = restore().err();
    Err(format!(
        "DEV channel-state commit failed after activation: {state_error}{}{}",
        rollback_error
            .map(|error| format!("; service rollback failed: {error}"))
            .unwrap_or_default(),
        restore_error
            .map(|error| format!("; DEV channel-state restore failed: {error}"))
            .unwrap_or_default(),
    ))
}

#[cfg(target_os = "macos")]
fn verify_dev_activation(runtime: &RuntimePaths, generation: &str) -> Result<Value, String> {
    let service = crate::service_manager::doctor_status()?;
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is required for DEV activation verification".to_owned())?;
    let link = crate::link::ownership::collect_status_report(&home, &runtime.config_dir);
    let native_host = crate::native_host_install::doctor_status()?;
    validate_dev_activation_evidence(generation, &service, &link, Some(&native_host))
}

#[cfg(not(target_os = "macos"))]
fn verify_dev_activation(_runtime: &RuntimePaths, _generation: &str) -> Result<Value, String> {
    Err(
        "DEV activation verification currently requires macOS service/Link ownership evidence"
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
    if current != generation || active != generation || !control_matches {
        return Err(format!(
            "production Link generation mismatch after DEV activation: expected={generation} current={current} active={active} control_matches={control_matches}"
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

fn dev_paths(runtime: &RuntimePaths) -> DevPaths {
    let runtime_root = runtime.config_dir.join("runtime");
    let prod_dir = runtime_root.join("channels").join("prod");
    DevPaths {
        state: runtime_root.join("channel.json"),
        prod_binary: prod_dir.join(executable_name("herdr-mcp")),
        prod_dir,
    }
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

fn restore_state(path: &Path, previous: Option<&DevRuntimeState>) -> Result<(), String> {
    match previous {
        Some(state) => write_state(path, state),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("cannot remove prepared DEV runtime state: {error}")),
        },
    }
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
                "loaded_environment_stale": true,
            }
        });
        let native = json!({ "ok": true, "runtime_matches_current": true });
        let evidence =
            validate_dev_activation_evidence(generation, &service, &link, Some(&native)).unwrap();
        assert_eq!(evidence["server_link_generation_reconciled"], true);
        assert_eq!(evidence["link_loaded_environment_stale"], true);
        assert_eq!(evidence["native_host"], "current");

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
    fn ordinary_release_build_defaults_to_prod_identity() {
        assert_eq!(crate::runtime_meta::runtime_channel(), "prod");
        assert_eq!(
            crate::runtime_meta::runtime_version(),
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn final_dev_state_commit_failure_compensates_runtime_and_state() {
        use std::cell::Cell;

        let rollback_calls = Cell::new(0usize);
        let restore_calls = Cell::new(0usize);
        let error = persist_committed_dev_state(
            || Err("synthetic state write failure".to_owned()),
            true,
            || {
                rollback_calls.set(rollback_calls.get() + 1);
                Ok(())
            },
            || {
                restore_calls.set(restore_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.contains("DEV channel-state commit failed after activation"));
        assert_eq!(rollback_calls.get(), 1);
        assert_eq!(restore_calls.get(), 1);
    }

    #[test]
    fn post_install_generation_read_failure_uses_expected_generation_for_compensation() {
        use std::cell::Cell;

        assert_eq!(
            generation_from_sha256(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            )
            .unwrap(),
            "rust-0123456789abcdef"
        );
        assert!(generation_from_sha256("bad").is_err());

        let rollback_calls = Cell::new(0usize);
        let restore_calls = Cell::new(0usize);
        let error = compensate_post_install_failure(
            "DEV post-install generation read failed",
            "synthetic runtime/current read failure",
            true,
            || {
                rollback_calls.set(rollback_calls.get() + 1);
                Ok(())
            },
            || {
                restore_calls.set(restore_calls.get() + 1);
                Ok(())
            },
        );
        assert!(error.contains("synthetic runtime/current read failure"));
        assert_eq!(rollback_calls.get(), 1);
        assert_eq!(restore_calls.get(), 1);
    }
}
