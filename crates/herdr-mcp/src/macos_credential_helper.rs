//! Stable, narrow macOS Keychain credential helper.
//!
//! The helper is a copied executable at a stable path, so rotating ad-hoc
//! runtime generations never become new Keychain clients. Secrets cross the
//! boundary only as bounded JSON on stdin/stdout, never in argv.

#[cfg(any(target_os = "macos", test))]
use crate::macos_keychain;
use serde_json::{Value, json};
#[cfg(any(target_os = "macos", test))]
use std::collections::BTreeMap;
use std::io::{Read, Write};
#[cfg(any(target_os = "macos", test))]
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "macos", test))]
use crate::config::Config;
#[cfg(any(target_os = "macos", test))]
use std::process::Command;
use std::process::ExitCode;
#[cfg(target_os = "macos")]
use std::process::Stdio;

pub const PROTOCOL_VERSION: u32 = 1;
#[cfg(any(target_os = "macos", test))]
pub const COMPAT_REVISION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 16 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024;
#[cfg(any(target_os = "macos", test))]
const METADATA_SCHEMA: u32 = 1;
#[cfg(any(target_os = "macos", test))]
const HELPER_NAME: &str = "herdr-mcp-credential-helper";

#[cfg(any(target_os = "macos", test))]
pub fn helper_path(config_dir: &Path) -> PathBuf {
    config_dir.join(HELPER_NAME)
}
#[cfg(any(target_os = "macos", test))]
pub fn metadata_path(config_dir: &Path) -> PathBuf {
    config_dir.join(format!("{HELPER_NAME}.json"))
}

#[cfg(target_os = "macos")]
fn candidate_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("HERDR_MCP_CREDENTIAL_HELPER_CANDIDATE") {
        let path = PathBuf::from(path);
        let m = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("cannot inspect helper candidate {}: {e}", path.display()))?;
        if m.file_type().is_symlink() || !m.is_file() {
            return Err(format!(
                "helper candidate {} is not a regular file",
                path.display()
            ));
        }
        return Ok(path);
    }
    std::env::current_exe().map_err(|e| format!("cannot resolve current executable: {e}"))
}

#[cfg(any(target_os = "macos", test))]
fn valid_regular(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| !m.file_type().is_symlink() && m.is_file())
        .unwrap_or(false)
}

#[cfg(any(target_os = "macos", test))]
fn metadata_bytes() -> Vec<u8> {
    serde_json::to_vec(
        &json!({"schema_version": METADATA_SCHEMA, "compat_revision": COMPAT_REVISION}),
    )
    .expect("static metadata")
}

#[cfg(any(target_os = "macos", test))]
pub fn installed_revision(config_dir: &Path) -> Option<u32> {
    if !valid_regular(&helper_path(config_dir)) || !valid_regular(&metadata_path(config_dir)) {
        return None;
    }
    let bytes = std::fs::read(metadata_path(config_dir)).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    if v.get("schema_version")?.as_u64()? != METADATA_SCHEMA as u64 {
        return None;
    }
    u32::try_from(v.get("compat_revision")?.as_u64()?).ok()
}

#[cfg(target_os = "macos")]
pub fn preserve_or_install(config_dir: &Path) -> Result<(), String> {
    let source = candidate_path()?;
    preserve_or_install_from(config_dir, &source)
}

#[cfg(any(target_os = "macos", test))]
fn preserve_or_install_from(config_dir: &Path, source: &Path) -> Result<(), String> {
    let target = helper_path(config_dir);
    if let Ok(m) = std::fs::symlink_metadata(&target) {
        if m.file_type().is_symlink() || !m.is_file() {
            return Err(format!(
                "credential helper {} must be a regular file and not a symlink",
                target.display()
            ));
        }
        if installed_revision(config_dir) == Some(COMPAT_REVISION) {
            return Ok(());
        }
        return Err(format!(
            "credential helper {} has missing or invalid compatibility metadata",
            target.display()
        ));
    }
    let source_metadata = std::fs::symlink_metadata(source).map_err(|e| {
        format!(
            "cannot inspect credential helper candidate {}: {e}",
            source.display()
        )
    })?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_file() {
        return Err(format!(
            "credential helper candidate {} is not a regular file",
            source.display()
        ));
    }
    let bytes = std::fs::read(source).map_err(|e| {
        format!(
            "cannot read credential helper candidate {}: {e}",
            source.display()
        )
    })?;
    std::fs::create_dir_all(config_dir)
        .map_err(|e| format!("cannot create {}: {e}", config_dir.display()))?;
    atomic_write(&target, &bytes, 0o700)?;
    if let Err(error) = atomic_write(&metadata_path(config_dir), &metadata_bytes(), 0o600) {
        let cleanup = std::fs::remove_file(&target).err();
        return Err(match cleanup {
            Some(cleanup) => format!(
                "credential helper metadata install failed: {error}; fresh helper cleanup also failed: {cleanup}"
            ),
            None => format!("credential helper metadata install failed: {error}"),
        });
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "credential helper path has no parent".to_owned())?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("helper");
    let temp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode);
    }
    let mut f = opts
        .open(&temp)
        .map_err(|e| format!("cannot create {}: {e}", temp.display()))?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("cannot install {}: {e}", path.display())
    })
}

pub fn run_once() -> ExitCode {
    let mut input = Vec::new();
    let stdin = std::io::stdin();
    if stdin
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .is_err()
        || input.len() > MAX_REQUEST_BYTES
    {
        return write_response(
            &json!({"ok":false,"code":"request_invalid","message":"request exceeds limit"}),
        );
    }
    let response = match handle_request(&input) {
        Ok(v) => v,
        Err(e) => json!({"ok":false,"code":"request_invalid","message":e}),
    };
    write_response(&response)
}

fn write_response(value: &Value) -> ExitCode {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return ExitCode::from(3);
    };
    if bytes.len() > MAX_RESPONSE_BYTES {
        return ExitCode::from(3);
    }
    let mut out = std::io::stdout();
    if out.write_all(&bytes).and_then(|_| out.flush()).is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(3)
    }
}

#[cfg(any(target_os = "macos", test))]
fn valid_label(value: &str, allow_empty: bool) -> bool {
    (allow_empty || !value.is_empty()) && value.len() <= 255 && !value.chars().any(char::is_control)
}

#[cfg(any(target_os = "macos", test))]
fn owned_service(value: &str) -> bool {
    valid_label(value, false)
        && (value == "herdr-edge-prod-link-secret"
            || value.starts_with("herdr-edge-link-")
            || value.starts_with("herdr-edge-"))
}

#[cfg(any(target_os = "macos", test))]
fn account_from_user_or_id(user: Option<&str>, id_output: Option<&[u8]>) -> Option<String> {
    if let Some(value) = user.filter(|value| valid_label(value, false)) {
        return Some(value.to_owned());
    }
    let value = std::str::from_utf8(id_output?).ok()?.trim();
    valid_label(value, false).then_some(value.to_owned())
}

#[cfg(any(target_os = "macos", test))]
fn current_account() -> Option<String> {
    let user = std::env::var("USER").ok();
    if let Some(account) = account_from_user_or_id(user.as_deref(), None) {
        return Some(account);
    }
    let mut command = Command::new("/usr/bin/id");
    command.arg("-un");
    let output = crate::child_process::run_bounded_output(
        &mut command,
        std::time::Duration::from_secs(2),
        256,
    )
    .ok()??;
    output
        .status
        .success()
        .then(|| account_from_user_or_id(None, Some(&output.stdout)))
        .flatten()
}

pub fn handle_request(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("request exceeds limit".to_owned());
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|_| "invalid request JSON".to_owned())?;
    let o = v
        .as_object()
        .ok_or_else(|| "request must be an object".to_owned())?;
    let required = ["protocol", "version", "op", "service", "account"];
    if !required.iter().all(|k| o.contains_key(*k)) {
        return Err("request has invalid fields".to_owned());
    }
    if o.get("protocol").and_then(Value::as_str) != Some("herdr-keychain-helper")
        || o.get("version").and_then(Value::as_u64) != Some(PROTOCOL_VERSION as u64)
    {
        return Err("unsupported helper protocol".to_owned());
    }
    let op = o
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| "operation is invalid".to_owned())?;
    let service = o
        .get("service")
        .and_then(Value::as_str)
        .ok_or_else(|| "service is invalid".to_owned())?;
    let account = o
        .get("account")
        .and_then(Value::as_str)
        .ok_or_else(|| "account is invalid".to_owned())?;
    let is_store = op == "store";
    let expected_fields = if is_store { 6 } else { 5 };
    if o.len() != expected_fields
        || (!is_store && o.contains_key("secret"))
        || (is_store && !o.contains_key("secret"))
    {
        return Err("request has invalid fields".to_owned());
    }
    #[cfg(any(target_os = "macos", test))]
    {
        if !owned_service(service) || current_account().as_deref() != Some(account) {
            return Err("credential request is not authorized".to_owned());
        }
        match op {
            "load" => macos_keychain::load_generic_secret(service, account)
                .map(|secret| json!({"ok":true,"secret":secret})),
            "store" => {
                let secret = o
                    .get("secret")
                    .and_then(Value::as_str)
                    .filter(|s| {
                        !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control)
                    })
                    .ok_or_else(|| "secret is invalid".to_owned())?;
                macos_keychain::store_generic_secret(service, account, secret)
                    .map(|_| json!({"ok":true}))
            }
            "delete" => {
                macos_keychain::delete_generic_secret(service, account).map(|_| json!({"ok":true}))
            }
            _ => Err("operation is not allowed".to_owned()),
        }
    }
    #[cfg(all(not(target_os = "macos"), not(test)))]
    {
        let _ = (op, service, account);
        Err("credential helper requires macOS".to_owned())
    }
}

#[cfg(target_os = "macos")]
fn owned_production_link_environment(
    home: &Path,
) -> Result<Option<BTreeMap<String, String>>, String> {
    let path = home.join("Library/LaunchAgents/dev.herdr-mcp.link-prod.plist");
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect production Link plist: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("production Link plist must be a regular file".to_owned());
    }
    let root = plist::Value::from_file(&path)
        .map_err(|_| "cannot read production Link plist".to_owned())?;
    owned_production_link_environment_from_plist(home, &root)
}

#[cfg(any(target_os = "macos", test))]
fn owned_production_link_environment_from_plist(
    home: &Path,
    root: &plist::Value,
) -> Result<Option<BTreeMap<String, String>>, String> {
    let dict = root
        .as_dictionary()
        .ok_or_else(|| "production Link plist root is not a dictionary".to_owned())?;
    if dict.get("Label").and_then(plist::Value::as_string)
        != Some(crate::link::ownership::LINK_PROD_LABEL)
    {
        return Ok(None);
    }
    let program = dict
        .get("ProgramArguments")
        .and_then(plist::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(plist::Value::as_string)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if program.len() != 3
        || !crate::link::ownership::program_points_at_managed_runtime(&program, home)
        || program[1] != "link"
        || program[2] != "run"
    {
        return Ok(None);
    }
    let env = dict
        .get("EnvironmentVariables")
        .and_then(plist::Value::as_dictionary)
        .ok_or_else(|| "production Link plist has no environment".to_owned())?;
    let values = env
        .iter()
        .filter_map(|(key, value)| value.as_string().map(|v| (key.clone(), v.to_owned())))
        .collect::<BTreeMap<_, _>>();
    let workstation = values
        .get("HERDR_WORKSTATION_ID")
        .filter(|v| valid_label(v, false));
    let service = values
        .get("HERDR_LINK_KEYCHAIN_SERVICE")
        .filter(|v| owned_service(v));
    if workstation.is_some() && service.is_some() {
        Ok(Some(values))
    } else {
        Ok(None)
    }
}

#[cfg(any(target_os = "macos", test))]
fn prewarm_service(
    config: &Config,
    production_env: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    if let Some(service) = production_env
        .and_then(|env| env.get("HERDR_LINK_KEYCHAIN_SERVICE"))
        .filter(|v| owned_service(v))
    {
        return Some(service.clone());
    }
    if config.edge_device_id.is_some() {
        return config
            .edge_link_keychain_service()
            .filter(|v| owned_service(v));
    }
    None
}

#[cfg(target_os = "macos")]
pub fn prewarm_existing_default(_config_dir: &Path) -> Result<(), String> {
    let paths = crate::paths::RuntimePaths::discover()?;
    if paths.instance.is_named() {
        return Ok(());
    }
    let config = crate::config::Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is required to inspect production Link".to_owned())?;
    let production_env = owned_production_link_environment(&home)?;
    let Some(service) = prewarm_service(&config, production_env.as_ref()) else {
        return Ok(());
    };
    let account = current_account().ok_or_else(|| {
        "cannot determine current local account for credential helper prewarm".to_owned()
    })?;
    // Exactly one bounded load. Discard the returned secret immediately.
    let _ = load(&service, &account).map_err(|_| {
        "stable credential helper authorization is required once before runtime mutation".to_owned()
    })?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn read_capped<R: Read>(reader: &mut R, max: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "credential helper response read failed".to_owned())?;
    if bytes.len() > max {
        return Err("credential helper response exceeds limit".to_owned());
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
fn call(
    op: &str,
    service: &str,
    account: &str,
    secret: Option<&str>,
) -> Result<Option<String>, String> {
    let config = crate::paths::RuntimePaths::discover()?.config_dir;
    preserve_or_install(&config)?;
    let mut request = json!({"protocol":"herdr-keychain-helper","version":PROTOCOL_VERSION,"op":op,"service":service,"account":account});
    if let Some(secret) = secret {
        request["secret"] = json!(secret);
    }
    let helper = helper_path(&config);
    let metadata = std::fs::symlink_metadata(&helper)
        .map_err(|e| format!("cannot inspect credential helper: {e}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("credential helper is not a regular file".to_owned());
    }
    let mut child = Command::new(&helper)
        .arg("__credential-helper")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot spawn credential helper: {e}"))?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| "credential helper stdin unavailable".to_owned())?;
    input
        .write_all(
            &serde_json::to_vec(&request)
                .map_err(|e| format!("cannot encode credential request: {e}"))?,
        )
        .map_err(|e| format!("cannot send credential request: {e}"))?;
    drop(input);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "credential helper stdout unavailable".to_owned())?;
    let reader = std::thread::spawn(move || {
        let mut reader = stdout;
        read_capped(&mut reader, MAX_RESPONSE_BYTES)
    });
    let status = crate::child_process::wait_bounded(&mut child, std::time::Duration::from_secs(60))
        .map_err(|e| format!("credential helper wait failed: {e}"))?;
    let output = reader
        .join()
        .map_err(|_| "credential helper reader failed".to_owned())??;
    let Some(status) = status else {
        return Err(
            "credential helper request timed out; authorize the stable helper once and retry"
                .to_owned(),
        );
    };
    if !status.success() {
        return Err("credential helper returned an error".to_owned());
    }
    let v: Value = serde_json::from_slice(&output)
        .map_err(|_| "credential helper returned invalid response".to_owned())?;
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("credential helper operation failed".to_owned());
    }
    Ok(v.get("secret").and_then(Value::as_str).map(str::to_owned))
}

#[cfg(target_os = "macos")]
pub fn load(service: &str, account: &str) -> Result<String, String> {
    call("load", service, account, None)?
        .ok_or_else(|| "credential helper returned no secret".to_owned())
}
#[cfg(target_os = "macos")]
pub fn store(service: &str, account: &str, secret: &str) -> Result<(), String> {
    call("store", service, account, Some(secret)).map(|_| ())
}
#[cfg(target_os = "macos")]
pub fn delete(service: &str, account: &str) -> Result<(), String> {
    call("delete", service, account, None).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "herdr-mcp-credential-helper-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    #[test]
    fn path_is_stable_and_outside_generations() {
        let p = helper_path(Path::new("/tmp/config"));
        assert_eq!(p, Path::new("/tmp/config/herdr-mcp-credential-helper"));
        assert!(!p.to_string_lossy().contains("generations"));
    }
    #[test]
    fn account_resolution_falls_back_to_id_output() {
        assert_eq!(
            account_from_user_or_id(None, Some(b"launchuser\n")),
            Some("launchuser".to_owned())
        );
        assert_eq!(
            account_from_user_or_id(Some("bad\nuser"), Some(b"launchuser\n")),
            Some("launchuser".to_owned())
        );
        assert_eq!(account_from_user_or_id(None, Some(b"")), None);
    }
    #[test]
    fn prewarm_plist_requires_exact_owned_managed_link_prod() {
        let home = Path::new("/Users/tester");
        let managed = home.join(".config/herdr-mcp/runtime/current/herdr-mcp");
        let make = |label: &str, program0: &Path| {
            let mut root = plist::Dictionary::new();
            root.insert("Label".to_owned(), plist::Value::String(label.to_owned()));
            root.insert(
                "ProgramArguments".to_owned(),
                plist::Value::Array(vec![
                    plist::Value::String(program0.to_string_lossy().into_owned()),
                    plist::Value::String("link".to_owned()),
                    plist::Value::String("run".to_owned()),
                ]),
            );
            let mut env = plist::Dictionary::new();
            env.insert(
                "HERDR_WORKSTATION_ID".to_owned(),
                plist::Value::String("prod-real-runtime".to_owned()),
            );
            env.insert(
                "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
                plist::Value::String("herdr-edge-prod-link-secret".to_owned()),
            );
            root.insert(
                "EnvironmentVariables".to_owned(),
                plist::Value::Dictionary(env),
            );
            plist::Value::Dictionary(root)
        };
        assert!(
            owned_production_link_environment_from_plist(
                home,
                &make(crate::link::ownership::LINK_PROD_LABEL, &managed)
            )
            .unwrap()
            .is_some()
        );
        assert!(
            owned_production_link_environment_from_plist(home, &make("foreign.link", &managed))
                .unwrap()
                .is_none()
        );
        assert!(
            owned_production_link_environment_from_plist(
                home,
                &make(
                    crate::link::ownership::LINK_PROD_LABEL,
                    Path::new("/tmp/foreign")
                )
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn legacy_production_link_service_is_selected_without_config_device_id() {
        let config = crate::config::Config::default();
        let mut env = BTreeMap::new();
        env.insert(
            "HERDR_WORKSTATION_ID".to_owned(),
            "prod-real-runtime".to_owned(),
        );
        env.insert(
            "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
            "herdr-edge-prod-link-secret".to_owned(),
        );
        assert_eq!(
            prewarm_service(&config, Some(&env)).as_deref(),
            Some("herdr-edge-prod-link-secret")
        );
        assert_eq!(prewarm_service(&config, None), None);
    }

    #[test]
    fn protocol_rejects_unknown_ops_and_extra_fields() {
        let base = json!({"protocol":"herdr-keychain-helper","version":1,"op":"exec","service":"s","account":"a"});
        assert!(handle_request(&serde_json::to_vec(&base).unwrap()).is_err());
        let mut extra = base;
        extra["extra"] = json!(1);
        assert!(handle_request(&serde_json::to_vec(&extra).unwrap()).is_err());
    }

    #[test]
    fn protocol_rejects_oversized_input_before_json_parsing() {
        let oversized = vec![b' '; MAX_REQUEST_BYTES + 1];
        assert_eq!(
            handle_request(&oversized).unwrap_err(),
            "request exceeds limit"
        );
    }

    #[test]
    fn same_revision_preserves_installed_helper_bytes() {
        let dir = test_dir();
        let source = dir.join("candidate");
        std::fs::write(&source, b"first-helper-bytes").unwrap();
        preserve_or_install_from(&dir, &source).unwrap();
        assert_eq!(
            std::fs::read(helper_path(&dir)).unwrap(),
            b"first-helper-bytes"
        );

        std::fs::write(&source, b"new-runtime-bytes").unwrap();
        preserve_or_install_from(&dir, &source).unwrap();
        assert_eq!(
            std::fs::read(helper_path(&dir)).unwrap(),
            b"first-helper-bytes"
        );
        assert_eq!(installed_revision(&dir), Some(COMPAT_REVISION));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_helper_is_refused() {
        use std::os::unix::fs::symlink;

        let dir = test_dir();
        let source = dir.join("candidate");
        let target = dir.join("target");
        std::fs::write(&source, b"candidate").unwrap();
        std::fs::write(&target, b"target").unwrap();
        symlink(&target, helper_path(&dir)).unwrap();
        assert!(preserve_or_install_from(&dir, &source).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_metadata_install_removes_fresh_helper() {
        let dir = test_dir();
        let source = dir.join("candidate");
        std::fs::write(&source, b"candidate").unwrap();
        std::fs::create_dir(metadata_path(&dir)).unwrap();
        assert!(preserve_or_install_from(&dir, &source).is_err());
        assert!(!helper_path(&dir).exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
