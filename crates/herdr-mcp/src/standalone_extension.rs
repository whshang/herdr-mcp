use reqwest::blocking::{Client, Response};
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::Write;
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
    let base = base_dir()?;
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
    let view = json!({
        "ok": installed,
        "installed": installed,
        "path": current,
        "extension_id": identity.extension_id,
        "manifest_key_matches": key_ok,
        "state": state,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&view).map_err(|e| e.to_string())?
    );
    Ok(if installed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn install(options: StandaloneInstallOptions) -> Result<Value, String> {
    let requested_ref = options.reference.unwrap_or_else(default_source_ref);
    validate_ref(&requested_ref)?;
    let client = client()?;
    let commit = resolve_ref(&client, &requested_ref)?;
    let blobs = list_blobs(&client, &commit)?;

    let base = base_dir()?;
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

    let current = base.join("current");
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
    });
    if let Err(error) = atomic_json(&base.join("state.json"), &state) {
        let _ = fs::remove_dir_all(&current);
        if had_current {
            let _ = fs::rename(&backup, &current);
        }
        return Err(error);
    }
    if backup.exists() {
        let _ = fs::remove_dir_all(&backup);
    }

    Ok(json!({
        "ok": true,
        "channel": "standalone",
        "repository": REPOSITORY,
        "requested_ref": state["requested_ref"],
        "source_commit": state["source_commit"],
        "extension_version": state["extension_version"],
        "extension_id": state["extension_id"],
        "path": current,
        "chrome": {
            "url": "chrome://extensions",
            "action": "Developer mode -> Load unpacked",
            "load_unpacked_path": current,
        },
        "next_command": "herdr-mcp native-host use standalone",
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

fn base_dir() -> Result<PathBuf, String> {
    let home = env::var_os("HOME").ok_or_else(|| "HOME is not set".to_owned())?;
    Ok(PathBuf::from(home).join(".config/herdr-mcp/extensions/standalone"))
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
}
