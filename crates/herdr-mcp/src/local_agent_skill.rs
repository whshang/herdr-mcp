use crate::cli::AgentSkillCommand;
use crate::paths::RuntimePaths;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

const SKILL_NAME: &str = "herdr-mcp";
const MANAGED_BY: &str = "herdr-mcp";
const SOURCE_ROOT: &str = "assets/local-agent-skill/herdr-mcp";
const SOURCE_MANIFEST: &str = "assets/local-agent-skill/herdr-mcp/manifest.json";
const INSTALLED_MARKER: &str = ".herdr-mcp-managed.json";
const INSTALLED_MANIFEST: &str = ".herdr-mcp-source-manifest.json";
const MANIFEST_SCHEMA_VERSION: u64 = 1;
const MARKER_SCHEMA_VERSION: u64 = 1;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_SKILL_FILE_BYTES: usize = 512 * 1024;
const MAX_SKILL_TOTAL_BYTES: usize = 2 * 1024 * 1024;
const MAX_SKILL_FILES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceIdentity {
    runtime_version: String,
    runtime_channel: String,
    source_commit: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct SkillManifest {
    schema_version: u64,
    skill: String,
    files: Vec<SkillManifestFile>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct SkillManifestFile {
    path: String,
    sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct InstalledMarker {
    schema_version: u64,
    managed_by: String,
    runtime_version: String,
    runtime_channel: String,
    source_commit: String,
    manifest_sha256: String,
    installed_at_ms: i64,
}

#[derive(Debug)]
struct SkillBundle {
    manifest_bytes: Vec<u8>,
    manifest_sha256: String,
    manifest: SkillManifest,
    files: Vec<(String, Vec<u8>)>,
}

pub(crate) fn run(command: AgentSkillCommand) -> Result<ExitCode, String> {
    let paths = RuntimePaths::discover()?;
    match command {
        AgentSkillCommand::Status => {
            let status = status_json()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&status)
                    .map_err(|error| format!("cannot encode agent skill status: {error}"))?
            );
            Ok(
                if status.get("current").and_then(Value::as_bool) == Some(true) {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                },
            )
        }
        AgentSkillCommand::Sync => {
            if paths.instance.is_named() {
                return Err(
                    "agent-skill is user-global; sync it from the default Herdr-MCP instance"
                        .to_owned(),
                );
            }
            let result = sync_from_runtime()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result)
                    .map_err(|error| format!("cannot encode agent skill sync result: {error}"))?
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Refresh the user-global local-agent Skill after a successful default-instance
/// service install. Skill delivery is an additive developer surface, not part of
/// the runtime transaction: a network/source failure must never roll back an
/// otherwise healthy service upgrade. The prior complete Skill is retained.
pub(crate) fn sync_after_install_best_effort() {
    let Ok(paths) = RuntimePaths::discover() else {
        return;
    };
    if paths.instance.is_named() {
        return;
    }
    if let Err(error) = sync_from_runtime() {
        eprintln!("herdr-mcp: local agent Skill refresh skipped: {error}");
    }
}

pub(crate) fn status_line() -> String {
    status_summary().1
}

pub(crate) fn status_summary() -> (bool, String) {
    match status_json() {
        Ok(value) => {
            let current = value.get("current").and_then(Value::as_bool) == Some(true);
            let state = value
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let commit = value
                .get("installed_source_commit")
                .and_then(Value::as_str)
                .map(|value| &value[..value.len().min(12)])
                .unwrap_or("-");
            (current, format!("{state} source={commit}"))
        }
        Err(error) => (false, format!("unknown reason={}", one_line(&error))),
    }
}

pub(crate) fn status_json() -> Result<Value, String> {
    let target = skill_target_dir()?;
    let expected = current_source_identity();
    if !target.exists() {
        return Ok(json!({
            "ok": true,
            "current": false,
            "state": "missing",
            "path": target,
            "expected_source_commit": expected.as_ref().ok().map(|source| source.source_commit.as_str()),
            "expected_runtime_version": expected.as_ref().ok().map(|source| source.runtime_version.as_str()),
            "runtime_identity_error": expected.as_ref().err().map(|error| one_line(error)),
        }));
    }
    let marker = match read_marker(&target) {
        Ok(marker) => marker,
        Err(error) => {
            return Ok(json!({
                "ok": true,
                "current": false,
                "state": "unmanaged_or_corrupt",
                "path": target,
                "reason": error,
                "expected_source_commit": expected.as_ref().ok().map(|source| source.source_commit.as_str()),
                "runtime_identity_error": expected.as_ref().err().map(|error| one_line(error)),
            }));
        }
    };
    let manifest_bytes = match fs::read(target.join(INSTALLED_MANIFEST)) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Ok(json!({
                "ok": true,
                "current": false,
                "state": "corrupt",
                "path": target,
                "installed_source_commit": marker.source_commit,
                "reason": format!("installed manifest is unavailable: {error}"),
            }));
        }
    };
    let manifest_sha256 = sha256_bytes(&manifest_bytes);
    let manifest: SkillManifest = match serde_json::from_slice(&manifest_bytes) {
        Ok(value) => value,
        Err(error) => {
            return Ok(json!({
                "ok": true,
                "current": false,
                "state": "corrupt",
                "path": target,
                "installed_source_commit": marker.source_commit,
                "reason": format!("installed manifest is invalid JSON: {error}"),
            }));
        }
    };
    let integrity = validate_manifest(&manifest)
        .and_then(|_| verify_installed_files(&target, &manifest))
        .is_ok()
        && manifest_sha256 == marker.manifest_sha256;
    let runtime_identity_available = expected.is_ok();
    let source_matches = expected.as_ref().ok().is_some_and(|source| {
        source.source_commit == marker.source_commit
            && source.runtime_version == marker.runtime_version
            && source.runtime_channel == marker.runtime_channel
    });
    let (state, current) =
        classify_installed_skill(integrity, runtime_identity_available, source_matches);
    Ok(json!({
        "ok": true,
        "current": current,
        "state": state,
        "path": target,
        "installed_runtime_version": marker.runtime_version,
        "installed_runtime_channel": marker.runtime_channel,
        "installed_source_commit": marker.source_commit,
        "manifest_sha256": marker.manifest_sha256,
        "integrity_ok": integrity,
        "runtime_identity_available": runtime_identity_available,
        "runtime_identity_error": expected.as_ref().err().map(|error| one_line(error)),
        "expected_runtime_version": expected.as_ref().ok().map(|source| source.runtime_version.as_str()),
        "expected_runtime_channel": expected.as_ref().ok().map(|source| source.runtime_channel.as_str()),
        "expected_source_commit": expected.as_ref().ok().map(|source| source.source_commit.as_str()),
    }))
}

/// Classify an installed Skill without conflating content integrity with the
/// runtime source comparison. An unreadable runtime identity leaves the source
/// verdict unknowable, so it is reported as `runtime_unavailable` rather than
/// as content drift.
fn classify_installed_skill(
    integrity: bool,
    runtime_identity_available: bool,
    source_matches: bool,
) -> (&'static str, bool) {
    let current = integrity && runtime_identity_available && source_matches;
    let state = if !integrity {
        "corrupt"
    } else if !runtime_identity_available {
        "runtime_unavailable"
    } else if source_matches {
        "current"
    } else {
        "drift"
    };
    (state, current)
}

fn sync_from_runtime() -> Result<Value, String> {
    let source = current_source_identity()?;
    let target = skill_target_dir()?;
    let source_commit = source.source_commit.clone();
    let local_dev_repo = if source.runtime_channel == "dev" {
        crate::dev::local_agent_skill_source_repo(&source_commit)?
    } else {
        None
    };
    let (bundle, source_origin) = if let Some(repo) = local_dev_repo.as_ref() {
        (
            fetch_bundle(&source, |repo_path, max_bytes| {
                fetch_local_git_file(repo, &source_commit, repo_path, max_bytes)
            })?,
            "local_dev_git_object",
        )
    } else {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .user_agent(format!("herdr-mcp/{}", source.runtime_version))
            .build()
            .map_err(|error| format!("cannot create agent Skill fetch client: {error}"))?;
        (
            fetch_bundle(&source, |repo_path, max_bytes| {
                fetch_repo_file(&client, &source_commit, repo_path, max_bytes)
            })?,
            "release_repository",
        )
    };
    let outcome = install_bundle(&target, &source, &bundle)?;
    let mut result = json!({
        "ok": true,
        "action": outcome.action,
        "path": target,
        "runtime_version": source.runtime_version,
        "runtime_channel": source.runtime_channel,
        "source_commit": source.source_commit,
        "source_origin": source_origin,
        "manifest_sha256": bundle.manifest_sha256,
        "file_count": bundle.manifest.files.len(),
    });
    if !outcome.warnings.is_empty() {
        // Activation already succeeded; a leftover backup is reported as a
        // non-fatal warning and never turns an explicit sync into a failure.
        result["warnings"] = json!(outcome.warnings);
    }
    Ok(result)
}

fn current_source_identity() -> Result<SourceIdentity, String> {
    let paths = RuntimePaths::discover()?;
    let config = crate::config::Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| format!("cannot create local runtime identity client: {error}"))?;
    let response = client
        .get(format!("http://127.0.0.1:{}/health", config.runtime_port))
        .send()
        .map_err(|error| format!("cannot read active runtime identity: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "active runtime identity returned HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|content_length| content_length > MAX_MANIFEST_BYTES as u64)
    {
        return Err("active runtime identity exceeds the size limit".to_owned());
    }
    let bytes = response
        .bytes()
        .map_err(|error| format!("cannot read active runtime identity body: {error}"))?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err("active runtime identity exceeds the size limit".to_owned());
    }
    let health: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("active runtime identity is invalid JSON: {error}"))?;
    if health.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("active runtime health is not ready".to_owned());
    }
    let build = health
        .get("build")
        .and_then(Value::as_object)
        .ok_or_else(|| "active runtime health is missing build identity".to_owned())?;
    if build.get("source_dirty").and_then(Value::as_bool) == Some(true) {
        return Err(
            "active runtime source is dirty; refusing to label repository Skill content as an exact runtime version"
                .to_owned(),
        );
    }
    let source_commit = build
        .get("source_commit")
        .and_then(Value::as_str)
        .or_else(|| build.get("commit").and_then(Value::as_str))
        .filter(|value| valid_commit(value))
        .ok_or_else(|| {
            "active runtime has no exact repository source commit; cannot version the local agent Skill"
                .to_owned()
        })?;
    let runtime_version = health
        .get("version")
        .and_then(Value::as_str)
        .or_else(|| build.get("server_version").and_then(Value::as_str))
        .filter(|value| valid_identity_text(value, 128))
        .ok_or_else(|| "active runtime version identity is invalid".to_owned())?;
    let runtime_channel = health
        .get("channel")
        .and_then(Value::as_str)
        .or_else(|| build.get("channel").and_then(Value::as_str))
        .filter(|value| valid_identity_text(value, 32))
        .ok_or_else(|| "active runtime channel identity is invalid".to_owned())?;
    Ok(SourceIdentity {
        runtime_version: runtime_version.to_owned(),
        runtime_channel: runtime_channel.to_owned(),
        source_commit: source_commit.to_owned(),
    })
}

fn fetch_bundle<F>(source: &SourceIdentity, mut fetch: F) -> Result<SkillBundle, String>
where
    F: FnMut(&str, usize) -> Result<Vec<u8>, String>,
{
    if !valid_commit(&source.source_commit) {
        return Err("agent Skill source commit is invalid".to_owned());
    }
    let manifest_bytes = fetch(SOURCE_MANIFEST, MAX_MANIFEST_BYTES)?;
    if manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err("agent Skill manifest exceeds the size limit".to_owned());
    }
    let manifest_sha256 = sha256_bytes(&manifest_bytes);
    let manifest: SkillManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("agent Skill manifest is invalid JSON: {error}"))?;
    validate_manifest(&manifest)?;
    let mut total = 0usize;
    let mut files = Vec::with_capacity(manifest.files.len());
    for entry in &manifest.files {
        let repo_path = format!("{SOURCE_ROOT}/{}", entry.path);
        let bytes = fetch(&repo_path, MAX_SKILL_FILE_BYTES)?;
        if bytes.len() > MAX_SKILL_FILE_BYTES {
            return Err(format!(
                "agent Skill file {} exceeds the size limit",
                entry.path
            ));
        }
        total = total.saturating_add(bytes.len());
        if total > MAX_SKILL_TOTAL_BYTES {
            return Err("agent Skill bundle exceeds the total size limit".to_owned());
        }
        let observed = sha256_bytes(&bytes);
        if observed != entry.sha256 {
            return Err(format!(
                "agent Skill file digest mismatch for {}: expected {}, observed {}",
                entry.path, entry.sha256, observed
            ));
        }
        files.push((entry.path.clone(), bytes));
    }
    Ok(SkillBundle {
        manifest_bytes,
        manifest_sha256,
        manifest,
        files,
    })
}

fn fetch_local_git_file(
    repo: &Path,
    source_commit: &str,
    repo_path: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    if !valid_commit(source_commit) || !valid_repo_path(repo_path) {
        return Err("invalid local DEV agent Skill repository source identity".to_owned());
    }
    let object = format!("{source_commit}:{repo_path}");
    let size_output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "-s", object.as_str()])
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .output()
        .map_err(|error| {
            format!(
                "cannot inspect exact DEV agent Skill source {repo_path} in {}: {error}",
                repo.display()
            )
        })?;
    if !size_output.status.success() {
        return Err(format!(
            "exact DEV agent Skill source {repo_path} is unavailable at {source_commit} in {}",
            repo.display()
        ));
    }
    let size_text = String::from_utf8(size_output.stdout)
        .map_err(|_| format!("invalid Git size for DEV agent Skill source {repo_path}"))?;
    let size = size_text
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("invalid Git size for DEV agent Skill source {repo_path}"))?;
    if size > max_bytes {
        return Err(format!(
            "DEV agent Skill source {repo_path} exceeds the size limit"
        ));
    }

    let content_output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "blob", object.as_str()])
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .output()
        .map_err(|error| {
            format!(
                "cannot read exact DEV agent Skill source {repo_path} in {}: {error}",
                repo.display()
            )
        })?;
    if !content_output.status.success() {
        return Err(format!(
            "cannot read exact DEV agent Skill source {repo_path} at {source_commit} in {}",
            repo.display()
        ));
    }
    if content_output.stdout.len() != size || content_output.stdout.len() > max_bytes {
        return Err(format!(
            "DEV agent Skill source {repo_path} changed or exceeded the size limit while reading"
        ));
    }
    Ok(content_output.stdout)
}

fn fetch_repo_file(
    client: &Client,
    source_commit: &str,
    repo_path: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let url = raw_repo_url(source_commit, repo_path)?;
    let response = client
        .get(url.clone())
        .send()
        .map_err(|error| format!("cannot fetch agent Skill source {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "agent Skill source fetch returned HTTP {} for {repo_path}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|content_length| content_length > max_bytes as u64)
    {
        return Err(format!(
            "agent Skill source {repo_path} exceeds the size limit"
        ));
    }
    let bytes = response
        .bytes()
        .map_err(|error| format!("cannot read agent Skill source {repo_path}: {error}"))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "agent Skill source {repo_path} exceeds the size limit"
        ));
    }
    Ok(bytes.to_vec())
}

fn raw_repo_url(source_commit: &str, repo_path: &str) -> Result<Url, String> {
    if !valid_commit(source_commit) || !valid_repo_path(repo_path) {
        return Err("invalid agent Skill repository source identity".to_owned());
    }
    Url::parse(&format!(
        "https://raw.githubusercontent.com/{}/{source_commit}/{repo_path}",
        crate::release_trust::RELEASE_REPOSITORY
    ))
    .map_err(|error| format!("cannot construct agent Skill source URL: {error}"))
}

fn validate_manifest(manifest: &SkillManifest) -> Result<(), String> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION || manifest.skill != SKILL_NAME {
        return Err("agent Skill manifest identity mismatch".to_owned());
    }
    if manifest.files.is_empty() || manifest.files.len() > MAX_SKILL_FILES {
        return Err("agent Skill manifest file count is invalid".to_owned());
    }
    let mut seen = BTreeSet::new();
    let mut has_entrypoint = false;
    for entry in &manifest.files {
        if !valid_skill_relative_path(&entry.path) || !valid_sha256(&entry.sha256) {
            return Err(format!(
                "agent Skill manifest entry is invalid: {}",
                entry.path
            ));
        }
        if !seen.insert(entry.path.as_str()) {
            return Err(format!(
                "agent Skill manifest contains duplicate path: {}",
                entry.path
            ));
        }
        has_entrypoint |= entry.path == "SKILL.md";
    }
    if !has_entrypoint {
        return Err("agent Skill manifest is missing SKILL.md".to_owned());
    }
    Ok(())
}

/// Outcome of refreshing the installed Skill. Activation is reported as
/// success even when a post-activation cleanup step fails: the new content is
/// already live, so a leftover backup is a non-fatal warning rather than a
/// failed sync.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallOutcome {
    action: &'static str,
    warnings: Vec<String>,
}

fn install_bundle(
    target: &Path,
    source: &SourceIdentity,
    bundle: &SkillBundle,
) -> Result<InstallOutcome, String> {
    install_bundle_with(target, source, bundle, |backup: &Path| {
        fs::remove_dir_all(backup)
    })
}

/// `install_bundle` with the post-activation backup cleanup injectable so a
/// failing cleanup can be exercised deterministically in tests.
fn install_bundle_with<F>(
    target: &Path,
    source: &SourceIdentity,
    bundle: &SkillBundle,
    remove_backup: F,
) -> Result<InstallOutcome, String>
where
    F: FnOnce(&Path) -> std::io::Result<()>,
{
    let prior_marker = managed_marker_if_present(target)?;
    if let Some(marker) = prior_marker.as_ref()
        && marker.source_commit == source.source_commit
        && marker.runtime_version == source.runtime_version
        && marker.runtime_channel == source.runtime_channel
        && marker.manifest_sha256 == bundle.manifest_sha256
        && verify_installed_files(target, &bundle.manifest).is_ok()
    {
        return Ok(InstallOutcome {
            action: "unchanged",
            warnings: Vec::new(),
        });
    }
    let parent = target
        .parent()
        .ok_or_else(|| "agent Skill target has no parent directory".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "cannot create agent Skill parent directory {}: {error}",
            parent.display()
        )
    })?;
    let nonce = format!("{}-{}", std::process::id(), now_ms());
    let staging = parent.join(format!(".{SKILL_NAME}.staging-{nonce}"));
    let backup = parent.join(format!(".{SKILL_NAME}.backup-{nonce}"));
    if staging.exists() || backup.exists() {
        return Err("agent Skill staging path collision".to_owned());
    }
    let staged = stage_bundle(&staging, source, bundle);
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    let current_marker = managed_marker_if_present(target)?;
    if current_marker != prior_marker {
        let _ = fs::remove_dir_all(&staging);
        return Err(
            "agent Skill target changed while the refresh was staged; refusing to replace it"
                .to_owned(),
        );
    }
    if prior_marker.is_some() {
        fs::rename(target, &backup).map_err(|error| {
            let _ = fs::remove_dir_all(&staging);
            format!("cannot preserve prior agent Skill before refresh: {error}")
        })?;
    }
    if let Err(error) = fs::rename(&staging, target) {
        if backup.exists() {
            let _ = fs::rename(&backup, target);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(format!(
            "cannot activate refreshed agent Skill; prior version restored when possible: {error}"
        ));
    }
    let mut warnings = Vec::new();
    if backup.exists()
        && let Err(error) = remove_backup(&backup)
    {
        let warning = format!(
            "agent Skill refresh succeeded but the previous version backup {} could not be removed: {error}",
            backup.display()
        );
        eprintln!("herdr-mcp: {warning}");
        warnings.push(warning);
    }
    Ok(InstallOutcome {
        action: "updated",
        warnings,
    })
}

fn stage_bundle(
    staging: &Path,
    source: &SourceIdentity,
    bundle: &SkillBundle,
) -> Result<(), String> {
    fs::create_dir(staging).map_err(|error| {
        format!(
            "cannot create agent Skill staging directory {}: {error}",
            staging.display()
        )
    })?;
    for (relative, bytes) in &bundle.files {
        let destination = staging.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "cannot create agent Skill staging directory {}: {error}",
                    parent.display()
                )
            })?;
        }
        fs::write(&destination, bytes).map_err(|error| {
            format!(
                "cannot write staged agent Skill file {}: {error}",
                destination.display()
            )
        })?;
    }
    fs::write(staging.join(INSTALLED_MANIFEST), &bundle.manifest_bytes)
        .map_err(|error| format!("cannot write staged agent Skill source manifest: {error}"))?;
    let marker = InstalledMarker {
        schema_version: MARKER_SCHEMA_VERSION,
        managed_by: MANAGED_BY.to_owned(),
        runtime_version: source.runtime_version.clone(),
        runtime_channel: source.runtime_channel.clone(),
        source_commit: source.source_commit.clone(),
        manifest_sha256: bundle.manifest_sha256.clone(),
        installed_at_ms: now_ms(),
    };
    let marker_bytes = serde_json::to_vec_pretty(&marker)
        .map_err(|error| format!("cannot encode agent Skill ownership marker: {error}"))?;
    fs::write(staging.join(INSTALLED_MARKER), marker_bytes)
        .map_err(|error| format!("cannot write agent Skill ownership marker: {error}"))?;
    verify_installed_files(staging, &bundle.manifest)?;
    Ok(())
}

fn read_marker(target: &Path) -> Result<InstalledMarker, String> {
    let path = target.join(INSTALLED_MARKER);
    let bytes = fs::read(&path).map_err(|_| {
        format!(
            "refusing to replace existing {} because it is not marked as Herdr-MCP managed",
            target.display()
        )
    })?;
    let marker: InstalledMarker = serde_json::from_slice(&bytes)
        .map_err(|_| "agent Skill ownership marker is invalid".to_owned())?;
    if marker.schema_version != MARKER_SCHEMA_VERSION || marker.managed_by != MANAGED_BY {
        return Err("agent Skill ownership marker does not belong to Herdr-MCP".to_owned());
    }
    if !valid_commit(&marker.source_commit) || !valid_sha256(&marker.manifest_sha256) {
        return Err("agent Skill ownership marker is corrupt".to_owned());
    }
    Ok(marker)
}

fn managed_marker_if_present(target: &Path) -> Result<Option<InstalledMarker>, String> {
    match fs::symlink_metadata(target) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                return Err(format!(
                    "refusing to replace existing {} because the Skill target is not a managed directory",
                    target.display()
                ));
            }
            read_marker(target).map(Some)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot inspect agent Skill target {}: {error}",
            target.display()
        )),
    }
}

fn verify_installed_files(target: &Path, manifest: &SkillManifest) -> Result<(), String> {
    for entry in &manifest.files {
        let path = target.join(&entry.path);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("agent Skill file {} is unavailable: {error}", entry.path))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(format!(
                "agent Skill file {} is not a regular file",
                entry.path
            ));
        }
        if metadata.len() > MAX_SKILL_FILE_BYTES as u64 {
            return Err(format!(
                "agent Skill file {} exceeds the size limit",
                entry.path
            ));
        }
        let bytes = fs::read(&path)
            .map_err(|error| format!("cannot read agent Skill file {}: {error}", entry.path))?;
        if sha256_bytes(&bytes) != entry.sha256 {
            return Err(format!(
                "agent Skill file {} failed integrity check",
                entry.path
            ));
        }
    }
    Ok(())
}

fn skill_target_dir() -> Result<PathBuf, String> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "cannot determine user home directory for agent Skill".to_owned())?;
    Ok(home.join(".agents/skills").join(SKILL_NAME))
}

fn valid_skill_relative_path(value: &str) -> bool {
    if value == "SKILL.md" {
        return true;
    }
    if !value.starts_with("references/") || !value.ends_with(".md") {
        return false;
    }
    valid_repo_path(value)
}

fn valid_repo_path(value: &str) -> bool {
    if value.is_empty() || value.len() > 512 || value.contains('\\') {
        return false;
    }
    let path = Path::new(value);
    !path.is_absolute()
        && path.components().all(|component| match component {
            Component::Normal(part) => !part.is_empty(),
            _ => false,
        })
}

fn valid_identity_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value == value.trim()
        && !value.chars().any(char::is_control)
}

fn valid_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> SourceIdentity {
        SourceIdentity {
            runtime_version: "1.0.0-test".to_owned(),
            runtime_channel: "dev".to_owned(),
            source_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        }
    }

    fn test_dir(label: &str) -> PathBuf {
        let root = env::temp_dir().join(format!(
            "herdr-mcp-agent-skill-{label}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn bundle(files: &[(&str, &[u8])]) -> SkillBundle {
        let manifest = SkillManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            skill: SKILL_NAME.to_owned(),
            files: files
                .iter()
                .map(|(path, bytes)| SkillManifestFile {
                    path: (*path).to_owned(),
                    sha256: sha256_bytes(bytes),
                })
                .collect(),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        SkillBundle {
            manifest_sha256: sha256_bytes(&manifest_bytes),
            manifest_bytes,
            manifest: manifest.clone(),
            files: files
                .iter()
                .map(|(path, bytes)| ((*path).to_owned(), (*bytes).to_vec()))
                .collect(),
        }
    }

    fn run_git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn local_dev_fetch_reads_exact_commit_objects_not_the_working_tree() {
        let root = test_dir("local-dev-git");
        let init = Command::new("git")
            .args(["init", "--quiet", "--object-format=sha1"])
            .arg(&root)
            .status()
            .unwrap();
        assert!(init.success());
        run_git(&root, &["config", "user.name", "Herdr Test"]);
        run_git(
            &root,
            &["config", "user.email", "herdr-test@example.invalid"],
        );

        let tracked = root.join(SOURCE_ROOT);
        fs::create_dir_all(&tracked).unwrap();
        let committed = bundle(&[("SKILL.md", b"committed-skill")]);
        fs::write(tracked.join("manifest.json"), &committed.manifest_bytes).unwrap();
        fs::write(tracked.join("SKILL.md"), b"committed-skill").unwrap();
        run_git(&root, &["add", "assets/local-agent-skill/herdr-mcp"]);
        run_git(&root, &["commit", "--quiet", "-m", "test skill"]);
        let commit = run_git(&root, &["rev-parse", "HEAD"]);
        assert!(valid_commit(&commit));

        // A later working-tree edit must never be mislabeled as the Skill for
        // the already-running DEV commit.
        fs::write(tracked.join("SKILL.md"), b"working-tree-skill").unwrap();
        let source = SourceIdentity {
            runtime_version: "1.0.0-test".to_owned(),
            runtime_channel: "dev".to_owned(),
            source_commit: commit.clone(),
        };
        let fetched = fetch_bundle(&source, |repo_path, max_bytes| {
            fetch_local_git_file(&root, &commit, repo_path, max_bytes)
        })
        .unwrap();
        assert_eq!(fetched.files[0].1, b"committed-skill");
        assert_ne!(
            fetched.files[0].1,
            fs::read(tracked.join("SKILL.md")).unwrap()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn install_bundle_is_atomic_from_the_callers_view_and_preserves_foreign_target() {
        let root = test_dir("atomic");
        let target = root.join(SKILL_NAME);
        let first = bundle(&[("SKILL.md", b"first"), ("references/memory.md", b"m1")]);
        assert_eq!(
            install_bundle(&target, &source(), &first).unwrap().action,
            "updated"
        );
        assert_eq!(fs::read(target.join("SKILL.md")).unwrap(), b"first");

        let mut second_source = source();
        second_source.runtime_version = "1.0.1-test".to_owned();
        let second = bundle(&[("SKILL.md", b"second"), ("references/memory.md", b"m2")]);
        assert_eq!(
            install_bundle(&target, &second_source, &second)
                .unwrap()
                .action,
            "updated"
        );
        assert_eq!(fs::read(target.join("SKILL.md")).unwrap(), b"second");
        assert!(verify_installed_files(&target, &second.manifest).is_ok());

        let foreign = root.join("foreign/herdr-mcp");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("SKILL.md"), b"user-owned").unwrap();
        let error = install_bundle(&foreign, &source(), &first).unwrap_err();
        assert!(error.contains("not marked as Herdr-MCP managed"));
        assert_eq!(fs::read(foreign.join("SKILL.md")).unwrap(), b"user-owned");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fetch_failure_and_digest_mismatch_do_not_touch_the_installed_skill() {
        let root = test_dir("fetch-failure");
        let target = root.join(SKILL_NAME);
        let current = bundle(&[("SKILL.md", b"current")]);
        install_bundle(&target, &source(), &current).unwrap();

        let manifest = SkillManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            skill: SKILL_NAME.to_owned(),
            files: vec![SkillManifestFile {
                path: "SKILL.md".to_owned(),
                sha256: sha256_bytes(b"expected"),
            }],
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let result = fetch_bundle(&source(), |path, _| {
            if path == SOURCE_MANIFEST {
                Ok(manifest_bytes.clone())
            } else {
                Ok(b"wrong".to_vec())
            }
        });
        assert!(result.unwrap_err().contains("digest mismatch"));
        assert_eq!(fs::read(target.join("SKILL.md")).unwrap(), b"current");

        let tracked_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/local-agent-skill/herdr-mcp");
        let tracked_manifest_bytes = fs::read(tracked_root.join("manifest.json")).unwrap();
        let tracked_manifest: SkillManifest =
            serde_json::from_slice(&tracked_manifest_bytes).unwrap();
        validate_manifest(&tracked_manifest).unwrap();
        verify_installed_files(&tracked_root, &tracked_manifest).unwrap();

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn install_bundle_reports_a_failed_backup_cleanup_as_a_non_fatal_warning() {
        let root = test_dir("cleanup-warning");
        let target = root.join(SKILL_NAME);
        let first = bundle(&[("SKILL.md", b"first")]);
        install_bundle(&target, &source(), &first).unwrap();

        let mut second_source = source();
        second_source.runtime_version = "1.0.1-test".to_owned();
        let second = bundle(&[("SKILL.md", b"second")]);
        let outcome = install_bundle_with(&target, &second_source, &second, |_| {
            Err(std::io::Error::other("simulated cleanup failure"))
        })
        .unwrap();

        // Activation already replaced the live Skill, so the refresh must stay
        // a success and surface the cleanup problem as a non-fatal warning.
        assert_eq!(outcome.action, "updated");
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].contains("could not be removed"));
        assert_eq!(fs::read(target.join("SKILL.md")).unwrap(), b"second");
        assert!(verify_installed_files(&target, &second.manifest).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_distinguishes_unavailable_runtime_identity_from_drift() {
        // An intact Skill whose runtime identity cannot be read is not drift:
        // the source comparison never happened, so the verdict stays unknown.
        assert_eq!(
            classify_installed_skill(true, false, false),
            ("runtime_unavailable", false)
        );
        // Drift requires a readable runtime identity that disagrees.
        assert_eq!(
            classify_installed_skill(true, true, false),
            ("drift", false)
        );
        assert_eq!(
            classify_installed_skill(true, true, true),
            ("current", true)
        );
        // Broken content is corrupt regardless of the runtime verdict.
        assert_eq!(
            classify_installed_skill(false, false, false),
            ("corrupt", false)
        );
        assert_eq!(
            classify_installed_skill(false, true, true),
            ("corrupt", false)
        );
    }
}
