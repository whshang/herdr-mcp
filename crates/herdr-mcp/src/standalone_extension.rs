use reqwest::blocking::{Client, Response};
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

const REPOSITORY: &str = "whshang/herdr-mcp";
const API_HOST: &str = "api.github.com";
const RAW_HOST: &str = "raw.githubusercontent.com";
const MAX_FILES: usize = 512;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandaloneInstallOptions {
    pub reference: Option<String>,
    pub load_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CommitResponse {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct TreeResponse {
    truncated: bool,
    tree: Vec<TreeEntry>,
}

#[derive(Debug, Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    size: Option<u64>,
    sha: String,
}

#[derive(Debug)]
struct Blob {
    repo_path: String,
    relative_path: PathBuf,
    size: u64,
    git_sha: String,
}

#[derive(Debug)]
struct PreparedManifest {
    version: String,
    extension_id: String,
    source_sha256: String,
}

pub fn run_install(options: StandaloneInstallOptions) -> Result<ExitCode, String> {
    let view = install(options)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&view).map_err(|e| e.to_string())?
    );
    Ok(ExitCode::SUCCESS)
}

pub fn run_status() -> Result<ExitCode, String> {
    let home = home_dir()?;
    let native_host_status = crate::native_host_install::doctor_status().unwrap_or_else(|error| {
        json!({
            "ok": false,
            "error": error,
        })
    });
    let view = status_view(&home, native_host_status)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&view).map_err(|e| e.to_string())?
    );
    Ok(ExitCode::SUCCESS)
}

fn status_view(home: &Path, native_host_status: Value) -> Result<Value, String> {
    let base = base_dir_for(home);
    let current = base.join("current");
    let identity = crate::browser_extension_identity::official_standalone_identity()?;
    let manifest = read_json(&current.join("manifest.json")).ok();
    let key_ok = manifest
        .as_ref()
        .and_then(|value| value.get("key"))
        .and_then(Value::as_str)
        .is_some_and(|key| identity.manifest_key.as_deref() == Some(key));
    let state = fs::read_to_string(base.join("state.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let installed = current.is_dir() && manifest.is_some() && key_ok;
    let configured_load_path = state
        .as_ref()
        .and_then(|value| value.get("load_unpacked_path"))
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let user_visible_path = configured_load_path
        .as_deref()
        .filter(|path| *path != current)
        .map(|path| inspect_alias(path, &current))
        .unwrap_or_else(|| inspect_user_visible_alias(home, &current));
    let load_unpacked_path = preferred_load_unpacked_path(&user_visible_path, &current);
    let native_host_configured = native_host_status
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let native_host_channel = native_host_status.get("active_channel").cloned();
    let standalone_origin_match = native_host_status
        .get("standalone_origin_match")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(json!({
        "ok": true,
        "installed": installed,
        "installed_semantics": "managed_installer_copy_valid",
        "path": current,
        "user_visible_path": user_visible_path,
        "managed_install": {
            "valid": installed,
            "path": current,
            "manifest_present": manifest.is_some(),
            "manifest_key_matches": key_ok,
            "state": state,
        },
        "browser_load": {
            "load_unpacked_path": load_unpacked_path,
            "loaded_observed": Value::Null,
            "observation": "browser_loaded_state_not_tracked_by_standalone_status",
        },
        "native_host": {
            "configured": native_host_configured,
            "active_channel": native_host_channel,
            "standalone_origin_match": standalone_origin_match,
            "connected_observed": Value::Null,
            "connection_observation": "active_native_host_connection_not_tracked_by_standalone_status",
            "status": native_host_status,
        },
        "chrome": {
            "url": "chrome://extensions",
            "action": "Developer mode -> Load unpacked",
            "load_unpacked_path": load_unpacked_path,
        },
        "extension_id": identity.extension_id,
        "manifest_key_matches": key_ok,
        "state": state,
    }))
}

fn install(options: StandaloneInstallOptions) -> Result<Value, String> {
    let requested_ref = options.reference.unwrap_or_else(default_source_ref);
    validate_ref(&requested_ref)?;
    let home = home_dir()?;
    let base = base_dir_for(&home);
    let current = base.join("current");
    let explicit_load_path = options
        .load_path
        .as_deref()
        .map(|value| resolve_load_path(&home, value))
        .transpose()?;
    if let Some(path) = explicit_load_path.as_deref() {
        preflight_explicit_load_path(path, &current)?;
    }

    let client = client()?;
    let commit = resolve_ref(&client, &requested_ref)?;
    let blobs = list_blobs(&client, &commit)?;

    fs::create_dir_all(&base).map_err(|e| format!("cannot create standalone directory: {e}"))?;
    let staging = base.join(format!(".staging-{}-{}", std::process::id(), now_ms()));
    fs::create_dir_all(&staging).map_err(|e| format!("cannot create staging directory: {e}"))?;

    let prepared = (|| -> Result<PreparedManifest, String> {
        for blob in &blobs {
            let bytes = download_blob(&client, &commit, blob)?;
            let target = staging.join(&blob.relative_path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create extension directory: {e}"))?;
            }
            fs::write(&target, bytes).map_err(|e| format!("cannot write extension file: {e}"))?;
        }
        prepare_manifest(&staging)
    })();
    let prepared = match prepared {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };

    let backup = base.join(format!(".previous-{}-{}", std::process::id(), now_ms()));
    let had_current = current.exists();
    if had_current {
        fs::rename(&current, &backup)
            .map_err(|e| format!("cannot stage previous standalone extension: {e}"))?;
    }
    if let Err(error) = fs::rename(&staging, &current) {
        if had_current {
            let _ = fs::rename(&backup, &current);
        }
        return Err(format!("cannot activate standalone extension: {error}"));
    }

    let user_visible_path = match explicit_load_path.as_deref() {
        Some(path) if path == current => json!({
            "ready": true,
            "status": "managed",
            "path": current,
            "target": current,
        }),
        Some(path) => match ensure_explicit_alias(&home, path, &current) {
            Ok(value) => value,
            Err(error) => {
                rollback_activation(&current, &backup, had_current);
                return Err(error);
            }
        },
        None => ensure_user_visible_alias(&home, &current),
    };
    let load_unpacked_path = preferred_load_unpacked_path(&user_visible_path, &current);

    let state = json!({
        "schema_version": 1,
        "repository": REPOSITORY,
        "requested_ref": requested_ref,
        "source_commit": commit,
        "extension_version": prepared.version,
        "extension_id": prepared.extension_id,
        "source_manifest_sha256": prepared.source_sha256,
        "installed_at_unix_ms": now_ms(),
        "path": current,
        "load_unpacked_path": load_unpacked_path,
    });
    if let Err(error) = atomic_json(&base.join("state.json"), &state) {
        remove_alias_if_created(&user_visible_path, &current);
        rollback_activation(&current, &backup, had_current);
        return Err(error);
    }
    if backup.exists() {
        let _ = fs::remove_dir_all(&backup);
    }
    let native_host = crate::native_host_install::activate_standalone().map_err(|error| {
        format!(
            "standalone extension files were installed, but automatic Native Host activation failed: {error}; rerun `herdr-mcp extension standalone install` after repairing Native Host state"
        )
    })?;
    Ok(json!({
        "ok": true,
        "channel": "standalone",
        "repository": REPOSITORY,
        "requested_ref": state["requested_ref"],
        "source_commit": state["source_commit"],
        "extension_version": state["extension_version"],
        "extension_id": state["extension_id"],
        "path": current,
        "user_visible_path": user_visible_path,
        "chrome": {
            "url": "chrome://extensions",
            "action": "Developer mode -> Load unpacked",
            "load_unpacked_path": load_unpacked_path,
        },
        "native_host": native_host,
        "note": "All Git-tracked extension/ files come from the pinned commit; manifest.json differs only by the injected standalone public key.",
    }))
}

fn prepare_manifest(root: &Path) -> Result<PreparedManifest, String> {
    let path = root.join("manifest.json");
    let source = fs::read(&path).map_err(|e| format!("downloaded manifest missing: {e}"))?;
    let source_sha256 = sha256(&source);
    let manifest: Value = serde_json::from_slice(&source)
        .map_err(|e| format!("downloaded manifest is invalid JSON: {e}"))?;
    let object = manifest
        .as_object()
        .ok_or_else(|| "manifest must be a JSON object".to_owned())?;
    if object.get("manifest_version").and_then(Value::as_u64) != Some(3) {
        return Err("standalone extension requires Manifest V3".to_owned());
    }
    let version = object
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "manifest has no version".to_owned())?
        .to_owned();
    let identity = crate::browser_extension_identity::official_standalone_identity()?;
    let key = identity
        .manifest_key
        .as_deref()
        .ok_or_else(|| "standalone identity has no manifest key".to_owned())?;
    let bytes = match object.get("key").and_then(Value::as_str) {
        Some(existing) if existing != key => {
            return Err("manifest contains a conflicting key".to_owned());
        }
        Some(_) => source.clone(),
        None => inject_key(&source, key)?,
    };
    fs::write(&path, bytes).map_err(|e| format!("cannot write standalone manifest: {e}"))?;
    let verify = read_json(&path)?;
    if verify.get("key").and_then(Value::as_str) != Some(key) {
        return Err("standalone manifest key verification failed".to_owned());
    }
    Ok(PreparedManifest {
        version,
        extension_id: identity.extension_id,
        source_sha256,
    })
}

fn inject_key(source: &[u8], key: &str) -> Result<Vec<u8>, String> {
    let closing = source
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .filter(|index| source[*index] == b'}')
        .ok_or_else(|| "manifest has no closing object brace".to_owned())?;
    let last = source[..closing]
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .ok_or_else(|| "manifest object is empty".to_owned())?;
    if matches!(source[last], b'{' | b',') {
        return Err("manifest has no final property".to_owned());
    }
    let key_json = serde_json::to_string(key).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(source.len() + key_json.len() + 16);
    out.extend_from_slice(&source[..=last]);
    out.push(b',');
    out.extend_from_slice(&source[last + 1..closing]);
    out.extend_from_slice(b"  \"key\": ");
    out.extend_from_slice(key_json.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(&source[closing..]);
    Ok(out)
}

fn list_blobs(client: &Client, commit: &str) -> Result<Vec<Blob>, String> {
    let url = format!("https://{API_HOST}/repos/whshang/herdr-mcp/git/trees/{commit}?recursive=1");
    let tree: TreeResponse = get(client, &url, API_HOST)?
        .json()
        .map_err(|e| format!("cannot decode GitHub tree: {e}"))?;
    if tree.truncated {
        return Err("GitHub tree was truncated; refusing partial extension download".to_owned());
    }
    let mut out = Vec::new();
    let mut total = 0_u64;
    for entry in tree.tree {
        if entry.kind != "blob" || !entry.path.starts_with("extension/") {
            continue;
        }
        let rel = entry.path.trim_start_matches("extension/");
        if rel.is_empty() {
            continue;
        }
        let relative_path = safe_rel(rel)?;
        let size = entry.size.unwrap_or(0);
        if !is_sha(&entry.sha) {
            return Err(format!(
                "extension tree returned invalid Git blob SHA: {}",
                entry.path
            ));
        }
        if size > MAX_FILE_BYTES {
            return Err(format!("extension file too large: {}", entry.path));
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| "extension size overflow".to_owned())?;
        if total > MAX_TOTAL_BYTES {
            return Err("extension tree exceeds size limit".to_owned());
        }
        out.push(Blob {
            repo_path: entry.path,
            relative_path,
            size,
            git_sha: entry.sha.to_ascii_lowercase(),
        });
        if out.len() > MAX_FILES {
            return Err("extension tree exceeds file-count limit".to_owned());
        }
    }
    out.sort_by(|a, b| a.repo_path.cmp(&b.repo_path));
    if !out
        .iter()
        .any(|blob| blob.repo_path == "extension/manifest.json")
    {
        return Err("extension tree has no manifest.json".to_owned());
    }
    Ok(out)
}

fn download_blob(client: &Client, commit: &str, blob: &Blob) -> Result<Vec<u8>, String> {
    let url = format!(
        "https://{RAW_HOST}/whshang/herdr-mcp/{commit}/{}",
        blob.repo_path
    );
    let bytes = get(client, &url, RAW_HOST)?
        .bytes()
        .map_err(|e| format!("cannot read {}: {e}", blob.repo_path))?;
    if bytes.len() as u64 > MAX_FILE_BYTES || (blob.size != 0 && bytes.len() as u64 != blob.size) {
        return Err(format!("downloaded file size mismatch: {}", blob.repo_path));
    }
    let actual_git_sha = git_blob_sha(&bytes);
    if actual_git_sha != blob.git_sha {
        return Err(format!(
            "downloaded file Git blob SHA mismatch: {}",
            blob.repo_path
        ));
    }
    Ok(bytes.to_vec())
}

fn git_blob_sha(bytes: &[u8]) -> String {
    let mut digest = Sha1::new();
    digest.update(format!("blob {}\0", bytes.len()).as_bytes());
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn resolve_ref(client: &Client, reference: &str) -> Result<String, String> {
    if is_sha(reference) {
        return Ok(reference.to_ascii_lowercase());
    }
    let mut url = Url::parse(&format!(
        "https://{API_HOST}/repos/whshang/herdr-mcp/commits/"
    ))
    .map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|_| "cannot construct GitHub commit URL".to_owned())?
        .push(reference);
    let commit: CommitResponse = get(client, url.as_str(), API_HOST)?
        .json()
        .map_err(|e| format!("cannot decode GitHub commit response: {e}"))?;
    if !is_sha(&commit.sha) {
        return Err("GitHub returned invalid commit SHA".to_owned());
    }
    Ok(commit.sha.to_ascii_lowercase())
}

fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent(concat!("herdr-mcp/", env!("CARGO_PKG_VERSION")))
        .redirect(Policy::limited(3))
        .build()
        .map_err(|e| format!("cannot create GitHub client: {e}"))
}

fn get(client: &Client, url: &str, expected_host: &str) -> Result<Response, String> {
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("GitHub request failed: {e}"))?;
    if response.url().host_str() != Some(expected_host) {
        return Err("GitHub request redirected to unexpected host".to_owned());
    }
    if !response.status().is_success() {
        return Err(format!(
            "GitHub request returned HTTP {}",
            response.status()
        ));
    }
    Ok(response)
}

fn default_source_ref() -> String {
    crate::runtime_meta::compiled_source_commit()
        .filter(|value| is_sha(value))
        .map(str::to_owned)
        .or_else(|| {
            env::var("HERDR_MCP_BUILD_COMMIT")
                .ok()
                .filter(|value| is_sha(value))
        })
        .unwrap_or_else(|| "main".to_owned())
}

fn validate_ref(value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > 200 || value.chars().any(|ch| ch.is_control()) {
        return Err("invalid standalone Git ref".to_owned());
    }
    Ok(())
}

fn is_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn safe_rel(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("extension tree contains unsafe path".to_owned());
    }
    Ok(path.to_path_buf())
}

fn home_dir() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_owned())
}

fn base_dir_for(home: &Path) -> PathBuf {
    home.join(".config/herdr-mcp/extensions/standalone")
}

fn user_visible_alias_path(home: &Path) -> PathBuf {
    home.join("Documents/herdr-mcp/extension")
}

fn inspect_user_visible_alias(home: &Path, current: &Path) -> Value {
    let path = user_visible_alias_path(home);
    inspect_alias(&path, current)
}

fn inspect_alias(path: &Path, current: &Path) -> Value {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => json!({
            "ready": false,
            "status": "missing",
            "path": path,
            "target": current,
        }),
        Err(error) => json!({
            "ready": false,
            "status": "error",
            "path": path,
            "target": current,
            "error": error.to_string(),
        }),
        Ok(metadata) if metadata.file_type().is_symlink() => match fs::read_link(path) {
            Ok(existing_target) if existing_target == current => json!({
                "ready": true,
                "status": "ready",
                "path": path,
                "target": current,
            }),
            Ok(existing_target) => json!({
                "ready": false,
                "status": "occupied",
                "kind": "symlink",
                "path": path,
                "target": current,
                "existing_target": existing_target,
            }),
            Err(error) => json!({
                "ready": false,
                "status": "error",
                "path": path,
                "target": current,
                "error": error.to_string(),
            }),
        },
        Ok(metadata) => json!({
            "ready": false,
            "status": "occupied",
            "kind": if metadata.is_dir() { "directory" } else if metadata.is_file() { "file" } else { "other" },
            "path": path,
            "target": current,
        }),
    }
}

fn ensure_user_visible_alias(home: &Path, current: &Path) -> Value {
    let path = user_visible_alias_path(home);
    ensure_alias(home, &path, current)
}

fn ensure_alias(home: &Path, path: &Path, current: &Path) -> Value {
    let initial = inspect_alias(path, current);
    if initial.get("status").and_then(Value::as_str) != Some("missing") {
        return initial;
    }
    let Some(parent) = path.parent() else {
        return json!({
            "ready": false,
            "status": "error",
            "path": path,
            "target": current,
            "error": "user-visible extension path has no parent",
        });
    };
    if let Err(error) = ensure_alias_parent(home, parent) {
        return json!({
            "ready": false,
            "status": "error",
            "path": path,
            "target": current,
            "error": error,
        });
    }
    match create_directory_symlink(current, path) {
        Ok(()) => json!({
            "ready": true,
            "status": "created",
            "path": path,
            "target": current,
        }),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => inspect_alias(path, current),
        Err(error) => json!({
            "ready": false,
            "status": "error",
            "path": path,
            "target": current,
            "error": error.to_string(),
        }),
    }
}

fn ensure_explicit_alias(home: &Path, path: &Path, current: &Path) -> Result<Value, String> {
    let result = ensure_alias(home, path, current);
    if result.get("ready").and_then(Value::as_bool) == Some(true) {
        return Ok(result);
    }
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Err(format!(
        "requested standalone --path '{}' is not usable ({status}); choose an unused path or the existing standalone symlink",
        path.display()
    ))
}

fn preflight_explicit_load_path(path: &Path, current: &Path) -> Result<(), String> {
    if path == current {
        return Ok(());
    }
    let state = inspect_alias(path, current);
    match state.get("status").and_then(Value::as_str) {
        Some("missing" | "ready") => Ok(()),
        Some(status) => Err(format!(
            "requested standalone --path '{}' is already occupied ({status}); refusing to overwrite it",
            path.display()
        )),
        None => Err(format!(
            "requested standalone --path '{}' cannot be inspected safely",
            path.display()
        )),
    }
}

fn resolve_load_path(home: &Path, value: &str) -> Result<PathBuf, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 4096 || value.chars().any(|ch| ch.is_control()) {
        return Err("invalid standalone --path".to_owned());
    }
    let path = if value == "~" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        let raw = PathBuf::from(value);
        if raw.is_absolute() {
            raw
        } else {
            home.join(raw)
        }
    };
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("standalone --path must not contain '..'".to_owned());
    }
    if path == home || !path.starts_with(home) {
        return Err("standalone --path must resolve below HOME".to_owned());
    }
    Ok(path)
}

fn ensure_alias_parent(home: &Path, parent: &Path) -> Result<(), String> {
    let canonical_home = fs::canonicalize(home)
        .map_err(|e| format!("cannot resolve HOME for standalone --path: {e}"))?;
    let mut ancestor = parent;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| "standalone --path has no existing ancestor".to_owned())?;
    }
    let canonical_ancestor = fs::canonicalize(ancestor)
        .map_err(|e| format!("cannot resolve standalone --path ancestor: {e}"))?;
    if !canonical_ancestor.starts_with(&canonical_home) {
        return Err("standalone --path would escape HOME through a symlinked parent".to_owned());
    }
    fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create standalone --path parent: {e}"))?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|e| format!("cannot resolve standalone --path parent: {e}"))?;
    if !canonical_parent.starts_with(canonical_home) {
        return Err("standalone --path parent resolves outside HOME".to_owned());
    }
    Ok(())
}

fn rollback_activation(current: &Path, backup: &Path, had_current: bool) {
    let _ = fs::remove_dir_all(current);
    if had_current {
        let _ = fs::rename(backup, current);
    }
}

fn remove_alias_if_created(alias: &Value, current: &Path) {
    if alias.get("status").and_then(Value::as_str) != Some("created") {
        return;
    }
    let Some(path) = alias.get("path").and_then(Value::as_str).map(PathBuf::from) else {
        return;
    };
    if fs::read_link(&path).ok().as_deref() == Some(current) {
        let _ = fs::remove_file(path);
    }
}

fn preferred_load_unpacked_path(alias: &Value, current: &Path) -> PathBuf {
    if alias.get("ready").and_then(Value::as_bool) == Some(true) {
        alias
            .get("path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| current.to_path_buf())
    } else {
        current.to_path_buf()
    }
}

#[cfg(unix)]
fn create_directory_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_directory_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(not(any(unix, windows)))]
fn create_directory_symlink(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory symlinks are unsupported on this platform",
    ))
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("invalid JSON {}: {e}", path.display()))
}

fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "state path has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|e| format!("cannot create state directory: {e}"))?;
    let tmp = parent.join(format!(".state-{}-{}.tmp", std::process::id(), now_ms()));
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    let mut file = fs::File::create(&tmp).map_err(|e| format!("cannot create state file: {e}"))?;
    file.write_all(&bytes)
        .map_err(|e| format!("cannot write state file: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("cannot sync state file: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("cannot activate state file: {e}"))
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_and_paths_are_strict() {
        assert!(is_sha(&"a".repeat(40)));
        assert!(!is_sha("main"));
        assert!(safe_rel("content/wake.js").is_ok());
        assert!(safe_rel("../Cargo.toml").is_err());
        assert!(safe_rel("content/../manifest.json").is_err());
        assert!(safe_rel("/tmp/file").is_err());
        assert_eq!(
            git_blob_sha(b"test content\n"),
            "d670460b4b4aece5915caf5c68d12f560a9fe3e4"
        );
    }

    #[test]
    fn manifest_injection_preserves_source_bytes_except_key() {
        let source = b"{\n  \"manifest_version\": 3,\n  \"name\": \"Herdr\",\n  \"version\": \"0.1.99\"\n}\n";
        let key = "abc123";
        let installed = inject_key(source, key).unwrap();
        let expected = b"{\n  \"manifest_version\": 3,\n  \"name\": \"Herdr\",\n  \"version\": \"0.1.99\",\n  \"key\": \"abc123\"\n}\n";
        assert_eq!(installed, expected);
    }

    #[test]
    fn prepared_manifest_uses_fixed_contract_identity() {
        let root = env::temp_dir().join(format!("herdr-standalone-test-{}", now_ms()));
        fs::create_dir_all(&root).unwrap();
        let source = b"{\n  \"manifest_version\": 3,\n  \"name\": \"Herdr\",\n  \"version\": \"0.1.99\"\n}\n";
        fs::write(root.join("manifest.json"), source).unwrap();
        let prepared = prepare_manifest(&root).unwrap();
        let manifest = read_json(&root.join("manifest.json")).unwrap();
        let identity = crate::browser_extension_identity::official_standalone_identity().unwrap();
        assert_eq!(manifest["key"], identity.manifest_key.unwrap());
        assert_eq!(prepared.extension_id, identity.extension_id);
        assert_eq!(prepared.source_sha256, sha256(source));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_load_paths_expand_under_home_and_reject_escape() {
        let home = PathBuf::from("/Users/example");
        assert_eq!(
            resolve_load_path(&home, "~/Documents/herdr/extension").unwrap(),
            home.join("Documents/herdr/extension")
        );
        assert_eq!(
            resolve_load_path(&home, "Downloads/herdr-extension").unwrap(),
            home.join("Downloads/herdr-extension")
        );
        assert!(resolve_load_path(&home, "~/../outside").is_err());
        assert!(resolve_load_path(&home, "/tmp/herdr-extension").is_err());
        assert!(resolve_load_path(&home, "~").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn explicit_load_path_is_created_and_conflicts_fail_closed() {
        let root = env::temp_dir().join(format!("herdr-standalone-custom-alias-{}", now_ms()));
        let home = root.join("home");
        let current = base_dir_for(&home).join("current");
        let custom = home.join("Downloads/herdr-extension");
        fs::create_dir_all(&current).unwrap();

        preflight_explicit_load_path(&custom, &current).unwrap();
        let created = ensure_explicit_alias(&home, &custom, &current).unwrap();
        assert_eq!(created["status"], "created");
        assert_eq!(fs::read_link(&custom).unwrap(), current);
        preflight_explicit_load_path(&custom, &current).unwrap();

        fs::remove_file(&custom).unwrap();
        fs::create_dir_all(&custom).unwrap();
        let error = preflight_explicit_load_path(&custom, &current).unwrap_err();
        assert!(error.contains("refusing to overwrite"));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn user_visible_alias_is_created_once_and_reused() {
        let root = env::temp_dir().join(format!("herdr-standalone-alias-test-{}", now_ms()));
        let home = root.join("home");
        let current = base_dir_for(&home).join("current");
        fs::create_dir_all(&current).unwrap();

        let created = ensure_user_visible_alias(&home, &current);
        assert_eq!(created["ready"], true);
        assert_eq!(created["status"], "created");
        let alias = user_visible_alias_path(&home);
        assert_eq!(fs::read_link(&alias).unwrap(), current);

        let reused = ensure_user_visible_alias(&home, &current);
        assert_eq!(reused["ready"], true);
        assert_eq!(reused["status"], "ready");
        assert_eq!(preferred_load_unpacked_path(&reused, &current), alias);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn user_visible_alias_never_overwrites_an_occupied_path() {
        let root = env::temp_dir().join(format!("herdr-standalone-alias-conflict-{}", now_ms()));
        let home = root.join("home");
        let current = base_dir_for(&home).join("current");
        let alias = user_visible_alias_path(&home);
        fs::create_dir_all(&current).unwrap();
        fs::create_dir_all(&alias).unwrap();

        let occupied = ensure_user_visible_alias(&home, &current);
        assert_eq!(occupied["ready"], false);
        assert_eq!(occupied["status"], "occupied");
        assert_eq!(occupied["kind"], "directory");
        assert!(alias.is_dir());
        assert_eq!(preferred_load_unpacked_path(&occupied, &current), current);

        #[cfg(unix)]
        {
            fs::remove_dir(&alias).unwrap();
            let other = root.join("other-extension");
            fs::create_dir_all(&other).unwrap();
            std::os::unix::fs::symlink(&other, &alias).unwrap();
            let wrong_link = ensure_user_visible_alias(&home, &current);
            assert_eq!(wrong_link["ready"], false);
            assert_eq!(wrong_link["status"], "occupied");
            assert_eq!(wrong_link["kind"], "symlink");
            assert_eq!(fs::read_link(&alias).unwrap(), other);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_separates_managed_install_browser_load_and_native_host_evidence() {
        let root = env::temp_dir().join(format!("herdr-standalone-status-test-{}", now_ms()));
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let view = status_view(
            &home,
            json!({
                "ok": true,
                "active_channel": "standalone",
                "standalone_origin_match": true,
            }),
        )
        .unwrap();

        assert_eq!(view["ok"], true);
        assert_eq!(view["installed"], false);
        assert_eq!(view["installed_semantics"], "managed_installer_copy_valid");
        assert_eq!(view["managed_install"]["valid"], false);
        assert_eq!(view["browser_load"]["loaded_observed"], Value::Null);
        assert_eq!(view["native_host"]["configured"], true);
        assert_eq!(view["native_host"]["standalone_origin_match"], true);
        assert_eq!(view["native_host"]["connected_observed"], Value::Null);
        let _ = fs::remove_dir_all(root);
    }
}
