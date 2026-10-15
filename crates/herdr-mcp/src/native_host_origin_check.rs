//! Cross-platform diagnostics for the Native Messaging host origin allowlist
//! (issue #626). `native-host status` reports whether the development
//! extension Chrome actually loaded is accepted by the installed manifests'
//! `allowed_origins`; when it is not, the extension cannot even launch the
//! host, so endpoint registration and every browser operation silently fail.
//!
//! The pure comparison lives here (ungated) so it is testable on every
//! platform; the macOS-only install lifecycle in `native_host_install` only
//! wires the pieces together.

use serde_json::{Value, json};

/// Stable machine code for the status warning emitted when the loaded
/// development extension id is missing from every installed allowlist.
pub(crate) const ALLOWLIST_WARNING_CODE: &str = "loaded_extension_id_not_in_allowed_origins";

/// The identity of the development extension that Chrome actually loaded,
/// derived from the maintainer's environment (never from a checkout or a
/// fixed generation path), plus the exact command that repairs the mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedDevExtensionIdentity {
    pub extension_id: String,
    pub fix_command: String,
}

/// Compare the development extension id Chrome actually loaded against the
/// union of installed Native Messaging allowlists.
///
/// Returns `Some(warning)` when the loaded id is valid but no allowlist entry
/// accepts it, and `None` when the loaded id is allowed (or not usable for
/// diagnostics). The warning always carries the loaded id, the full
/// allowlist, and the repair command so an operator can copy-paste the fix.
pub(crate) fn dev_origin_allowlist_warning(
    loaded_extension_id: &str,
    allowed_origins: &[String],
    fix_command: &str,
) -> Option<Value> {
    if !is_chromium_extension_id(loaded_extension_id) {
        return None;
    }
    if is_chromium_extension_origin_allowed(loaded_extension_id, allowed_origins) {
        return None;
    }
    let message = format!(
        "Chrome loaded development extension {loaded_extension_id}, but the installed native-host allowlists only accept [{}]; the extension cannot connect until the allowlist is repaired. Fix: {fix_command}",
        allowed_origins.join(", ")
    );
    Some(json!({
        "code": ALLOWLIST_WARNING_CODE,
        "loaded_extension_id": loaded_extension_id,
        "allowed_origins": allowed_origins,
        "fix_command": fix_command,
        "message": message,
    }))
}

/// Collect the union of `allowed_origins` across installed manifest views,
/// preserving first-seen order. Malformed manifest views contribute nothing.
pub(crate) fn collect_allowed_origins(manifest_views: &[Value]) -> Vec<String> {
    let mut origins = Vec::new();
    for view in manifest_views {
        let Some(view_origins) = view.get("allowed_origins").and_then(Value::as_array) else {
            continue;
        };
        for origin in view_origins.iter().filter_map(Value::as_str) {
            if !origins.iter().any(|seen: &String| seen == origin) {
                origins.push(origin.to_owned());
            }
        }
    }
    origins
}

fn is_chromium_extension_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))
}

fn is_chromium_extension_origin_allowed(extension_id: &str, allowed_origins: &[String]) -> bool {
    let expected = format!("chrome-extension://{extension_id}/");
    allowed_origins.iter().any(|origin| origin == &expected)
}

/// Resolve the development extension identity Chrome actually loaded from the
/// maintainer environment, mirroring the runtime's own origin precedence
/// (`HERDR_EXTENSION_ORIGIN`, then an id derived from the unpacked
/// `HERDR_EXTENSION_PATH` directory). Returns `Ok(None)` when neither variable
/// is set, and `Err` when a set variable is unusable (status then skips the
/// check instead of failing the whole report).
#[cfg(unix)]
pub(crate) fn loaded_dev_extension_identity(
    extension_origin_env: Option<&std::ffi::OsStr>,
    extension_path_env: Option<&std::ffi::OsStr>,
) -> Result<Option<LoadedDevExtensionIdentity>, String> {
    if let Some(origin) = extension_origin_env {
        let origin = origin
            .to_str()
            .ok_or_else(|| "extension_origin_not_utf8".to_owned())?
            .trim();
        if !origin.is_empty() {
            let origin = crate::native_host::validate_extension_origin(origin)?.to_owned();
            let extension_id = origin
                .strip_prefix("chrome-extension://")
                .and_then(|value| value.strip_suffix('/'))
                .ok_or_else(|| "extension_origin_unconfigured".to_owned())?
                .to_owned();
            let fix_command = shell_quoted_env_fix("HERDR_EXTENSION_ORIGIN", &origin);
            return Ok(Some(LoadedDevExtensionIdentity {
                extension_id,
                fix_command,
            }));
        }
    }
    if let Some(path) = extension_path_env.filter(|path| !path.is_empty()) {
        let path = std::path::PathBuf::from(path);
        let path = crate::native_host::require_extension_dir(
            &path,
            "HERDR_EXTENSION_PATH points to a missing or incomplete unpacked development extension directory",
        )?;
        let path = crate::native_host::lexical_absolute(&path)?;
        let extension_id = crate::native_host::chromium_id_for_path(&path)?;
        let fix_command = shell_quoted_env_fix("HERDR_EXTENSION_PATH", &path.to_string_lossy());
        return Ok(Some(LoadedDevExtensionIdentity {
            extension_id,
            fix_command,
        }));
    }
    Ok(None)
}

#[cfg(unix)]
fn shell_quoted_env_fix(variable: &str, value: &str) -> String {
    format!(
        "{variable}='{}' herdr-mcp native-host install",
        value.replace('\'', "'\\''")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD_DEV_ID: &str = "iflileolljfdjhlaopdpgjnpdjcidlae";
    const LOADED_DEV_ID: &str = "nfpapdlifleaejfjignibinjbahbcham";
    const FIX_COMMAND: &str =
        "HERDR_EXTENSION_PATH='/opt/herdr/extension' herdr-mcp native-host install";

    fn old_allowlist() -> Vec<String> {
        vec![format!("chrome-extension://{OLD_DEV_ID}/")]
    }

    #[test]
    fn warn_when_loaded_dev_extension_id_is_missing_from_allowlist() {
        let warning = dev_origin_allowlist_warning(LOADED_DEV_ID, &old_allowlist(), FIX_COMMAND)
            .expect("a loaded dev extension id missing from the allowlist must warn");
        assert_eq!(warning["code"], ALLOWLIST_WARNING_CODE);
        assert_eq!(warning["loaded_extension_id"], LOADED_DEV_ID);
        let origins = warning["allowed_origins"].as_array().unwrap();
        assert_eq!(origins.len(), 1);
        assert_eq!(origins[0], format!("chrome-extension://{OLD_DEV_ID}/"));
        assert_eq!(warning["fix_command"], FIX_COMMAND);
        let message = warning["message"].as_str().unwrap();
        assert!(
            message.contains(LOADED_DEV_ID),
            "the warning must name the actually loaded extension id: {message}"
        );
        assert!(
            message.contains(OLD_DEV_ID),
            "the warning must expose the current allowlist: {message}"
        );
        assert!(
            message.contains(FIX_COMMAND),
            "the warning must contain the copy-pasteable fix command: {message}"
        );
    }

    #[test]
    fn no_warning_when_loaded_dev_extension_id_is_allowed() {
        assert!(dev_origin_allowlist_warning(OLD_DEV_ID, &old_allowlist(), FIX_COMMAND).is_none());
        let multi = vec![
            format!("chrome-extension://{OLD_DEV_ID}/"),
            format!("chrome-extension://{LOADED_DEV_ID}/"),
        ];
        assert!(dev_origin_allowlist_warning(LOADED_DEV_ID, &multi, FIX_COMMAND).is_none());
    }

    #[test]
    fn warn_when_allowlist_is_empty() {
        let warning = dev_origin_allowlist_warning(LOADED_DEV_ID, &[], FIX_COMMAND)
            .expect("a loaded dev extension id with no allowlist at all must warn");
        assert_eq!(warning["code"], ALLOWLIST_WARNING_CODE);
        assert_eq!(warning["allowed_origins"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn invalid_loaded_extension_id_never_warns() {
        assert!(dev_origin_allowlist_warning("", &old_allowlist(), FIX_COMMAND).is_none());
        assert!(
            dev_origin_allowlist_warning("NOT-A-VALID-ID", &old_allowlist(), FIX_COMMAND).is_none()
        );
    }

    #[test]
    fn collect_allowed_origins_unions_manifest_views_in_first_seen_order() {
        let views = vec![
            json!({
                "path": "/manifest-a.json",
                "allowed_origins": [
                    format!("chrome-extension://{OLD_DEV_ID}/"),
                    format!("chrome-extension://{LOADED_DEV_ID}/"),
                ],
            }),
            json!({
                "path": "/manifest-b.json",
                "allowed_origins": [format!("chrome-extension://{OLD_DEV_ID}/")],
            }),
            json!({"path": "/manifest-invalid.json", "invalid": true}),
        ];
        let origins = collect_allowed_origins(&views);
        assert_eq!(
            origins,
            vec![
                format!("chrome-extension://{OLD_DEV_ID}/"),
                format!("chrome-extension://{LOADED_DEV_ID}/"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn loaded_identity_prefers_extension_origin_env_and_quotes_the_fix() {
        let identity = loaded_dev_extension_identity(
            Some(std::ffi::OsStr::new(&format!(
                "chrome-extension://{LOADED_DEV_ID}/"
            ))),
            Some(std::ffi::OsStr::new("/opt/herdr/extension")),
        )
        .unwrap()
        .expect("HERDR_EXTENSION_ORIGIN must resolve a loaded identity");
        assert_eq!(identity.extension_id, LOADED_DEV_ID);
        assert_eq!(
            identity.fix_command,
            format!(
                "HERDR_EXTENSION_ORIGIN='chrome-extension://{LOADED_DEV_ID}/' herdr-mcp native-host install"
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn loaded_identity_derives_extension_id_from_unpacked_path() {
        let root = std::env::temp_dir().join(format!(
            "herdr-origin-check-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let extension = root.join("extension");
        std::fs::create_dir_all(&extension).unwrap();
        std::fs::write(extension.join("manifest.json"), b"{}").unwrap();

        let identity = loaded_dev_extension_identity(None, Some(extension.as_os_str()))
            .unwrap()
            .expect("HERDR_EXTENSION_PATH must resolve a loaded identity");
        assert_eq!(
            identity.extension_id,
            crate::native_host::chromium_id_for_path(&extension).unwrap()
        );
        assert_eq!(
            identity.fix_command,
            format!(
                "HERDR_EXTENSION_PATH='{}' herdr-mcp native-host install",
                extension.display()
            )
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn loaded_identity_is_none_without_environment_input() {
        assert!(loaded_dev_extension_identity(None, None).unwrap().is_none());
        assert!(
            loaded_dev_extension_identity(
                Some(std::ffi::OsStr::new("")),
                Some(std::ffi::OsStr::new(""))
            )
            .unwrap()
            .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn loaded_identity_fails_closed_on_unusable_environment_input() {
        assert!(
            loaded_dev_extension_identity(Some(std::ffi::OsStr::new("not-an-origin")), None)
                .is_err()
        );
        let missing = std::env::temp_dir().join("herdr-origin-check-missing-extension");
        assert!(loaded_dev_extension_identity(None, Some(missing.as_os_str())).is_err());
    }
}
