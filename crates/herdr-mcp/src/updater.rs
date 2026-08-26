//! Native release updater.
//!
//! The updater intentionally does not own launchd or generation activation.
//! It verifies a release manifest + raw binary, stages the candidate under the
//! local state directory, then launches that downloaded binary as a detached
//! worker. The worker reuses `service install`, which already owns generation
//! staging, health verification, automatic rollback, and service evidence.

#[cfg(target_os = "macos")]
use crate::cli::ServiceCommand;
use crate::cli::UpdateCommand;
use crate::contract;
use crate::paths::RuntimePaths;
use crate::release_trust::{self, ReleaseIdentity};
#[cfg(target_os = "macos")]
use crate::service_manager;
use crate::state_store::SCHEMA_VERSION;
use crate::updater_store::{UpdateJobRecord, UpdateStore};
use reqwest::blocking::{Client, Response};
use semver::Version;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::env;
#[cfg(target_os = "macos")]
use std::fs::{self, OpenOptions};
use std::io::Read;
#[cfg(target_os = "macos")]
use std::io::Write;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
use std::process::ExitCode;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

#[cfg(target_os = "macos")]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(target_os = "macos")]
use std::os::unix::process::CommandExt;

const DEFAULT_RELEASES_API_URL: &str =
    "https://api.github.com/repos/whshang/herdr-mcp/releases?per_page=20";
const RELEASES_MAX_BYTES: usize = 1024 * 1024;
const MANIFEST_MAX_BYTES: usize = 1024 * 1024;
const ATTESTATION_MAX_BYTES: usize = 2 * 1024 * 1024;
const BINARY_MAX_BYTES: u64 = 64 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleaseAsset {
    target: String,
    name: String,
    size: u64,
    sha256: String,
    url: Url,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleasePlan {
    version: Version,
    tag: String,
    identity: ReleaseIdentity,
    asset: ReleaseAsset,
}

pub fn run(command: UpdateCommand) -> Result<ExitCode, String> {
    match command {
        UpdateCommand::Check { manifest_url } => check(manifest_url.as_deref()),
        UpdateCommand::Apply { manifest_url } => apply(manifest_url.as_deref()),
        UpdateCommand::Status => status(),
        UpdateCommand::Worker { job_id } => worker(&job_id),
    }
}

fn check(manifest_override: Option<&str>) -> Result<ExitCode, String> {
    let plan = fetch_release_plan(manifest_override)?;
    let current = current_version()?;
    print_json(&json!({
        "ok": true,
        "code": "update_check",
        "current_version": current.to_string(),
        "available": plan.version > current,
        "release_version": plan.version.to_string(),
        "tag": plan.tag,
        "source_commit": plan.identity.source_commit,
        "repository": plan.identity.repository,
        "provenance_verified": true,
        "target": plan.asset.target,
        "asset": plan.asset.name,
        "sha256": plan.asset.sha256,
        "size": plan.asset.size,
    }))?;
    Ok(ExitCode::SUCCESS)
}

fn apply(manifest_override: Option<&str>) -> Result<ExitCode, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = manifest_override;
        Err("native update apply currently requires macOS service manager".to_owned())
    }

    #[cfg(target_os = "macos")]
    {
        let plan = fetch_release_plan(manifest_override)?;
        let current = current_version()?;
        if plan.version <= current {
            return Err(format!(
                "release {} is not newer than current {}; refusing downgrade/reinstall",
                plan.version, current
            ));
        }
        let paths = RuntimePaths::discover()?;
        let store = UpdateStore::open(&paths)?;
        recover_or_reject_active_update(&store, &paths)?;
        let (job_id, binary_path) = stage_release(&paths, &plan)?;
        if let Err(error) = probe_candidate_binary(&binary_path, &plan.version) {
            cleanup_staging(&binary_path);
            return Err(error);
        }

        let now = now_ms_i64();
        let job = UpdateJobRecord {
            job_id: job_id.clone(),
            version: plan.version.to_string(),
            target: plan.asset.target.clone(),
            asset_name: plan.asset.name.clone(),
            sha256: plan.asset.sha256.clone(),
            binary_path: binary_path.to_string_lossy().into_owned(),
            state: "queued".to_owned(),
            detail: Some("verified candidate staged".to_owned()),
            worker_pid: None,
            created_at: now,
            updated_at: now,
        };
        if let Err(error) = store.create_update_job(&job) {
            cleanup_staging(&binary_path);
            return Err(error);
        }

        let child = spawn_worker(&paths, &binary_path, &job_id);
        let child = match child {
            Ok(child) => child,
            Err(error) => {
                let _ = store.update_update_job(
                    &job_id,
                    "failed",
                    Some("worker spawn failed"),
                    None,
                    now_ms_i64(),
                );
                cleanup_staging(&binary_path);
                return Err(error);
            }
        };
        let worker_pid_persisted = store
            .set_update_worker_pid(&job_id, child.id(), now_ms_i64())
            .is_ok();

        print_json(&json!({
            "ok": true,
            "code": "update_queued",
            "job_id": job_id,
            "version": plan.version.to_string(),
            "target": plan.asset.target,
            "asset": plan.asset.name,
            "worker_pid": child.id(),
            "worker_pid_persisted": worker_pid_persisted,
        }))?;
        Ok(ExitCode::SUCCESS)
    }
}

fn status() -> Result<ExitCode, String> {
    let paths = RuntimePaths::discover()?;
    let store = UpdateStore::open(&paths)?;
    let latest = store.latest_update_job()?;
    let payload = match latest {
        Some(job) => json!({
            "ok": true,
            "code": "update_status",
            "job": public_job_view(&job),
        }),
        None => json!({
            "ok": true,
            "code": "update_status",
            "job": null,
        }),
    };
    print_json(&payload)?;
    Ok(ExitCode::SUCCESS)
}

fn worker(job_id: &str) -> Result<ExitCode, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = job_id;
        Err("native update worker currently requires macOS service manager".to_owned())
    }

    #[cfg(target_os = "macos")]
    {
        if !valid_job_id(job_id) {
            return Err("invalid update job id".to_owned());
        }
        let paths = RuntimePaths::discover()?;
        let store = UpdateStore::open(&paths)?;
        let job = store
            .update_job(job_id)?
            .ok_or_else(|| "update job not found".to_owned())?;
        if job.state != "queued" {
            return Err(format!(
                "update job {job_id} is {}, expected queued",
                job.state
            ));
        }
        let binary = PathBuf::from(&job.binary_path);
        if !binary_is_confined(&paths, &binary, job_id) {
            store.update_update_job(
                job_id,
                "failed",
                Some("staged binary escaped update job directory"),
                None,
                now_ms_i64(),
            )?;
            return Err("staged update binary is outside its job directory".to_owned());
        }
        verify_staged_file(&binary, &job.sha256)?;
        probe_candidate_binary(
            &binary,
            &Version::parse(&job.version)
                .map_err(|_| "durable update job contains an invalid version".to_owned())?,
        )?;
        store.update_update_job(
            job_id,
            "installing",
            Some("candidate verified; service install started"),
            None,
            now_ms_i64(),
        )?;

        let install = service_manager::run(ServiceCommand::Install { adopt_node: false });
        let succeeded = matches!(install, Ok(code) if code == ExitCode::SUCCESS);
        if succeeded {
            store.update_update_job(
                job_id,
                "succeeded",
                Some("service install committed and health gate passed"),
                None,
                now_ms_i64(),
            )?;
            cleanup_staging(&binary);
            Ok(ExitCode::SUCCESS)
        } else {
            let detail = match &install {
                Ok(_) => "service install returned non-zero",
                Err(_) => "service install failed; service manager rollback policy applied",
            };
            store.update_update_job(job_id, "failed", Some(detail), None, now_ms_i64())?;
            cleanup_staging(&binary);
            install
        }
    }
}

fn fetch_release_plan(manifest_override: Option<&str>) -> Result<ReleasePlan, String> {
    let client = update_client()?;
    let manifest_url = match manifest_override
        .map(str::to_owned)
        .or_else(|| env::var("HERDR_MCP_UPDATE_MANIFEST_URL").ok())
    {
        Some(raw) => parse_update_url(&raw)?,
        None => discover_default_manifest_url(&client)?,
    };
    let bytes = fetch_bounded(
        &client,
        manifest_url,
        MANIFEST_MAX_BYTES,
        "release manifest",
    )?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("release manifest is invalid JSON: {error}"))?;
    let plan = parse_release_plan(&value, current_target()?)?;
    let manifest_sha256 = sha256_bytes(&bytes);
    verify_artifact_attestation(
        &client,
        "release-manifest.json",
        &manifest_sha256,
        &plan.identity,
    )?;
    Ok(plan)
}

fn discover_default_manifest_url(client: &Client) -> Result<Url, String> {
    let releases_url = Url::parse(DEFAULT_RELEASES_API_URL)
        .map_err(|_| "default GitHub releases API URL is invalid".to_owned())?;
    let bytes = fetch_bounded(
        client,
        releases_url,
        RELEASES_MAX_BYTES,
        "GitHub releases index",
    )?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("GitHub releases index is invalid JSON: {error}"))?;
    let tag = select_release_tag(&value)?;
    Url::parse(&format!(
        "https://github.com/{}/releases/download/{tag}/release-manifest.json",
        release_trust::RELEASE_REPOSITORY
    ))
    .map_err(|_| "cannot construct discovered release manifest URL".to_owned())
}

fn select_release_tag(value: &Value) -> Result<String, String> {
    let releases = value
        .as_array()
        .ok_or_else(|| "GitHub releases index must be a JSON array".to_owned())?;
    let mut best: Option<(Version, String)> = None;
    for release in releases {
        let Some(object) = release.as_object() else {
            continue;
        };
        if object.get("draft").and_then(Value::as_bool) != Some(false) {
            continue;
        }
        let Some(tag) = object.get("tag_name").and_then(Value::as_str) else {
            continue;
        };
        let Some(version_text) = tag.strip_prefix('v') else {
            continue;
        };
        let Ok(version) = Version::parse(version_text) else {
            continue;
        };
        if tag != format!("v{version}") {
            continue;
        }
        let has_manifest = object
            .get("assets")
            .and_then(Value::as_array)
            .is_some_and(|assets| {
                assets.iter().any(|asset| {
                    asset.get("name").and_then(Value::as_str) == Some("release-manifest.json")
                })
            });
        if !has_manifest {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(best_version, _)| version > *best_version)
        {
            best = Some((version, tag.to_owned()));
        }
    }
    best.map(|(_, tag)| tag).ok_or_else(|| {
        "GitHub releases index contains no non-draft semver release with release-manifest.json"
            .to_owned()
    })
}

fn parse_release_plan(value: &Value, target: &str) -> Result<ReleasePlan, String> {
    if value.get("schema_version").and_then(Value::as_u64)
        != Some(release_trust::MANIFEST_SCHEMA_VERSION)
    {
        return Err("unsupported release manifest schema".to_owned());
    }
    if value.get("product").and_then(Value::as_str) != Some("herdr-mcp") {
        return Err("release manifest product mismatch".to_owned());
    }
    if value.get("state_schema").and_then(Value::as_i64) != Some(SCHEMA_VERSION) {
        return Err(format!(
            "release state schema is not rollback-compatible with local schema {SCHEMA_VERSION}"
        ));
    }
    let version_text = value
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| "release manifest is missing version".to_owned())?;
    let version = Version::parse(version_text)
        .map_err(|_| "release manifest version is not semver".to_owned())?;
    let tag = value
        .get("tag")
        .and_then(Value::as_str)
        .filter(|tag| *tag == format!("v{version}"))
        .ok_or_else(|| "release manifest tag/version mismatch".to_owned())?
        .to_owned();
    let identity = release_trust::parse_manifest_identity(value, &tag)?;

    let expected = contract::identity()?;
    let contract = value
        .get("contract")
        .and_then(Value::as_object)
        .ok_or_else(|| "release manifest is missing contract identity".to_owned())?;
    if contract.get("epoch").and_then(Value::as_u64) != Some(u64::from(expected.epoch))
        || contract.get("hash").and_then(Value::as_str) != Some(expected.hash.as_str())
        || contract.get("tool_count").and_then(Value::as_u64)
            != Some(u64::from(expected.tool_count))
    {
        return Err("release manifest contract identity mismatch".to_owned());
    }

    let assets = value
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| "release manifest is missing assets".to_owned())?;
    let matches = assets
        .iter()
        .filter(|asset| asset.get("target").and_then(Value::as_str) == Some(target))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "release manifest must contain exactly one asset for target {target}"
        ));
    }
    let asset = matches[0];
    let name = asset
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| valid_asset_name(name))
        .ok_or_else(|| "release asset name is invalid".to_owned())?
        .to_owned();
    let size = asset
        .get("size")
        .and_then(Value::as_u64)
        .filter(|size| *size > 0 && *size <= BINARY_MAX_BYTES)
        .ok_or_else(|| "release asset size is invalid or too large".to_owned())?;
    let sha256 = asset
        .get("sha256")
        .and_then(Value::as_str)
        .filter(|hash| valid_sha256(hash))
        .ok_or_else(|| "release asset sha256 is invalid".to_owned())?
        .to_owned();
    let asset_url = asset
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| "release asset URL is missing".to_owned())?;
    let url = parse_update_url(asset_url)?;
    let expected_url = Url::parse(&format!(
        "https://github.com/{}/releases/download/{}/{}",
        identity.repository, tag, name
    ))
    .map_err(|_| "cannot construct expected release asset URL".to_owned())?;
    if url != expected_url {
        return Err("release asset URL does not match trusted repository/tag/name".to_owned());
    }
    Ok(ReleasePlan {
        version,
        tag,
        identity,
        asset: ReleaseAsset {
            target: target.to_owned(),
            name,
            size,
            sha256,
            url,
        },
    })
}

#[cfg(target_os = "macos")]
fn recover_or_reject_active_update(
    store: &UpdateStore,
    paths: &RuntimePaths,
) -> Result<(), String> {
    let Some(active) = store.active_update_job()? else {
        return Ok(());
    };
    let now = now_ms_i64();
    let worker_alive = active.worker_pid.is_some_and(process_alive);
    let awaiting_pid =
        active.worker_pid.is_none() && now.saturating_sub(active.updated_at) < 30_000;
    if worker_alive || awaiting_pid {
        return Err(format!(
            "update already in progress: {} ({})",
            active.job_id, active.state
        ));
    }
    store.update_update_job(
        &active.job_id,
        "failed",
        Some("previous update worker is no longer running"),
        None,
        now,
    )?;
    let binary = PathBuf::from(&active.binary_path);
    if binary_is_confined(paths, &binary, &active.job_id) {
        cleanup_staging(&binary);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn stage_release(paths: &RuntimePaths, plan: &ReleasePlan) -> Result<(String, PathBuf), String> {
    let root = paths.config_dir.join("update").join("jobs");
    ensure_real_dir(&paths.config_dir)?;
    ensure_real_dir(&paths.config_dir.join("update"))?;
    ensure_real_dir(&root)?;
    let job_id = format!(
        "upd-{}-{}-{}",
        now_ms_i64(),
        std::process::id(),
        plan.asset.sha256.get(..8).unwrap_or("candidate")
    );
    let job_dir = root.join(&job_id);
    fs::create_dir(&job_dir)
        .map_err(|error| format!("cannot create update job directory: {error}"))?;
    #[cfg(unix)]
    fs::set_permissions(&job_dir, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure update job directory: {error}"))?;
    let binary = job_dir.join(if cfg!(windows) {
        "herdr-mcp-candidate.exe"
    } else {
        "herdr-mcp-candidate"
    });
    let client = update_client()?;
    verify_artifact_attestation(
        &client,
        &plan.asset.name,
        &plan.asset.sha256,
        &plan.identity,
    )?;
    if let Err(error) = download_asset(&client, &plan.asset, &binary) {
        let _ = fs::remove_dir_all(&job_dir);
        return Err(error);
    }
    Ok((job_id, binary))
}

#[cfg(target_os = "macos")]
fn download_asset(client: &Client, asset: &ReleaseAsset, target: &Path) -> Result<(), String> {
    let mut response = client
        .get(asset.url.clone())
        .send()
        .map_err(|error| format!("release asset download failed: {}", error_kind(&error)))?;
    validate_response(&response)?;
    if let Some(length) = response.content_length()
        && length != asset.size
    {
        return Err("release asset content-length does not match manifest".to_owned());
    }
    let temp = target.with_extension(format!("tmp-{}", std::process::id()));
    #[cfg(unix)]
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|error| format!("cannot create staged update binary: {error}"))?;
    #[cfg(not(unix))]
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|error| format!("cannot create staged update binary: {error}"))?;
    let result = (|| {
        let mut digest = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|error| format!("cannot read release asset: {error}"))?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(read as u64);
            if total > asset.size || total > BINARY_MAX_BYTES {
                return Err("release asset exceeded declared size".to_owned());
            }
            digest.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .map_err(|error| format!("cannot write staged update binary: {error}"))?;
        }
        if total != asset.size {
            return Err("release asset byte count does not match manifest".to_owned());
        }
        let actual = format!("{:x}", digest.finalize());
        if actual != asset.sha256 {
            return Err("release asset sha256 mismatch".to_owned());
        }
        file.sync_all()
            .map_err(|error| format!("cannot sync staged update binary: {error}"))?;
        #[cfg(unix)]
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot make staged update binary executable: {error}"))?;
        fs::rename(&temp, target)
            .map_err(|error| format!("cannot activate staged update binary: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(target_os = "macos")]
fn verify_staged_file(path: &Path, expected_sha256: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect staged update binary: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("staged update binary is not a real file".to_owned());
    }
    let mut file = fs::File::open(path)
        .map_err(|error| format!("cannot open staged update binary: {error}"))?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash staged update binary: {error}"))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > BINARY_MAX_BYTES {
            return Err("staged update binary is too large".to_owned());
        }
        digest.update(&buffer[..read]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected_sha256 {
        return Err("staged update binary changed after verification".to_owned());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn probe_candidate_binary(path: &Path, expected: &Version) -> Result<(), String> {
    let output = Command::new(path)
        .arg("version")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot execute staged update binary: {error}"))?;
    if !output.status.success() || output.stdout.len() > 16 * 1024 {
        return Err("staged update binary version probe failed".to_owned());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let expected_first = format!("herdr-mcp {expected}");
    if text.lines().next() != Some(expected_first.as_str())
        || !text.contains("contract epoch 2 / 18 tools")
        || !text.contains(&format!("state schema {SCHEMA_VERSION}"))
    {
        return Err("staged update binary identity does not match release manifest".to_owned());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn spawn_worker(
    paths: &RuntimePaths,
    binary: &Path,
    job_id: &str,
) -> Result<std::process::Child, String> {
    let mut command = Command::new(binary);
    command
        .arg("update")
        .arg("worker")
        .arg("--job")
        .arg(job_id)
        .env("HERDR_MCP_CONFIG_DIR", &paths.config_dir)
        .env_remove("HERDR_MCP_EXEC_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
        .spawn()
        .map_err(|error| format!("cannot spawn detached update worker: {error}"))
}

fn fetch_bounded(client: &Client, url: Url, max: usize, label: &str) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(url)
        .send()
        .map_err(|error| format!("{label} download failed: {}", error_kind(&error)))?;
    validate_response(&response)?;
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return Err(format!("{label} is too large"));
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {label}: {error}"))?;
    if bytes.len() > max {
        return Err(format!("{label} is too large"));
    }
    Ok(bytes)
}

fn verify_artifact_attestation(
    client: &Client,
    artifact_name: &str,
    sha256: &str,
    identity: &ReleaseIdentity,
) -> Result<(), String> {
    let url = release_trust::attestation_api_url(sha256)?;
    let mut response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(|error| format!("GitHub attestation lookup failed: {}", error_kind(&error)))?;
    validate_response(&response)?;
    if response
        .content_length()
        .is_some_and(|length| length > ATTESTATION_MAX_BYTES as u64)
    {
        return Err("GitHub attestation response is too large".to_owned());
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(ATTESTATION_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read GitHub attestation response: {error}"))?;
    if bytes.len() > ATTESTATION_MAX_BYTES {
        return Err("GitHub attestation response is too large".to_owned());
    }
    release_trust::verify_github_attestations(&bytes, artifact_name, sha256, identity)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn update_client() -> Result<Client, String> {
    Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(format!("herdr-mcp-updater/{}", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many update redirects");
            }
            if update_redirect_allowed(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|error| format!("cannot create update HTTP client: {error}"))
}

fn validate_response(response: &Response) -> Result<(), String> {
    if !update_url_allowed(response.url()) {
        return Err("release download redirected to a disallowed URL".to_owned());
    }
    if !response.status().is_success() {
        return Err(format!(
            "release download returned HTTP {}",
            response.status().as_u16()
        ));
    }
    Ok(())
}

fn parse_update_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| "update URL is invalid".to_owned())?;
    if !update_url_allowed(&url) {
        return Err("update URL must be HTTPS or loopback HTTP without credentials".to_owned());
    }
    Ok(url)
}

fn update_url_allowed(url: &Url) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if url.scheme() == "https" {
        return true;
    }
    if url.scheme() != "http" {
        return false;
    }
    loopback_host(url.host_str())
}

fn update_redirect_allowed(url: &Url) -> bool {
    if !update_url_allowed(url) {
        return false;
    }
    if loopback_host(url.host_str()) {
        return true;
    }
    matches!(
        url.host_str().map(|host| host.to_ascii_lowercase()),
        Some(host)
            if host == "github.com"
                || host == "api.github.com"
                || host == "objects.githubusercontent.com"
                || host == "release-assets.githubusercontent.com"
    )
}

fn loopback_host(host: Option<&str>) -> bool {
    matches!(
        host.map(str::to_ascii_lowercase),
        Some(host) if host == "localhost" || host == "127.0.0.1" || host == "::1"
    )
}

fn current_target() -> Result<&'static str, String> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        (os, arch) => Err(format!("unsupported update target {os}/{arch}")),
    }
}

fn current_version() -> Result<Version, String> {
    Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|_| "current herdr-mcp version is not valid semver".to_owned())
}

fn valid_asset_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !name.contains("..")
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(any(target_os = "macos", test))]
fn valid_job_id(value: &str) -> bool {
    (8..=96).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(target_os = "macos")]
fn binary_is_confined(paths: &RuntimePaths, binary: &Path, job_id: &str) -> bool {
    binary
        .parent()
        .is_some_and(|parent| parent == paths.config_dir.join("update").join("jobs").join(job_id))
}

#[cfg(target_os = "macos")]
fn ensure_real_dir(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("update state path is not a real directory".to_owned());
        }
    } else {
        fs::create_dir_all(path)
            .map_err(|error| format!("cannot create update state directory: {error}"))?;
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure update state directory: {error}"))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn cleanup_staging(binary: &Path) {
    let parent = binary.parent().map(Path::to_path_buf);
    let _ = fs::remove_file(binary);
    if let Some(parent) = parent {
        let _ = fs::remove_dir(parent);
    }
}

#[cfg(target_os = "macos")]
fn process_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(target_os = "macos")]
fn now_ms_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn public_job_view(job: &UpdateJobRecord) -> Value {
    json!({
        "job_id": job.job_id,
        "version": job.version,
        "target": job.target,
        "asset": job.asset_name,
        "sha256": job.sha256,
        "state": job.state,
        "detail": job.detail,
        "worker_pid": job.worker_pid,
        "created_at": job.created_at,
        "updated_at": job.updated_at,
    })
}

fn print_json(value: &Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|error| format!("cannot encode updater result: {error}"))?
    );
    Ok(())
}

fn error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_request() {
        "request"
    } else {
        "transport"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_for(target: &str, version: &str) -> Value {
        let identity = contract::identity().unwrap();
        let name = format!("herdr-mcp-{version}-{target}");
        let tag = format!("v{version}");
        json!({
            "schema_version": release_trust::MANIFEST_SCHEMA_VERSION,
            "product": "herdr-mcp",
            "state_schema": SCHEMA_VERSION,
            "version": version,
            "tag": tag,
            "release_identity": {
                "tag": tag,
                "source_commit": "b".repeat(40),
                "source_ref": format!("refs/tags/v{version}"),
            },
            "repository_identity": {
                "repository": release_trust::RELEASE_REPOSITORY,
                "repository_id": release_trust::RELEASE_REPOSITORY_ID,
            },
            "provenance": {
                "predicate_type": release_trust::SLSA_PROVENANCE_V1,
                "attestation": release_trust::GITHUB_ARTIFACT_ATTESTATION,
                "bundle_media_type": release_trust::SIGSTORE_BUNDLE_V03,
                "workflow": release_trust::RELEASE_WORKFLOW,
                "workflow_name": release_trust::RELEASE_WORKFLOW_NAME,
                "issuer": release_trust::RELEASE_ISSUER,
                "runner_environment": release_trust::RELEASE_RUNNER_ENVIRONMENT,
            },
            "contract": {
                "epoch": identity.epoch,
                "hash": identity.hash,
                "tool_count": identity.tool_count,
            },
            "assets": [{
                "target": target,
                "name": name,
                "size": 1234,
                "sha256": "a".repeat(64),
                "url": format!("https://github.com/whshang/herdr-mcp/releases/download/v{version}/herdr-mcp-{version}-{target}")
            }]
        })
    }

    #[test]
    fn manifest_validation_pins_contract_target_and_semver() {
        let target = current_target().unwrap();
        let plan = parse_release_plan(&manifest_for(target, "9.9.9"), target).unwrap();
        assert_eq!(plan.version, Version::parse("9.9.9").unwrap());
        assert_eq!(plan.asset.target, target);

        let mut bad = manifest_for(target, "9.9.9");
        bad["contract"]["hash"] = json!("sha256:wrong");
        assert!(
            parse_release_plan(&bad, target)
                .unwrap_err()
                .contains("contract")
        );
        let mut future_schema = manifest_for(target, "9.9.9");
        future_schema["state_schema"] = json!(SCHEMA_VERSION + 1);
        assert!(
            parse_release_plan(&future_schema, target)
                .unwrap_err()
                .contains("rollback-compatible")
        );
        let mut wrong_repo = manifest_for(target, "9.9.9");
        wrong_repo["repository_identity"]["repository"] = json!("attacker/fork");
        assert!(
            parse_release_plan(&wrong_repo, target)
                .unwrap_err()
                .contains("repository")
        );
        let mut wrong_url = manifest_for(target, "9.9.9");
        wrong_url["assets"][0]["url"] = json!("https://example.com/herdr-mcp");
        assert!(
            parse_release_plan(&wrong_url, target)
                .unwrap_err()
                .contains("trusted repository")
        );
        assert!(parse_release_plan(&manifest_for(target, "9.9.9"), "other-target").is_err());
    }

    #[test]
    fn release_discovery_includes_prereleases_and_fails_closed_on_invalid_entries() {
        let releases = json!([
            {
                "draft": false,
                "prerelease": true,
                "tag_name": "v0.4.0-alpha.5",
                "assets": [{"name": "release-manifest.json"}]
            },
            {
                "draft": false,
                "prerelease": true,
                "tag_name": "v0.4.0-alpha.6",
                "assets": [{"name": "release-manifest.json"}]
            },
            {
                "draft": true,
                "prerelease": true,
                "tag_name": "v9.0.0-alpha.1",
                "assets": [{"name": "release-manifest.json"}]
            },
            {
                "draft": false,
                "prerelease": false,
                "tag_name": "v8.0.0",
                "assets": [{"name": "other.bin"}]
            },
            {
                "draft": false,
                "prerelease": true,
                "tag_name": "not-semver",
                "assets": [{"name": "release-manifest.json"}]
            }
        ]);
        assert_eq!(select_release_tag(&releases).unwrap(), "v0.4.0-alpha.6");

        assert!(select_release_tag(&json!({"tag_name": "v1.0.0"})).is_err());
        assert!(
            select_release_tag(&json!([{
                "draft": false,
                "tag_name": "v1.0.0",
                "assets": []
            }]))
            .unwrap_err()
            .contains("no non-draft semver release")
        );
    }

    #[test]
    fn update_urls_are_https_or_loopback_http_and_never_carry_credentials() {
        assert!(parse_update_url("https://github.com/a/b").is_ok());
        assert!(parse_update_url("http://127.0.0.1:9000/manifest.json").is_ok());
        assert!(parse_update_url("http://localhost:9000/manifest.json").is_ok());
        assert!(parse_update_url("http://example.com/manifest.json").is_err());
        assert!(parse_update_url("https://user:pass@example.com/manifest.json").is_err());
        assert!(parse_update_url("file:///tmp/manifest.json").is_err());
        assert!(update_redirect_allowed(
            &Url::parse("https://release-assets.githubusercontent.com/a/b").unwrap()
        ));
        assert!(update_redirect_allowed(
            &Url::parse("https://objects.githubusercontent.com/a/b").unwrap()
        ));
        assert!(update_redirect_allowed(
            &Url::parse("http://127.0.0.1:9000/artifact").unwrap()
        ));
        assert!(!update_redirect_allowed(
            &Url::parse("https://example.com/artifact").unwrap()
        ));
    }

    #[test]
    fn asset_and_job_identifiers_are_path_safe() {
        assert!(valid_asset_name("herdr-mcp-1.0.0-aarch64-apple-darwin"));
        assert!(!valid_asset_name("../herdr-mcp"));
        assert!(!valid_asset_name("dir/herdr-mcp"));
        assert!(valid_sha256(&"f".repeat(64)));
        assert!(!valid_sha256(&"F".repeat(64)));
        assert!(!valid_sha256("not-a-hash"));
        assert!(valid_job_id("upd-12345678-1234-abcdef12"));
        assert!(!valid_job_id("../bad"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn active_update_recovery_blocks_live_worker_and_reaps_stale_queue() {
        let root = env::temp_dir().join(format!(
            "herdr-updater-active-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        let paths = RuntimePaths {
            config_dir: root.clone(),
            config_file: root.join("config.toml"),
            dev_state_dir: root.join("dev"),
            herdr_socket: None,
        };
        fs::create_dir_all(root.join("update/jobs/upd-live-12345678")).unwrap();
        let live_binary = root.join("update/jobs/upd-live-12345678/herdr-mcp-candidate");
        fs::write(&live_binary, b"live").unwrap();
        let store = UpdateStore::open(&paths).unwrap();
        store
            .create_update_job(&UpdateJobRecord {
                job_id: "upd-live-12345678".to_owned(),
                version: "9.9.9".to_owned(),
                target: current_target().unwrap().to_owned(),
                asset_name: "candidate".to_owned(),
                sha256: "a".repeat(64),
                binary_path: live_binary.to_string_lossy().into_owned(),
                state: "queued".to_owned(),
                detail: None,
                worker_pid: Some(std::process::id()),
                created_at: now_ms_i64(),
                updated_at: now_ms_i64(),
            })
            .unwrap();
        assert!(recover_or_reject_active_update(&store, &paths).is_err());
        assert!(live_binary.exists());

        store
            .update_update_job(
                "upd-live-12345678",
                "failed",
                Some("test transition"),
                None,
                now_ms_i64(),
            )
            .unwrap();
        fs::create_dir_all(root.join("update/jobs/upd-stale-12345678")).unwrap();
        let stale_binary = root.join("update/jobs/upd-stale-12345678/herdr-mcp-candidate");
        fs::write(&stale_binary, b"stale").unwrap();
        store
            .create_update_job(&UpdateJobRecord {
                job_id: "upd-stale-12345678".to_owned(),
                version: "9.9.10".to_owned(),
                target: current_target().unwrap().to_owned(),
                asset_name: "candidate".to_owned(),
                sha256: "b".repeat(64),
                binary_path: stale_binary.to_string_lossy().into_owned(),
                state: "queued".to_owned(),
                detail: None,
                worker_pid: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        recover_or_reject_active_update(&store, &paths).unwrap();
        assert!(!stale_binary.exists());
        assert_eq!(
            store
                .update_job("upd-stale-12345678")
                .unwrap()
                .unwrap()
                .state,
            "failed"
        );
        let _ = fs::remove_dir_all(root);
    }
}
