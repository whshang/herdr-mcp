//! Installation lifecycle for the Rust Chrome Native Messaging host.
//!
//! Chromium always points at a stable wrapper under ~/.config/herdr-mcp/native.
//! The wrapper launches a colocated Rust binary copy. Future updater/supervisor
//! code can atomically replace that binary without rewriting browser manifests.

use crate::cli::NativeHostCommand;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const HOST_NAME: &str = "dev.herdr.mcp";
const WRAPPER_MARKER: &str = "# herdr-mcp rust native host v1";

#[cfg(unix)]
#[derive(Debug, Clone)]
struct InstallPaths {
    source_binary: PathBuf,
    runtime_binary: PathBuf,
    wrapper: PathBuf,
    extension_path: Option<PathBuf>,
    extension_id: String,
    extension_origin: String,
    targets: Vec<(PathBuf, bool)>,
}

pub fn run(command: NativeHostCommand) -> Result<ExitCode, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = command;
        return Err("native_host_install_currently_requires_macos".to_owned());
    }

    #[cfg(target_os = "macos")]
    {
        let paths = InstallPaths::discover(&command)?;
        let result = match command {
            NativeHostCommand::Install => install(&paths)?,
            NativeHostCommand::Status => status(&paths),
            NativeHostCommand::Uninstall => uninstall(&paths)?,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&result)
                .map_err(|error| format!("cannot encode native-host result: {error}"))?
        );
        Ok(if result.get("ok").and_then(Value::as_bool) == Some(true) {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        })
    }
}

#[cfg(unix)]
impl InstallPaths {
    fn discover(command: &NativeHostCommand) -> Result<Self, String> {
        let home = home_dir()?;
        let runtime_paths = crate::paths::RuntimePaths::discover()?;
        let source_binary = env::current_exe()
            .map_err(|error| format!("cannot locate current herdr-mcp binary: {error}"))?;
        let native_dir = runtime_paths.config_dir.join("native");
        let wrapper = native_dir.join("herdr-extension-host");
        let targets = install_targets(&home);

        if matches!(command, NativeHostCommand::Install) {
            if let Some(extension_origin) = find_registered_origin(&targets, &wrapper)? {
                let extension_id = extension_id_from_origin(&extension_origin)
                    .ok_or_else(|| "registered native-host origin is invalid".to_owned())?;
                let extension_path = crate::native_host::extension_path_for_install()
                    .ok()
                    .filter(|path| {
                        crate::native_host::chromium_id_for_path(path)
                            .ok()
                            .is_some_and(|id| id == extension_id)
                    });
                return Ok(Self {
                    source_binary,
                    runtime_binary: native_dir.join("herdr-mcp"),
                    wrapper,
                    extension_path,
                    extension_id,
                    extension_origin,
                    targets,
                });
            }
            let extension_path = crate::native_host::extension_path_for_install()?;
            return Self::for_layout(
                &extension_path,
                Some(extension_path.clone()),
                source_binary,
                native_dir,
                wrapper,
                targets,
            );
        }

        if let Some(extension_origin) = find_registered_origin(&targets, &wrapper)? {
            let extension_id = extension_id_from_origin(&extension_origin)
                .ok_or_else(|| "registered native-host origin is invalid".to_owned())?;
            let live_path = crate::native_host::extension_path_for_install().ok();
            let extension_path = live_path.filter(|path| {
                crate::native_host::chromium_id_for_path(path)
                    .ok()
                    .is_some_and(|id| id == extension_id)
            });
            return Ok(Self {
                source_binary,
                runtime_binary: native_dir.join("herdr-mcp"),
                wrapper,
                extension_path,
                extension_id,
                extension_origin,
                targets,
            });
        }

        let extension_path = crate::native_host::extension_path_for_install()?;
        Self::for_layout(
            &extension_path,
            Some(extension_path.clone()),
            source_binary,
            native_dir,
            wrapper,
            targets,
        )
    }

    #[cfg(test)]
    fn for_values(
        home: &Path,
        extension_path: &Path,
        source_binary: &Path,
    ) -> Result<Self, String> {
        let native_dir = home.join(".config").join("herdr-mcp").join("native");
        let wrapper = native_dir.join("herdr-extension-host");
        Self::for_layout(
            extension_path,
            Some(extension_path.to_path_buf()),
            source_binary.to_path_buf(),
            native_dir,
            wrapper,
            install_targets(home),
        )
    }

    fn for_layout(
        identity_path: &Path,
        extension_path: Option<PathBuf>,
        source_binary: PathBuf,
        native_dir: PathBuf,
        wrapper: PathBuf,
        targets: Vec<(PathBuf, bool)>,
    ) -> Result<Self, String> {
        let extension_id = crate::native_host::chromium_id_for_path(identity_path)?;
        let extension_origin = format!("chrome-extension://{extension_id}/");
        Ok(Self {
            source_binary,
            runtime_binary: native_dir.join("herdr-mcp"),
            wrapper,
            extension_path,
            extension_id,
            extension_origin,
            targets,
        })
    }
}

#[cfg(unix)]
fn install(paths: &InstallPaths) -> Result<Value, String> {
    let native_dir = paths
        .runtime_binary
        .parent()
        .ok_or_else(|| "native runtime path has no parent".to_owned())?;
    ensure_secure_dir(native_dir)?;
    atomic_copy_executable(&paths.source_binary, &paths.runtime_binary)?;

    let wrapper = wrapper_body(paths);
    atomic_write(&paths.wrapper, wrapper.as_bytes(), 0o700)?;

    let manifest = manifest_value(paths);
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("cannot encode native-host manifest: {error}"))?;
    let mut installed = Vec::new();
    for (target, always) in &paths.targets {
        let browser_dir = target.parent().unwrap_or(target);
        if !*always && !browser_dir.exists() {
            continue;
        }
        fs::create_dir_all(target).map_err(|error| {
            format!(
                "cannot create native messaging directory {}: {error}",
                target.display()
            )
        })?;
        let manifest_path = target.join(format!("{HOST_NAME}.json"));
        atomic_write(
            &manifest_path,
            &[manifest_bytes.as_slice(), b"\n"].concat(),
            0o600,
        )?;
        installed.push(manifest_path.to_string_lossy().into_owned());
    }
    let runtime_sha256 = file_sha256(&paths.runtime_binary)?;
    Ok(json!({
        "ok": !installed.is_empty(),
        "implementation": "rust",
        "host": HOST_NAME,
        "extension_id": paths.extension_id,
        "extension_path": paths.extension_path,
        "extension_origin": paths.extension_origin,
        "runtime_binary": paths.runtime_binary,
        "runtime_sha256": runtime_sha256,
        "wrapper": paths.wrapper,
        "installed": installed,
    }))
}

#[cfg(unix)]
fn status(paths: &InstallPaths) -> Value {
    let runtime_binary_ok = is_regular_executable(&paths.runtime_binary);
    let wrapper_ok = wrapper_is_rust(&paths.wrapper);
    let runtime_matches_current = runtime_binary_ok
        && file_sha256(&paths.source_binary)
            .ok()
            .is_some_and(|source| {
                file_sha256(&paths.runtime_binary).ok().as_deref() == Some(source.as_str())
            });
    let mut manifests = Vec::new();
    let mut owned_count = 0usize;
    for (target, _) in &paths.targets {
        let manifest_path = target.join(format!("{HOST_NAME}.json"));
        if !path_present(&manifest_path) {
            continue;
        }
        let view = manifest_status(&manifest_path, paths);
        if view.get("owned").and_then(Value::as_bool) == Some(true) {
            owned_count += 1;
        }
        manifests.push(view);
    }
    json!({
        "ok": runtime_binary_ok && wrapper_ok && owned_count > 0,
        "implementation": "rust",
        "host": HOST_NAME,
        "extension_id": paths.extension_id,
        "extension_path": paths.extension_path,
        "extension_origin": paths.extension_origin,
        "runtime_binary": paths.runtime_binary,
        "runtime_binary_ok": runtime_binary_ok,
        "runtime_matches_current": runtime_matches_current,
        "wrapper": paths.wrapper,
        "wrapper_ok": wrapper_ok,
        "owned_manifest_count": owned_count,
        "manifests": manifests,
    })
}

#[cfg(unix)]
fn uninstall(paths: &InstallPaths) -> Result<Value, String> {
    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    for (target, _) in &paths.targets {
        let manifest_path = target.join(format!("{HOST_NAME}.json"));
        if !path_present(&manifest_path) {
            continue;
        }
        let view = manifest_status(&manifest_path, paths);
        if view.get("owned").and_then(Value::as_bool) == Some(true) {
            fs::remove_file(&manifest_path).map_err(|error| {
                format!(
                    "cannot remove native-host manifest {}: {error}",
                    manifest_path.display()
                )
            })?;
            removed.push(manifest_path.to_string_lossy().into_owned());
        } else {
            skipped.push(json!({
                "path": manifest_path,
                "reason": "manifest_not_owned",
            }));
        }
    }

    let any_manifest_left = paths
        .targets
        .iter()
        .any(|(target, _)| path_present(&target.join(format!("{HOST_NAME}.json"))));
    let mut files_removed = Vec::new();
    if !any_manifest_left {
        if wrapper_is_rust(&paths.wrapper) && fs::remove_file(&paths.wrapper).is_ok() {
            files_removed.push(paths.wrapper.to_string_lossy().into_owned());
        }
        if is_regular_executable(&paths.runtime_binary)
            && fs::remove_file(&paths.runtime_binary).is_ok()
        {
            files_removed.push(paths.runtime_binary.to_string_lossy().into_owned());
        }
    }
    Ok(json!({
        "ok": skipped.is_empty(),
        "implementation": "rust",
        "host": HOST_NAME,
        "extension_id": paths.extension_id,
        "removed": removed,
        "skipped": skipped,
        "files_removed": files_removed,
    }))
}

#[cfg(unix)]
fn home_dir() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "cannot determine user home directory".to_owned())
}

#[cfg(unix)]
fn install_targets(home: &Path) -> Vec<(PathBuf, bool)> {
    let app_support = home.join("Library").join("Application Support");
    [
        (vec!["Google", "Chrome"], true),
        (vec!["Google", "Chrome Beta"], false),
        (vec!["Google", "Chrome Canary"], false),
        (vec!["Chromium"], false),
        (vec!["BraveSoftware", "Brave-Browser"], false),
        (vec!["Microsoft Edge"], false),
        (vec!["Citro Labs", "ego lite"], false),
    ]
    .into_iter()
    .map(|(parts, always)| {
        let mut target = app_support.clone();
        for part in parts {
            target.push(part);
        }
        target.push("NativeMessagingHosts");
        (target, always)
    })
    .collect()
}

#[cfg(unix)]
fn find_registered_origin(
    targets: &[(PathBuf, bool)],
    wrapper: &Path,
) -> Result<Option<String>, String> {
    let mut origins = BTreeSet::new();
    for (target, _) in targets {
        let path = target.join(format!("{HOST_NAME}.json"));
        if !path_present(&path) {
            continue;
        }
        let raw = match fs::read(&path) {
            Ok(raw) if raw.len() <= 64 * 1024 => raw,
            _ => continue,
        };
        let Ok(manifest) = serde_json::from_slice::<Value>(&raw) else {
            continue;
        };
        if manifest.get("name").and_then(Value::as_str) != Some(HOST_NAME)
            || manifest.get("type").and_then(Value::as_str) != Some("stdio")
            || manifest.get("path").and_then(Value::as_str) != wrapper.to_str()
        {
            continue;
        }
        if let Some(allowed) = manifest.get("allowed_origins").and_then(Value::as_array) {
            for origin in allowed.iter().filter_map(Value::as_str) {
                if extension_id_from_origin(origin).is_some() {
                    origins.insert(origin.to_owned());
                }
            }
        }
    }
    if origins.len() > 1 {
        return Err("native_host_registered_origins_conflict".to_owned());
    }
    Ok(origins.into_iter().next())
}

#[cfg(unix)]
fn extension_id_from_origin(origin: &str) -> Option<String> {
    let id = origin
        .strip_prefix("chrome-extension://")?
        .strip_suffix('/')?;
    (id.len() == 32 && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))).then(|| id.to_owned())
}

#[cfg(unix)]
fn wrapper_body(paths: &InstallPaths) -> String {
    format!(
        "#!/bin/sh\n{WRAPPER_MARKER}\nexport HERDR_EXTENSION_ORIGIN={}\nexec {} extension-host \"$@\"\n",
        shell_quote(&paths.extension_origin),
        shell_quote(paths.runtime_binary.to_string_lossy().as_ref()),
    )
}

#[cfg(unix)]
fn manifest_value(paths: &InstallPaths) -> Value {
    json!({
        "name": HOST_NAME,
        "description": "herdr-mcp local browser-extension IPC bridge",
        "path": paths.wrapper,
        "type": "stdio",
        "allowed_origins": [paths.extension_origin],
    })
}

#[cfg(unix)]
fn manifest_status(path: &Path, paths: &InstallPaths) -> Value {
    let raw = match fs::read(path) {
        Ok(raw) if raw.len() <= 64 * 1024 => raw,
        Ok(_) => return json!({"path": path, "invalid": true, "reason": "manifest_too_large"}),
        Err(error) => {
            return json!({
                "path": path,
                "invalid": true,
                "reason": "manifest_read_failed",
                "message": error.to_string(),
            });
        }
    };
    let manifest: Value = match serde_json::from_slice(&raw) {
        Ok(value) => value,
        Err(_) => return json!({"path": path, "invalid": true}),
    };
    let host_path = manifest.get("path").and_then(Value::as_str);
    let allowed = manifest
        .get("allowed_origins")
        .and_then(Value::as_array)
        .is_some_and(|origins| origins.iter().any(|value| value == &paths.extension_origin));
    let rust_wrapper = wrapper_is_rust(&paths.wrapper);
    let owned = rust_wrapper
        && manifest.get("name").and_then(Value::as_str) == Some(HOST_NAME)
        && manifest.get("type").and_then(Value::as_str) == Some("stdio")
        && host_path == paths.wrapper.to_str()
        && allowed;
    json!({
        "path": path,
        "host_path": host_path,
        "allowed": allowed,
        "rust_wrapper": rust_wrapper,
        "owned": owned,
    })
}

#[cfg(unix)]
fn ensure_secure_dir(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "native-host directory {} is not a real directory",
                path.display()
            ));
        }
    } else {
        fs::create_dir_all(path).map_err(|error| {
            format!(
                "cannot create native-host directory {}: {error}",
                path.display()
            )
        })?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "cannot secure native-host directory {}: {error}",
            path.display()
        )
    })
}

#[cfg(unix)]
fn atomic_copy_executable(source: &Path, target: &Path) -> Result<(), String> {
    if source == target
        || (source.canonicalize().ok().is_some()
            && source.canonicalize().ok() == target.canonicalize().ok())
    {
        return fs::set_permissions(target, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "cannot secure native-host binary {}: {error}",
                target.display()
            )
        });
    }
    reject_symlink_target(target)?;
    let temp = temporary_sibling(target);
    let result = (|| {
        let mut input = fs::File::open(source)
            .map_err(|error| format!("cannot open source binary {}: {error}", source.display()))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&temp)
            .map_err(|error| {
                format!(
                    "cannot create native-host binary temp {}: {error}",
                    temp.display()
                )
            })?;
        std::io::copy(&mut input, &mut output)
            .map_err(|error| format!("cannot copy native-host binary: {error}"))?;
        output
            .sync_all()
            .map_err(|error| format!("cannot sync native-host binary: {error}"))?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure native-host binary temp: {error}"))?;
        fs::rename(&temp, target).map_err(|error| {
            format!(
                "cannot activate native-host binary {}: {error}",
                target.display()
            )
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    reject_symlink_target(path)?;
    let temp = temporary_sibling(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)
            .map_err(|error| format!("cannot create temp file {}: {error}", temp.display()))?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot write temp file {}: {error}", temp.display()))?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))
            .map_err(|error| format!("cannot secure temp file {}: {error}", temp.display()))?;
        fs::rename(&temp, path)
            .map_err(|error| format!("cannot activate {}: {error}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
fn reject_symlink_target(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(format!(
            "native-host target {} must not be a symlink",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn temporary_sibling(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("native-host");
    path.with_file_name(format!(".{name}.tmp-{:x}-{nonce:x}", std::process::id()))
}

#[cfg(unix)]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn is_regular_executable(path: &Path) -> bool {
    fs::symlink_metadata(path).ok().is_some_and(|metadata| {
        !metadata.file_type().is_symlink()
            && metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
    })
}

#[cfg(unix)]
fn wrapper_is_rust(path: &Path) -> bool {
    if fs::symlink_metadata(path)
        .ok()
        .is_none_or(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return false;
    }
    fs::read_to_string(path)
        .ok()
        .filter(|content| content.len() <= 32 * 1024)
        .is_some_and(|content| content.contains(WRAPPER_MARKER))
}

#[cfg(unix)]
fn path_present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[cfg(unix)]
fn file_sha256(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("cannot hash {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    fn fixture() -> (PathBuf, InstallPaths) {
        let root = env::temp_dir().join(format!(
            "herdr-native-install-{}-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let home = root.join("home");
        let extension = root.join("repo").join("extension");
        let source = root.join("source-herdr-mcp");
        fs::create_dir_all(&extension).unwrap();
        fs::write(&source, b"rust-binary-fixture").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
        let paths = InstallPaths::for_values(&home, &extension, &source).unwrap();
        (root, paths)
    }

    #[test]
    fn install_status_uninstall_use_stable_wrapper_and_exact_origin() {
        let (root, paths) = fixture();
        let installed = install(&paths).unwrap();
        assert_eq!(installed["ok"], true);
        assert!(paths.runtime_binary.exists());
        assert!(paths.wrapper.exists());
        let wrapper = fs::read_to_string(&paths.wrapper).unwrap();
        assert!(wrapper.contains(WRAPPER_MARKER));
        assert!(wrapper.contains(&paths.extension_origin));
        assert!(!wrapper.contains("HERDR_MCP_TOKEN"));
        assert_eq!(
            fs::metadata(&paths.runtime_binary)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&paths.wrapper).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let stable_manifest = paths.targets[0].0.join(format!("{HOST_NAME}.json"));
        let manifest: Value = serde_json::from_slice(&fs::read(&stable_manifest).unwrap()).unwrap();
        assert_eq!(manifest["path"], paths.wrapper.to_string_lossy().as_ref());
        assert_eq!(manifest["allowed_origins"], json!([paths.extension_origin]));
        assert_eq!(
            fs::metadata(&stable_manifest).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let view = status(&paths);
        assert_eq!(view["ok"], true);
        assert_eq!(view["owned_manifest_count"], 1);

        let removed = uninstall(&paths).unwrap();
        assert_eq!(removed["ok"], true);
        assert!(!stable_manifest.exists());
        assert!(!paths.wrapper.exists());
        assert!(!paths.runtime_binary.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uninstall_preserves_manifest_that_is_not_owned() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        let stable_manifest = paths.targets[0].0.join(format!("{HOST_NAME}.json"));
        fs::write(
            &stable_manifest,
            br#"{"name":"dev.herdr.mcp","type":"stdio","path":"/tmp/other","allowed_origins":[]}"#,
        )
        .unwrap();
        let removed = uninstall(&paths).unwrap();
        assert_eq!(removed["ok"], false);
        assert!(stable_manifest.exists());
        assert!(paths.wrapper.exists());
        assert!(paths.runtime_binary.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uninstall_never_removes_node_compatibility_host_manifest() {
        let (root, paths) = fixture();
        install(&paths).unwrap();
        let stable_manifest = paths.targets[0].0.join(format!("{HOST_NAME}.json"));
        fs::write(&paths.wrapper, "#!/bin/sh\nexec node compat-host \"$@\"\n").unwrap();
        fs::set_permissions(&paths.wrapper, fs::Permissions::from_mode(0o700)).unwrap();

        let removed = uninstall(&paths).unwrap();
        assert_eq!(removed["ok"], false);
        assert!(stable_manifest.exists());
        assert!(paths.wrapper.exists());
        assert!(paths.runtime_binary.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_refuses_symlinked_native_target() {
        let (root, paths) = fixture();
        let native_dir = paths.runtime_binary.parent().unwrap();
        fs::create_dir_all(native_dir).unwrap();
        let elsewhere = root.join("elsewhere");
        fs::write(&elsewhere, b"leave-me").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &paths.runtime_binary).unwrap();
        let error = install(&paths).unwrap_err();
        assert!(error.contains("must not be a symlink"));
        assert_eq!(fs::read(&elsewhere).unwrap(), b"leave-me");
        fs::remove_dir_all(root).unwrap();
    }
}
