use crate::cli::WorkerCommand;
use crate::config::Config;
use crate::link::ownership::LINK_PROD_LABEL;
use crate::paths::RuntimePaths;
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

const ENROLLMENT_FILE_SCHEMA: u32 = 1;
const LEGACY_LINK_KEYCHAIN_SERVICE: &str = "herdr-edge-prod-link-secret";
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Serialize, Deserialize)]
struct EnrollmentFile {
    schema_version: u32,
    edge_origin: String,
    enrollment_code: String,
    expires_at_ms: u64,
}

#[derive(Debug)]
struct OwnerLinkIdentity {
    edge_origin: String,
    workstation_id: String,
    credential: String,
}

#[derive(Debug)]
struct EnrolledCredential {
    device_id: String,
    workstation_id: String,
    device_secret: String,
}

pub fn run(command: WorkerCommand) -> Result<ExitCode, String> {
    let paths = RuntimePaths::discover()?;
    if paths.instance.is_named() {
        return Err("Worker enrollment is available only on the default Herdr instance".to_owned());
    }
    match command {
        WorkerCommand::EnrollmentCreate {
            ttl_seconds,
            name,
            output,
        } => create_enrollment(&paths, ttl_seconds, name.as_deref(), output.as_deref()),
        WorkerCommand::Connect {
            enrollment_file,
            edge_origin,
            name,
        } => connect_existing_worker(
            &paths,
            Path::new(&enrollment_file),
            edge_origin.as_deref(),
            name.as_deref(),
        ),
    }
}

fn create_enrollment(
    paths: &RuntimePaths,
    ttl_seconds: u64,
    name: Option<&str>,
    output: Option<&str>,
) -> Result<ExitCode, String> {
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let owner = resolve_owner_link_identity(paths, &config)?;
    let endpoint = endpoint(&owner.edge_origin, "/devices/enrollments")?;
    let mut headers = bearer_headers(&owner.credential)?;
    headers.insert(
        "x-herdr-workstation",
        HeaderValue::from_str(&owner.workstation_id)
            .map_err(|_| "current workstation identity is not a valid HTTP header".to_owned())?,
    );
    let response = client()?
        .post(endpoint)
        .headers(headers)
        .json(&json!({ "ttl_seconds": ttl_seconds, "name": name }))
        .send()
        .map_err(|error| format!("cannot create device enrollment: {error}"))?;
    let payload = parse_json_response(response, "device enrollment creation")?;
    let enrollment_code = payload
        .get("enrollment_code")
        .and_then(Value::as_str)
        .ok_or_else(|| "device enrollment creation returned no enrollment code".to_owned())?;
    validate_enrollment_code(enrollment_code)?;
    let expires_at_ms = payload
        .get("expires_at_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| "device enrollment creation returned no expiry".to_owned())?;

    let path = enrollment_output_path(paths, output, expires_at_ms)?;
    let file = EnrollmentFile {
        schema_version: ENROLLMENT_FILE_SCHEMA,
        edge_origin: owner.edge_origin,
        enrollment_code: enrollment_code.to_owned(),
        expires_at_ms,
    };
    write_secret_json_new(&path, &file)?;
    print_json(&json!({
        "ok": true,
        "action": "worker_enrollment_create",
        "enrollment_file": path,
        "expires_at_ms": expires_at_ms,
        "workstation_id": owner.workstation_id,
        "secret_printed": false,
    }))?;
    Ok(ExitCode::SUCCESS)
}

fn connect_existing_worker(
    paths: &RuntimePaths,
    enrollment_path: &Path,
    edge_origin_override: Option<&str>,
    name: Option<&str>,
) -> Result<ExitCode, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (paths, enrollment_path, edge_origin_override, name);
        return Err(
            "worker connect currently requires macOS Keychain; refusing to consume a one-time enrollment on this platform"
                .to_owned(),
        );
    }

    #[cfg(target_os = "macos")]
    {
        let enrollment = read_enrollment_file(enrollment_path)?;
        if enrollment.expires_at_ms <= now_ms() {
            return Err("device enrollment file has expired; create a new enrollment".to_owned());
        }
        let file_origin = normalize_edge_origin(&enrollment.edge_origin)?;
        let edge_origin = match edge_origin_override {
            Some(value) => {
                let normalized = normalize_edge_origin(value)?;
                if normalized != file_origin {
                    return Err(
                        "--edge-origin does not match the Worker bound into the enrollment file"
                            .to_owned(),
                    );
                }
                normalized
            }
            None => file_origin,
        };
        validate_enrollment_code(&enrollment.enrollment_code)?;

        let enrolled = consume_enrollment(&edge_origin, &enrollment.enrollment_code, name)?;
        let device_id = crate::config::normalize_device_id(&enrolled.device_id)?;
        if enrolled.workstation_id != device_id {
            let _ = revoke_self(
                &edge_origin,
                &enrolled.workstation_id,
                &enrolled.device_secret,
            );
            return Err("Worker returned a workstation identity that does not match the immutable device_id".to_owned());
        }
        validate_device_secret(&enrolled.device_secret)?;

        let account = current_account()?;
        let keychain_service = format!("herdr-edge-link-{device_id}");
        if let Err(error) = crate::macos_keychain::store_generic_secret(
            &keychain_service,
            &account,
            &enrolled.device_secret,
        ) {
            let revoked =
                revoke_self(&edge_origin, &device_id, &enrolled.device_secret).unwrap_or(false);
            return Err(format!(
                "cannot persist the new device credential; remote compensation revoked={revoked}: {error}"
            ));
        }

        let mut config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
        config.set_edge_public_origin(&edge_origin)?;
        config.set_edge_device_id(&device_id)?;
        if let Err(error) = write_config_atomic(paths, &config) {
            return Err(format!(
                "device credential is safely stored in Keychain service {keychain_service}, but config update failed for {device_id}: {error}"
            ));
        }

        let enrollment_deleted = fs::remove_file(enrollment_path).is_ok();
        if let Err(error) = crate::link::reconcile_after_service_generation_change(paths) {
            return Err(format!(
                "device {device_id} is enrolled and persisted, but production Link reconciliation failed: {error}"
            ));
        }

        print_json(&json!({
            "ok": true,
            "action": "worker_connect",
            "device_id": device_id,
            "workstation_id": enrolled.workstation_id,
            "edge_origin": edge_origin,
            "keychain_service": keychain_service,
            "enrollment_file_deleted": enrollment_deleted,
            "secret_printed": false,
            "link_reconciled": true,
        }))?;
        Ok(ExitCode::SUCCESS)
    }
}

fn consume_enrollment(
    edge_origin: &str,
    enrollment_code: &str,
    name: Option<&str>,
) -> Result<EnrolledCredential, String> {
    let response = client()?
        .post(endpoint(edge_origin, "/devices/enroll")?)
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({ "enrollment_code": enrollment_code, "name": name }))
        .send()
        .map_err(|error| format!("cannot consume device enrollment: {error}"))?;
    let payload = parse_json_response(response, "device enrollment consumption")?;
    Ok(EnrolledCredential {
        device_id: required_string(&payload, "device_id")?,
        workstation_id: required_string(&payload, "workstation_id")?,
        device_secret: required_string(&payload, "device_secret")?,
    })
}

fn revoke_self(edge_origin: &str, workstation_id: &str, credential: &str) -> Result<bool, String> {
    let response = client()?
        .post(endpoint(edge_origin, "/devices/revoke-self")?)
        .headers(bearer_headers(credential)?)
        .json(&json!({ "workstation_id": workstation_id }))
        .send()
        .map_err(|error| format!("cannot compensate failed device enrollment: {error}"))?;
    Ok(response.status().is_success())
}

fn resolve_owner_link_identity(
    paths: &RuntimePaths,
    config: &Config,
) -> Result<OwnerLinkIdentity, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (paths, config);
        return Err("device enrollment creation currently requires macOS Keychain".to_owned());
    }

    #[cfg(target_os = "macos")]
    {
        let plist_env = production_link_environment()?;
        let workstation_id = config
            .edge_device_id
            .clone()
            .or_else(|| plist_env.get("HERDR_WORKSTATION_ID").cloned())
            .ok_or_else(|| "production Link does not expose a workstation identity".to_owned())?;
        let keychain_service = config
            .edge_link_keychain_service()
            .or_else(|| plist_env.get("HERDR_LINK_KEYCHAIN_SERVICE").cloned())
            .unwrap_or_else(|| LEGACY_LINK_KEYCHAIN_SERVICE.to_owned());
        let edge_origin = match config.edge_public_origin.clone() {
            Some(origin) => normalize_edge_origin(&origin)?,
            None => {
                let edge_url = plist_env.get("HERDR_EDGE_URL").ok_or_else(|| {
                    "configure [edge].public_origin before creating an enrollment".to_owned()
                })?;
                origin_from_ws_url(edge_url)?
            }
        };
        let account = current_account()?;
        let credential = crate::macos_keychain::load_generic_secret(&keychain_service, &account)?;
        let _ = paths;
        Ok(OwnerLinkIdentity {
            edge_origin,
            workstation_id,
            credential,
        })
    }
}

#[cfg(target_os = "macos")]
fn production_link_environment() -> Result<std::collections::BTreeMap<String, String>, String> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is required to locate the production Link plist".to_owned())?;
    let path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LINK_PROD_LABEL}.plist"));
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
    Ok(env_dict
        .iter()
        .filter_map(|(key, value)| {
            value
                .as_string()
                .map(|value| (key.clone(), value.to_owned()))
        })
        .collect())
}

fn enrollment_output_path(
    paths: &RuntimePaths,
    output: Option<&str>,
    expires_at_ms: u64,
) -> Result<PathBuf, String> {
    let path = match output {
        Some(value) => PathBuf::from(value),
        None => paths
            .config_dir
            .join("enrollments")
            .join(format!("device-enrollment-{expires_at_ms}.json")),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "cannot create enrollment directory {}: {error}",
                parent.display()
            )
        })?;
    }
    Ok(path)
}

fn write_secret_json_new(path: &Path, value: &EnrollmentFile) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("cannot encode enrollment file: {error}"))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot create enrollment file {}: {error}", path.display()))?;
    file.write_all(&bytes)
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot persist enrollment file {}: {error}", path.display()))?;
    Ok(())
}

fn read_enrollment_file(path: &Path) -> Result<EnrollmentFile, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("cannot inspect enrollment file {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err("enrollment path must be a regular file".to_owned());
    }
    #[cfg(unix)]
    {
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("enrollment file must be mode 0600 (not group/world readable)".to_owned());
        }
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err("enrollment file must be owned by the current user".to_owned());
        }
    }
    let bytes = fs::read(path)
        .map_err(|error| format!("cannot read enrollment file {}: {error}", path.display()))?;
    if bytes.len() > 16 * 1024 {
        return Err("enrollment file exceeds the 16 KiB safety bound".to_owned());
    }
    let file: EnrollmentFile = serde_json::from_slice(&bytes)
        .map_err(|_| "enrollment file is not valid Herdr enrollment JSON".to_owned())?;
    if file.schema_version != ENROLLMENT_FILE_SCHEMA {
        return Err(format!(
            "unsupported enrollment file schema {}",
            file.schema_version
        ));
    }
    Ok(file)
}

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

fn client() -> Result<Client, String> {
    Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(Policy::none())
        .build()
        .map_err(|error| format!("cannot initialize Worker HTTP client: {error}"))
}

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

fn endpoint(origin: &str, path: &str) -> Result<Url, String> {
    let mut url = Url::parse(&normalize_edge_origin(origin)?)
        .map_err(|error| format!("invalid Worker origin: {error}"))?;
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn normalize_edge_origin(value: &str) -> Result<String, String> {
    let mut config = Config::default();
    config.set_edge_public_origin(value)?;
    config
        .edge_public_origin
        .ok_or_else(|| "Worker origin is missing".to_owned())
}

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

fn required_string(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(|| format!("Worker response is missing {key}"))
}

fn validate_enrollment_code(value: &str) -> Result<(), String> {
    let suffix = value
        .strip_prefix("enroll_")
        .ok_or_else(|| "Worker returned an invalid enrollment code".to_owned())?;
    if suffix.len() != 64 || !suffix.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("Worker returned an invalid enrollment code".to_owned());
    }
    Ok(())
}

fn validate_device_secret(value: &str) -> Result<(), String> {
    let suffix = value
        .strip_prefix("devsec_")
        .ok_or_else(|| "Worker returned an invalid device credential".to_owned())?;
    if suffix.len() != 64 || !suffix.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("Worker returned an invalid device credential".to_owned());
    }
    Ok(())
}

fn current_account() -> Result<String, String> {
    env::var("USER")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| {
            !value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| "USER is required for macOS Keychain device credentials".to_owned())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

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
    fn secret_validators_are_strict_and_never_accept_argv_shaped_garbage() {
        assert!(validate_enrollment_code(&format!("enroll_{}", "a".repeat(64))).is_ok());
        assert!(validate_enrollment_code("enroll_short").is_err());
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

    #[cfg(unix)]
    #[test]
    fn enrollment_file_reads_require_0600_owner_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = env::temp_dir().join(format!(
            "herdr-worker-enrollment-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("enrollment.json");
        let body = serde_json::to_vec(&EnrollmentFile {
            schema_version: ENROLLMENT_FILE_SCHEMA,
            edge_origin: "https://edge.example".to_owned(),
            enrollment_code: format!("enroll_{}", "a".repeat(64)),
            expires_at_ms: now_ms() + 60_000,
        })
        .unwrap();

        fs::write(&path, &body).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&path, permissions).unwrap();
        assert!(read_enrollment_file(&path).is_err());

        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&path, permissions).unwrap();
        let file = read_enrollment_file(&path).unwrap();
        assert_eq!(file.schema_version, ENROLLMENT_FILE_SCHEMA);
        assert_eq!(file.edge_origin, "https://edge.example");
        assert!(validate_enrollment_code(&file.enrollment_code).is_ok());

        let _ = fs::remove_dir_all(&dir);
    }
}
