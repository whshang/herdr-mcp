use crate::fs_security::{denied_secret_path, path_within};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const MAX_TEXT_BYTES: usize = 256 * 1024;

pub fn read(params: &Value) -> Value {
    let path = match required_string(params, "path") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let max_bytes = match optional_usize(params, "max_bytes", 1, MAX_TEXT_BYTES) {
        Ok(value) => value.unwrap_or(MAX_TEXT_BYTES),
        Err(error) => return error,
    };
    let path = match safe_home_path(&path, false) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let metadata = match fs::metadata(&path) {
        Ok(value) if value.is_file() => value,
        Ok(_) => {
            return fail(
                "path_not_file",
                "text transfer source must be a regular file",
                &path,
            );
        }
        Err(error) => return io_fail("read_failed", &path, error),
    };
    if metadata.len() > max_bytes as u64 {
        return json!({
            "ok": false,
            "code": "text_too_large",
            "path": path,
            "bytes": metadata.len(),
            "max_bytes": max_bytes,
        });
    }
    let bytes = match fs::read(&path) {
        Ok(value) => value,
        Err(error) => return io_fail("read_failed", &path, error),
    };
    let content = match String::from_utf8(bytes) {
        Ok(value) => value,
        Err(_) => {
            return fail(
                "text_utf8_required",
                "text transfer accepts UTF-8 text only",
                &path,
            );
        }
    };
    if sensitive_text(&path, &content) {
        return fail(
            "sensitive_content_denied",
            "text transfer refuses detected credentials or private keys",
            &path,
        );
    }
    let digest = sha256_hex(content.as_bytes());
    json!({
        "ok": true,
        "path": path,
        "bytes": content.len(),
        "sha256": digest,
        "content": content,
    })
}

pub fn write(params: &Value) -> Value {
    let path = match required_string(params, "path") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let content = match required_string(params, "content") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let expected_sha256 = match required_string(params, "sha256") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let overwrite = match optional_bool(params, "overwrite") {
        Ok(value) => value.unwrap_or(false),
        Err(error) => return error,
    };
    let backup = match optional_bool(params, "backup") {
        Ok(value) => value.unwrap_or(true),
        Err(error) => return error,
    };
    if sensitive_text(Path::new(&path), &content) {
        return fail(
            "sensitive_content_denied",
            "text transfer refuses detected credentials or private keys",
            Path::new(&path),
        );
    }
    if content.len() > MAX_TEXT_BYTES {
        return json!({
            "ok": false,
            "code": "text_too_large",
            "bytes": content.len(),
            "max_bytes": MAX_TEXT_BYTES,
        });
    }
    let actual_sha256 = sha256_hex(content.as_bytes());
    if expected_sha256 != actual_sha256 {
        return json!({
            "ok": false,
            "code": "sha256_mismatch",
            "expected_sha256": expected_sha256,
            "actual_sha256": actual_sha256,
        });
    }
    let path = match safe_home_path(&path, true) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let existing = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return fail(
                "symlink_denied",
                "text transfer target must not be a symlink",
                &path,
            );
        }
        Ok(metadata) if !metadata.is_file() => {
            return fail(
                "path_not_file",
                "text transfer target must be a regular file",
                &path,
            );
        }
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return io_fail("target_inspect_failed", &path, error),
    };
    if existing.is_some() && !overwrite {
        return fail(
            "target_exists",
            "set overwrite=true to replace an existing text file",
            &path,
        );
    }

    let backup_path = if existing.is_some() && backup {
        let candidate = backup_name(&path);
        if let Err(error) = fs::copy(&path, &candidate) {
            return io_fail("backup_failed", &candidate, error);
        }
        Some(candidate)
    } else {
        None
    };

    let parent = match path.parent() {
        Some(value) => value,
        None => {
            return fail(
                "invalid_path",
                "text transfer target has no parent directory",
                &path,
            );
        }
    };
    let temp = parent.join(format!(
        ".{}.herdr-transfer-{}-{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("text"),
        std::process::id(),
        now_ms()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = match options.open(&temp) {
        Ok(value) => value,
        Err(error) => return io_fail("temp_create_failed", &temp, error),
    };
    if let Err(error) = file
        .write_all(content.as_bytes())
        .and_then(|_| file.sync_all())
    {
        let _ = fs::remove_file(&temp);
        return io_fail("write_failed", &temp, error);
    }
    if let Some(metadata) = existing
        && let Err(error) = fs::set_permissions(&temp, metadata.permissions())
    {
        let _ = fs::remove_file(&temp);
        return io_fail("permissions_failed", &temp, error);
    }
    if let Err(error) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return io_fail("replace_failed", &path, error);
    }
    json!({
        "ok": true,
        "path": path,
        "bytes": content.len(),
        "sha256": actual_sha256,
        "backup_path": backup_path,
    })
}

fn safe_home_path(input: &str, allow_missing: bool) -> Result<PathBuf, Value> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| json!({"ok": false, "code": "home_unavailable"}))?;
    let home =
        fs::canonicalize(&home).map_err(|error| io_fail("home_resolution_failed", &home, error))?;
    let candidate = PathBuf::from(input);
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        home.join(candidate)
    };
    if denied_secret_path(&candidate) {
        return Err(fail(
            "secret_path_denied",
            "text transfer refuses secret-like paths",
            &candidate,
        ));
    }
    match fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(fail(
                "symlink_denied",
                "text transfer refuses a symlink as the final path component",
                &candidate,
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && allow_missing => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(io_fail("path_resolution_failed", &candidate, error));
        }
        Err(error) => return Err(io_fail("path_inspect_failed", &candidate, error)),
    }
    if !allow_missing || candidate.exists() {
        let real = fs::canonicalize(&candidate)
            .map_err(|error| io_fail("path_resolution_failed", &candidate, error))?;
        if !path_within(&home, &real) {
            return Err(fail(
                "path_outside_home",
                "text transfer is limited to HOME",
                &candidate,
            ));
        }
        return Ok(real);
    }
    let parent = candidate.parent().ok_or_else(|| {
        fail(
            "invalid_path",
            "text transfer target has no parent directory",
            &candidate,
        )
    })?;
    let parent_real = fs::canonicalize(parent)
        .map_err(|error| io_fail("parent_resolution_failed", parent, error))?;
    if !path_within(&home, &parent_real) {
        return Err(fail(
            "path_outside_home",
            "text transfer is limited to HOME",
            &candidate,
        ));
    }
    let name = candidate.file_name().ok_or_else(|| {
        fail(
            "invalid_path",
            "text transfer target has no file name",
            &candidate,
        )
    })?;
    Ok(parent_real.join(name))
}

fn backup_name(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("text");
    path.with_file_name(format!("{file_name}.herdr-backup-{}", now_ms()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sensitive_text(path: &Path, content: &str) -> bool {
    if content.contains("-----BEGIN PRIVATE KEY-----")
        || content.contains("-----BEGIN OPENSSH PRIVATE KEY-----")
        || content.contains("-----BEGIN RSA PRIVATE KEY-----")
        || contains_secret_assignment(content)
    {
        return true;
    }
    if path.extension().and_then(|value| value.to_str()) != Some("json") {
        return false;
    }
    serde_json::from_str::<Value>(content)
        .ok()
        .is_some_and(|value| json_contains_secret(&value))
}

fn contains_secret_assignment(content: &str) -> bool {
    const KEYS: &[&str] = &[
        "api_key",
        "apikey",
        "access_token",
        "refresh_token",
        "client_secret",
        "token",
        "password",
    ];
    content.lines().any(|line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            return false;
        }
        let lower = trimmed.to_ascii_lowercase();
        KEYS.iter().any(|key| {
            lower
                .strip_prefix(key)
                .and_then(|rest| rest.strip_prefix('='))
                .is_some_and(|value| !value.trim().is_empty())
        }) || lower.starts_with("authorization: bearer ")
    })
}

fn json_contains_secret(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            let sensitive_key = key.contains("apikey")
                || key.contains("api_key")
                || key.contains("token")
                || key.contains("secret")
                || key.contains("credential")
                || key == "authorization"
                || key == "password";
            (sensitive_key && !value.is_null() && value != "") || json_contains_secret(value)
        }),
        Value::Array(items) => items.iter().any(json_contains_secret),
        _ => false,
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn required_string(params: &Value, key: &str) -> Result<String, Value> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| json!({"ok": false, "code": "invalid_params", "message": format!("{key} must be a non-empty string")}))
}

fn optional_bool(params: &Value, key: &str) -> Result<Option<bool>, Value> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(
            json!({"ok": false, "code": "invalid_params", "message": format!("{key} must be a boolean")}),
        ),
    }
}

fn optional_usize(
    params: &Value,
    key: &str,
    min: usize,
    max: usize,
) -> Result<Option<usize>, Value> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_u64().and_then(|value| usize::try_from(value).ok()) {
            Some(value) if value >= min && value <= max => Ok(Some(value)),
            _ => Err(
                json!({"ok": false, "code": "invalid_params", "message": format!("{key} must be an integer in {min}..={max}")}),
            ),
        },
    }
}

fn fail(code: &str, message: &str, path: &Path) -> Value {
    json!({"ok": false, "code": code, "message": message, "path": path})
}

fn io_fail(code: &str, path: &Path, error: std::io::Error) -> Value {
    json!({"ok": false, "code": code, "message": error.to_string(), "path": path})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_transfer_round_trip_preserves_backup_and_checksum() {
        let _guard = crate::test_env::lock();
        let previous = std::env::var_os("HOME");
        let root = std::env::temp_dir().join(format!("herdr-text-transfer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        unsafe { std::env::set_var("HOME", &root) };

        fs::write(root.join("source.txt"), "新版本\n").unwrap();
        fs::write(root.join("target.txt"), "旧版本\n").unwrap();
        let read_result = read(&json!({"path": "source.txt"}));
        assert_eq!(read_result["ok"], true);
        let result = write(&json!({
            "path": "target.txt",
            "content": read_result["content"],
            "sha256": read_result["sha256"],
            "overwrite": true,
        }));
        assert_eq!(result["ok"], true);
        assert_eq!(
            fs::read_to_string(root.join("target.txt")).unwrap(),
            "新版本\n"
        );
        let backup = PathBuf::from(result["backup_path"].as_str().unwrap());
        assert_eq!(fs::read_to_string(backup).unwrap(), "旧版本\n");

        unsafe {
            match previous {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn text_transfer_rejects_integrity_and_secret_failures() {
        let result = write(&json!({
            "path": "target.txt",
            "content": "hello",
            "sha256": "wrong",
        }));
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "sha256_mismatch");

        let _guard = crate::test_env::lock();
        let previous = std::env::var_os("HOME");
        let root =
            std::env::temp_dir().join(format!("herdr-text-sensitive-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        unsafe { std::env::set_var("HOME", &root) };
        fs::write(root.join("models.json"), r#"{"apiKey":"secret-value"}"#).unwrap();
        let result = read(&json!({"path": "models.json"}));
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "sensitive_content_denied");
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn text_transfer_rejects_final_symlink_inside_home() {
        use std::os::unix::fs::symlink;

        let _guard = crate::test_env::lock();
        let previous = std::env::var_os("HOME");
        let root = std::env::temp_dir().join(format!("herdr-text-symlink-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        unsafe { std::env::set_var("HOME", &root) };
        fs::write(root.join("real.txt"), "safe\n").unwrap();
        symlink(root.join("real.txt"), root.join("alias.txt")).unwrap();

        let result = read(&json!({"path": "alias.txt"}));
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "symlink_denied");

        let replacement = "replacement\n";
        let result = write(&json!({
            "path": "alias.txt",
            "content": replacement,
            "sha256": sha256_hex(replacement.as_bytes()),
            "overwrite": true,
        }));
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "symlink_denied");
        assert_eq!(fs::read_to_string(root.join("real.txt")).unwrap(), "safe\n");

        unsafe {
            match previous {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn text_transfer_rejects_obvious_secret_assignment_in_plain_text() {
        let content = "password=fixture-value\n";
        let result = write(&json!({
            "path": "notes.txt",
            "content": content,
            "sha256": sha256_hex(content.as_bytes()),
        }));
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "sensitive_content_denied");
    }
}
