//! Minimal non-paid macOS TCC broker.
//!
//! macOS TCC (Transparency, Consent, and Control) grants privacy-protected
//! resource access (e.g. `~/Documents`) to a *stable code identity*. A
//! Developer-ID-signed, notarized binary keeps a stable identity across
//! upgrades; an unsigned binary that is replaced on every release does not —
//! each new binary is a new identity and loses any previously granted TCC
//! permission.
//!
//! This module provides a **stable local broker**: a single fixed binary
//! installed at `<config_dir>/tcc-broker/herdr-mcp-broker`, separate from the
//! rotating `runtime/generations/rust-<sha256>/` runtime generations. Because
//! the broker path and binary are never rewritten by service install / update
//! apply, the broker keeps one stable identity across runtime upgrades. The
//! broker is a one-shot JSON-over-stdin/stdout process that dispatches only
//! a strict allowlist of bounded fs/git operations to the existing security
//! gates. Arbitrary shell execution is intentionally outside this broker; on
//! macOS protected roots, exec sessions use a dedicated Herdr pane instead.
//!
//! Routing is opt-in via `HERDR_MCP_TCC_BROKER=1`; the default MCP path stays
//! direct in-process execution. This is a feasibility layer for a non-paid
//! macOS TCC story — it does not by itself grant TCC permission, and it does
//! not claim to replace a Developer ID / notarization.

use crate::cli::TccBrokerCommand;
use crate::fs_mutation;
use crate::fs_patch;
use crate::fs_tools;
use crate::git_tools;
use serde_json::{Value, json};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Broker protocol version. Bump only on a breaking wire change.
pub const PROTOCOL_VERSION: u32 = 1;
/// Compatibility revision of the long-lived broker implementation. This is
/// deliberately independent of the rotating `herdr-mcp` runtime bytes. Bump
/// only when the installed broker itself must be replaced.
pub const BROKER_COMPAT_REVISION: u32 = 1;
/// Stable signing identifier reserved for separately signed broker candidates.
pub const BROKER_SIGNING_IDENTIFIER: &str = "cc.agentforme.herdr.tcc-broker";
/// Brokers installed before revision metadata existed are the v0.4.2 revision.
const LEGACY_BROKER_COMPAT_REVISION: u32 = 1;
const BROKER_METADATA_SCHEMA: u32 = 1;
/// Maximum accepted request size (bytes) on stdin.
pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
/// Maximum response size (bytes) written to stdout.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Hard wall-clock budget for a single broker request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const BROKER_CHILD_ENV: &str = "HERDR_MCP_TCC_BROKER_CHILD";

/// The 8 focused operations the broker may dispatch. This is the complete
/// allowlist — no arbitrary operation names are accepted.
const ALLOWED_OPS: [&str; 8] = [
    "fs_read", "fs_list", "fs_grep", "fs_image", "fs_edit", "fs_write", "fs_patch", "git",
];

/// Stable broker path, independent of `runtime/generations`.
pub fn broker_path(config_dir: &Path) -> PathBuf {
    config_dir.join("tcc-broker").join("herdr-mcp-broker")
}

pub fn broker_metadata_path(config_dir: &Path) -> PathBuf {
    config_dir.join("tcc-broker").join("metadata.json")
}

fn broker_candidate_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("HERDR_MCP_TCC_BROKER_CANDIDATE") {
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(format!(
                "TCC broker candidate is not a regular file: {}",
                path.display()
            ));
        }
        return Ok(path);
    }
    std::env::current_exe().map_err(|error| format!("cannot resolve current executable: {error}"))
}

fn write_broker_metadata(config_dir: &Path) -> Result<(), String> {
    let value = json!({
        "schema_version": BROKER_METADATA_SCHEMA,
        "compat_revision": BROKER_COMPAT_REVISION,
        "preferred_signing_identifier": BROKER_SIGNING_IDENTIFIER,
    });
    let bytes = serde_json::to_vec_pretty(&value)
        .map_err(|error| format!("cannot encode TCC broker metadata: {error}"))?;
    atomic_write(&broker_metadata_path(config_dir), &bytes, 0o600)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstalledCompatState {
    LegacyRevision,
    Revision(u32),
    Invalid(String),
}

fn parse_broker_metadata(bytes: &[u8]) -> Result<u32, String> {
    let value = serde_json::from_slice::<Value>(bytes)
        .map_err(|error| format!("metadata JSON is invalid: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "metadata must be a JSON object".to_owned())?;
    let schema = object
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "metadata schema_version must be an integer".to_owned())?;
    if schema != u64::from(BROKER_METADATA_SCHEMA) {
        return Err(format!(
            "unsupported metadata schema_version {schema} (expected {BROKER_METADATA_SCHEMA})"
        ));
    }
    let revision = object
        .get("compat_revision")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| "metadata compat_revision must be a u32 integer".to_owned())?;
    let identifier = object
        .get("preferred_signing_identifier")
        .and_then(Value::as_str)
        .ok_or_else(|| "metadata preferred_signing_identifier must be a string".to_owned())?;
    if identifier != BROKER_SIGNING_IDENTIFIER {
        return Err(format!(
            "unexpected metadata signing identifier '{identifier}' (expected '{BROKER_SIGNING_IDENTIFIER}')"
        ));
    }
    Ok(revision)
}

pub fn installed_compat_state(config_dir: &Path) -> Option<InstalledCompatState> {
    status(&broker_path(config_dir))?;
    let metadata_path = broker_metadata_path(config_dir);
    let metadata = match std::fs::symlink_metadata(&metadata_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(InstalledCompatState::LegacyRevision);
        }
        Err(error) => {
            return Some(InstalledCompatState::Invalid(format!(
                "cannot inspect {}: {error}",
                metadata_path.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Some(InstalledCompatState::Invalid(format!(
            "{} must be a regular file and not a symlink",
            metadata_path.display()
        )));
    }
    match std::fs::read(&metadata_path) {
        Ok(bytes) => Some(match parse_broker_metadata(&bytes) {
            Ok(revision) => InstalledCompatState::Revision(revision),
            Err(detail) => InstalledCompatState::Invalid(detail),
        }),
        Err(error) => Some(InstalledCompatState::Invalid(format!(
            "cannot read {}: {error}",
            metadata_path.display()
        ))),
    }
}

/// Compatibility revision of an installed broker. Only a genuinely missing
/// metadata file inherits the first v0.4.2 legacy revision. Present-but-invalid
/// metadata returns `None` so callers fail closed.
pub fn installed_compat_revision(config_dir: &Path) -> Option<u32> {
    match installed_compat_state(config_dir)? {
        InstalledCompatState::LegacyRevision => Some(LEGACY_BROKER_COMPAT_REVISION),
        InstalledCompatState::Revision(revision) => Some(revision),
        InstalledCompatState::Invalid(_) => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerCodeIdentity {
    pub mode: String,
    pub identifier: Option<String>,
    pub team: Option<String>,
    designated_requirement: Option<String>,
}

impl BrokerCodeIdentity {
    fn stable_requirement(&self) -> Option<&str> {
        let requirement = self.designated_requirement.as_deref()?;
        if self.mode == "adhoc" || requirement.trim_start().starts_with("cdhash ") {
            return None;
        }
        Some(requirement)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerUpgradeStatus {
    pub installed_revision: Option<u32>,
    pub candidate_revision: u32,
    pub update_available: bool,
    pub identity_compatible: Option<bool>,
    pub update_requires_reauthorization: bool,
    pub metadata_invalid: Option<String>,
    pub installed_identity: Option<BrokerCodeIdentity>,
}

pub fn inspect_code_identity(path: &Path) -> BrokerCodeIdentity {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/usr/bin/codesign")
            .args(["-dvvv", "--requirements", "-"])
            .arg(path)
            .output();
        let Ok(output) = output else {
            return BrokerCodeIdentity {
                mode: "unverifiable".to_owned(),
                identifier: None,
                team: None,
                designated_requirement: None,
            };
        };
        let text = String::from_utf8_lossy(&output.stderr);
        let identifier = line_value(&text, "Identifier=");
        let team = line_value(&text, "TeamIdentifier=")
            .filter(|value| value != "not set" && !value.is_empty());
        let designated_requirement = line_value(&text, "# designated => ");
        let mode = if text.lines().any(|line| line.trim() == "Signature=adhoc") {
            "adhoc"
        } else if team.is_some() {
            "certificate"
        } else if identifier.is_some() {
            "signed-no-team"
        } else {
            "unsigned-or-unverifiable"
        };
        BrokerCodeIdentity {
            mode: mode.to_owned(),
            identifier,
            team,
            designated_requirement,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        BrokerCodeIdentity {
            mode: "not-applicable".to_owned(),
            identifier: None,
            team: None,
            designated_requirement: None,
        }
    }
}

#[cfg(target_os = "macos")]
fn line_value(text: &str, prefix: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix(prefix).map(str::trim))
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
}

fn candidate_satisfies_requirement(candidate: &Path, requirement: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        Command::new("/usr/bin/codesign")
            .arg("--verify")
            .arg("--strict")
            .arg(format!("-R={requirement}"))
            .arg(candidate)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (candidate, requirement);
        false
    }
}

fn replacement_identity_compatible(installed: &Path, candidate: &Path) -> bool {
    let identity = inspect_code_identity(installed);
    identity
        .stable_requirement()
        .map(|requirement| candidate_satisfies_requirement(candidate, requirement))
        .unwrap_or(false)
}

pub fn upgrade_status(config_dir: &Path) -> BrokerUpgradeStatus {
    let installed = broker_path(config_dir);
    let installed_state = installed_compat_state(config_dir);
    let metadata_invalid = installed_state.as_ref().and_then(|state| match state {
        InstalledCompatState::Invalid(detail) => Some(detail.clone()),
        _ => None,
    });
    let installed_revision = installed_state.as_ref().and_then(|state| match state {
        InstalledCompatState::LegacyRevision => Some(LEGACY_BROKER_COMPAT_REVISION),
        InstalledCompatState::Revision(revision) => Some(*revision),
        InstalledCompatState::Invalid(_) => None,
    });
    let candidate_revision = BROKER_COMPAT_REVISION;
    let update_available = metadata_invalid.is_none()
        && installed_revision
            .map(|revision| revision != candidate_revision)
            .unwrap_or(false);
    let installed_identity = status(&installed).map(|_| inspect_code_identity(&installed));
    let identity_compatible = if metadata_invalid.is_some() || installed_revision.is_none() {
        None
    } else if !update_available {
        Some(true)
    } else {
        broker_candidate_path()
            .ok()
            .map(|candidate| replacement_identity_compatible(&installed, &candidate))
    };
    let update_requires_reauthorization =
        metadata_invalid.is_some() || (update_available && identity_compatible != Some(true));
    BrokerUpgradeStatus {
        installed_revision,
        candidate_revision,
        update_available,
        identity_compatible,
        update_requires_reauthorization,
        metadata_invalid,
        installed_identity,
    }
}

pub fn doctor_line(config_dir: &Path) -> String {
    let upgrade = upgrade_status(config_dir);
    let identity = upgrade.installed_identity.as_ref();
    let revision = upgrade
        .installed_revision
        .map(|value| value.to_string())
        .unwrap_or_else(|| {
            if upgrade.metadata_invalid.is_some() {
                "invalid".to_owned()
            } else {
                "none".to_owned()
            }
        });
    format!(
        "LAYER tcc-broker-identity mode={} identifier={} team={} installed_revision={} candidate_revision={} metadata={} identity_compatible={} update_requires_reauthorization={}",
        identity
            .map(|value| value.mode.as_str())
            .unwrap_or("missing"),
        identity
            .and_then(|value| value.identifier.as_deref())
            .unwrap_or("unknown"),
        identity
            .and_then(|value| value.team.as_deref())
            .unwrap_or("none"),
        revision,
        upgrade.candidate_revision,
        if upgrade.metadata_invalid.is_some() {
            "invalid"
        } else {
            "ok"
        },
        upgrade
            .identity_compatible
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_owned()),
        upgrade.update_requires_reauthorization,
    )
}

pub fn run_broker_once() -> ExitCode {
    let request = match read_bounded_stdin(MAX_REQUEST_BYTES) {
        Ok(bytes) => bytes,
        Err(message) => {
            write_response(&broker_error("read_failed", &message));
            return ExitCode::from(2);
        }
    };
    let response = match handle_request_bytes(&request) {
        Ok(value) => value,
        Err(message) => broker_error("dispatch_failed", &message),
    };
    if write_response(&response) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(3)
    }
}

/// Run the user-facing `tcc-broker` CLI subcommand.
pub fn run_cli(command: TccBrokerCommand) -> Result<ExitCode, String> {
    let config_dir = crate::paths::RuntimePaths::discover()?.config_dir;
    match command {
        TccBrokerCommand::Install { force } => {
            install(&config_dir, force)?;
            println!(
                "tcc-broker installed at {}",
                broker_path(&config_dir).display()
            );
            Ok(ExitCode::SUCCESS)
        }
        TccBrokerCommand::Status => {
            let path = broker_path(&config_dir);
            match status(&path) {
                Some(info) => {
                    let upgrade = upgrade_status(&config_dir);
                    println!("tcc-broker: installed");
                    println!("path: {}", path.display());
                    println!("sha256: {}", info.sha256);
                    println!("bytes: {}", info.bytes);
                    println!(
                        "installed_revision: {}",
                        upgrade
                            .installed_revision
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| if upgrade.metadata_invalid.is_some() {
                                "invalid".to_owned()
                            } else {
                                "none".to_owned()
                            })
                    );
                    println!("candidate_revision: {}", upgrade.candidate_revision);
                    println!(
                        "metadata_status: {}",
                        if upgrade.metadata_invalid.is_some() {
                            "invalid"
                        } else {
                            "ok"
                        }
                    );
                    if let Some(detail) = upgrade.metadata_invalid.as_deref() {
                        println!("metadata_error: {detail}");
                        println!(
                            "metadata_hint: repair the broker metadata or perform an explicit one-time broker identity migration and reauthorization"
                        );
                    }
                    println!("update_available: {}", upgrade.update_available);
                    println!(
                        "identity_compatible: {}",
                        upgrade
                            .identity_compatible
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "unknown".to_owned())
                    );
                    println!(
                        "update_requires_reauthorization: {}",
                        upgrade.update_requires_reauthorization
                    );
                    if let Some(identity) = upgrade.installed_identity {
                        println!("identity_mode: {}", identity.mode);
                        println!(
                            "signing_identifier: {}",
                            identity.identifier.as_deref().unwrap_or("unknown")
                        );
                        println!("team: {}", identity.team.as_deref().unwrap_or("none"));
                    }
                    println!("preferred_signing_identifier: {BROKER_SIGNING_IDENTIFIER}");
                    Ok(ExitCode::SUCCESS)
                }
                None => {
                    println!("tcc-broker: not installed");
                    println!("expected: {}", path.display());
                    Ok(ExitCode::SUCCESS)
                }
            }
        }
        TccBrokerCommand::Uninstall => {
            let path = broker_path(&config_dir);
            if uninstall_broker(&config_dir)? {
                println!("tcc-broker removed: {}", path.display());
            } else {
                println!("tcc-broker not installed: {}", path.display());
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Install the broker as an immutable copy of the current executable at the
/// stable broker path. Runtime byte changes do not imply a broker update: an
/// installed broker with the same compatibility revision is always preserved.
/// `force` only permits a real revision upgrade whose candidate satisfies the
/// installed broker's stable designated requirement.
pub fn install(config_dir: &Path, force: bool) -> Result<(), String> {
    let source = broker_candidate_path()?;
    let target = broker_path(config_dir);
    let parent = target
        .parent()
        .ok_or_else(|| "broker path has no parent directory".to_owned())?;
    ensure_secure_dir(parent)?;
    if let Ok(metadata) = std::fs::symlink_metadata(&target)
        && metadata.file_type().is_symlink()
    {
        return Err(format!("broker {} must not be a symlink", target.display()));
    }
    let source_bytes = std::fs::read(&source).map_err(|error| {
        format!(
            "cannot read current executable {}: {error}",
            source.display()
        )
    })?;
    if target.exists() {
        if let Some(InstalledCompatState::Invalid(detail)) = installed_compat_state(config_dir) {
            return Err(format!(
                "refusing to modify TCC broker {} because its metadata is invalid: {detail}; repair the metadata or explicitly migrate the broker identity and reauthorize macOS access",
                target.display()
            ));
        }
        let existing = std::fs::read(&target).map_err(|error| {
            format!("cannot read existing broker {}: {error}", target.display())
        })?;
        if existing == source_bytes {
            // Identical bytes already installed — preserve the stable identity.
            if !broker_metadata_path(config_dir).is_file() {
                write_broker_metadata(config_dir)?;
            }
            return Ok(());
        }
        let installed_revision = installed_compat_revision(config_dir).ok_or_else(|| {
            "installed TCC broker compatibility revision is unavailable".to_owned()
        })?;
        if installed_revision == BROKER_COMPAT_REVISION {
            // The broker implementation did not change. Never replace a stable
            // TCC client merely because the surrounding runtime was rebuilt.
            // A different-byte pre-metadata broker is preserved as legacy
            // evidence rather than being silently blessed with new metadata.
            return Ok(());
        }
        if !force {
            return Err(format!(
                "TCC broker revision {installed_revision} is installed and revision {BROKER_COMPAT_REVISION} is available; run `herdr-mcp permissions setup --upgrade-broker` for an identity-checked upgrade"
            ));
        }
        if !replacement_identity_compatible(&target, &source) {
            return Err(format!(
                "refusing to replace TCC broker {} because the candidate does not satisfy the installed stable code requirement; this upgrade requires a one-time TCC identity migration and reauthorization (explicitly uninstall the broker first if that migration is intended)",
                target.display()
            ));
        }
    }
    atomic_write(&target, &source_bytes, 0o700)?;
    write_broker_metadata(config_dir)
}

/// Remove the stable broker binary. Full uninstall may call this; ordinary
/// runtime/generation rotation must not. Returns whether a file was removed.
pub fn uninstall_broker(config_dir: &Path) -> Result<bool, String> {
    let path = broker_path(config_dir);
    let metadata_path = broker_metadata_path(config_dir);
    if let Ok(metadata) = std::fs::symlink_metadata(&path)
        && metadata.file_type().is_symlink()
    {
        return Err(format!("broker {} must not be a symlink", path.display()));
    }
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path)
        .map_err(|error| format!("cannot remove {}: {error}", path.display()))?;
    if metadata_path.exists() {
        std::fs::remove_file(&metadata_path)
            .map_err(|error| format!("cannot remove {}: {error}", metadata_path.display()))?;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(true)
}

/// Read-only status of an installed broker.
pub struct BrokerStatus {
    pub sha256: String,
    pub bytes: u64,
}

pub fn status(path: &Path) -> Option<BrokerStatus> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let bytes = metadata.len();
    let sha256 = file_sha256(path).ok()?;
    Some(BrokerStatus { sha256, bytes })
}

/// One-line status for `herdr-mcp status` output.
pub fn status_line(config_dir: &Path) -> String {
    let path = broker_path(config_dir);
    match status(&path) {
        Some(info) => {
            let upgrade = upgrade_status(config_dir);
            let revision = upgrade
                .installed_revision
                .map(|value| value.to_string())
                .unwrap_or_else(|| {
                    if upgrade.metadata_invalid.is_some() {
                        "invalid".to_owned()
                    } else {
                        "none".to_owned()
                    }
                });
            format!(
                "installed at {} (sha256 {}, revision {}, metadata {}, identity {}, reauth={})",
                path.display(),
                &info.sha256[..16.min(info.sha256.len())],
                revision,
                if upgrade.metadata_invalid.is_some() {
                    "invalid"
                } else {
                    "ok"
                },
                upgrade
                    .installed_identity
                    .as_ref()
                    .map(|identity| identity.mode.as_str())
                    .unwrap_or("unknown"),
                upgrade.update_requires_reauthorization,
            )
        }
        None => format!("not installed (expected {})", path.display()),
    }
}

/// Parse and validate a broker request. Strict: exact top-level keys, object
/// `snapshot`/`args`, protocol/version, and an allowlisted operation.
fn parse_request(request: &Value) -> Result<Request, String> {
    let object = request
        .as_object()
        .ok_or_else(|| "request must be a JSON object".to_owned())?;
    let mut keys = object.keys().collect::<Vec<_>>();
    keys.sort();
    if keys != ["args", "op", "protocol", "snapshot", "version"] {
        let got = keys
            .iter()
            .map(|key| key.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "request must have exactly protocol, version, op, snapshot, args (got {got})"
        ));
    }
    let protocol = object
        .get("protocol")
        .and_then(Value::as_str)
        .ok_or_else(|| "protocol must be a string".to_owned())?;
    if protocol != "herdr-tcc-broker" {
        return Err(format!("unsupported protocol '{protocol}'"));
    }
    let version = object
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "version must be an integer".to_owned())?;
    if version != PROTOCOL_VERSION as u64 {
        return Err(format!(
            "unsupported protocol version {version} (expected {PROTOCOL_VERSION})"
        ));
    }
    let op = object
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| "op must be a string".to_owned())?;
    if !ALLOWED_OPS.contains(&op) {
        return Err(format!(
            "unknown operation '{op}' (allowed: {})",
            ALLOWED_OPS.join(", ")
        ));
    }
    let snapshot = object
        .get("snapshot")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| "snapshot must be an object".to_owned())?;
    let args = object
        .get("args")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| "args must be an object".to_owned())?;
    Ok(Request {
        op: op.to_owned(),
        snapshot: Value::Object(snapshot),
        args: Value::Object(args),
    })
}

struct Request {
    op: String,
    snapshot: Value,
    args: Value,
}

/// Handle a raw request byte slice, returning the response JSON value. This is
/// the in-process entry point used by both the one-shot broker mode and tests.
pub fn handle_request_bytes(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(format!(
            "request exceeds {MAX_REQUEST_BYTES} bytes (got {})",
            bytes.len()
        ));
    }
    let request: Value =
        serde_json::from_slice(bytes).map_err(|error| format!("invalid request JSON: {error}"))?;
    let parsed = parse_request(&request)?;
    Ok(dispatch(&parsed))
}

/// Dispatch a validated request to the existing bounded fs/git functions.
/// Response shapes match the direct MCP tool results exactly.
fn dispatch(request: &Request) -> Value {
    match request.op.as_str() {
        "fs_read" => fs_tools::read(&request.snapshot, &request.args),
        "fs_list" => fs_tools::list(&request.snapshot, &request.args),
        "fs_grep" => fs_tools::grep(&request.snapshot, &request.args),
        "fs_image" => match fs_tools::image(&request.snapshot, &request.args) {
            Ok(image) => json!({
                "ok": true,
                "kind": "image",
                "meta": image.meta,
                "data": image.data,
                "mime_type": image.mime_type,
            }),
            Err(error) => error,
        },
        "fs_edit" => fs_mutation::edit(&request.snapshot, &request.args),
        "fs_write" => fs_mutation::write(&request.snapshot, &request.args),
        "fs_patch" => fs_patch::apply(&request.snapshot, &request.args),
        "git" => git_tools::run(&request.snapshot, &request.args),
        // Unreachable: parse_request enforces the allowlist.
        _ => broker_error("unknown_operation", &request.op),
    }
}

/// Reconstruct the MCP `herdr_fs_image` tool result from a broker image
/// response. Validates the MIME type against the fixed set the image tool can
/// produce; never silently defaults malformed broker output.
pub fn image_tool_result_from_broker(value: &Value) -> Result<Value, String> {
    if value.get("ok").and_then(Value::as_bool) != Some(true)
        || value.get("kind").and_then(Value::as_str) != Some("image")
    {
        return Err("broker fs_image response is not a successful image".to_owned());
    }
    let meta = value
        .get("meta")
        .cloned()
        .ok_or_else(|| "broker fs_image response missing meta".to_owned())?;
    let data = value
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| "broker fs_image response missing data".to_owned())?
        .to_owned();
    let mime_type = value
        .get("mime_type")
        .and_then(Value::as_str)
        .ok_or_else(|| "broker fs_image response missing mime_type".to_owned())?;
    let mime_type = match mime_type {
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" => mime_type,
        other => {
            return Err(format!(
                "broker fs_image returned invalid mime_type '{other}'"
            ));
        }
    };
    let text = serde_json::to_string(&meta).unwrap_or_else(|_| "{}".to_owned());
    Ok(json!({
        "content": [
            {"type": "text", "text": text},
            {"type": "image", "data": data, "mimeType": mime_type}
        ]
    }))
}

/// Route a single focused fs/git MCP tool through the broker when
/// `HERDR_MCP_TCC_BROKER=1` is set. Returns `None` when broker routing is not
/// enabled (caller falls back to direct execution). The broker is spawned as a
/// one-shot child of the current executable's stable broker path.
pub fn route_fs_git(op: &str, snapshot: &Value, args: &Value) -> Option<Result<Value, String>> {
    if std::env::var("HERDR_MCP_TCC_BROKER").ok().as_deref() != Some("1") {
        return None;
    }
    Some(run_stable_broker(op, snapshot, args))
}

/// Run the already-supported `git status` broker operation even when a direct
/// CLI/runtime path did not inherit the service's broker-routing flag. This
/// keeps protected project discovery on the long-lived broker identity without
/// adding a wire operation or changing the broker compatibility revision.
pub(crate) fn git_status_via_stable_broker(snapshot: &Value, root: &Path) -> Result<Value, String> {
    run_stable_broker(
        "git",
        snapshot,
        &json!({
            "root": root.to_string_lossy(),
            "action": "status",
            "max_bytes": 65_536,
        }),
    )
}

/// Fence recursive protected-path routing when the installed one-shot broker
/// itself executes Git/filesystem security checks.
pub(crate) fn is_broker_child_process() -> bool {
    std::env::var(BROKER_CHILD_ENV).ok().as_deref() == Some("1")
}

fn run_stable_broker(op: &str, snapshot: &Value, args: &Value) -> Result<Value, String> {
    let config_dir = crate::paths::RuntimePaths::discover()?.config_dir;
    let broker = broker_path(&config_dir);
    if !broker.is_file() {
        return Err(format!(
            "tcc-broker not installed at {} (run `herdr-mcp tcc-broker install`)",
            broker.display()
        ));
    }
    let request = json!({
        "protocol": "herdr-tcc-broker",
        "version": PROTOCOL_VERSION,
        "op": op,
        "snapshot": snapshot,
        "args": args,
    });
    let request_bytes = match serde_json::to_vec(&request) {
        Ok(bytes) => bytes,
        Err(message) => return Err(format!("cannot serialize broker request: {message}")),
    };
    if request_bytes.len() > MAX_REQUEST_BYTES {
        return Err(format!("broker request exceeds {MAX_REQUEST_BYTES} bytes"));
    }
    run_broker_child(&broker, &request_bytes)
}

fn run_broker_child(broker: &Path, request_bytes: &[u8]) -> Result<Value, String> {
    use crate::child_process;
    use std::process::{Command, Stdio};

    // Fail closed: never spawn a broker path that is a symlink or not a
    // regular file.
    let metadata = std::fs::symlink_metadata(broker)
        .map_err(|error| format!("cannot inspect broker {}: {error}", broker.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "broker {} must be a regular file (not a symlink)",
            broker.display()
        ));
    }

    let mut command = Command::new(broker);
    command
        .arg("__tcc-broker")
        .env(BROKER_CHILD_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    child_process::configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot spawn broker {}: {error}", broker.display()))?;
    let _registration = child_process::register_owned_child("tcc-broker", &child);
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "broker stdin unavailable".to_owned())?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "broker stdout unavailable".to_owned())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "broker stderr unavailable".to_owned())?;

    let request_owned = request_bytes.to_vec();
    let write_handle = std::thread::spawn(move || {
        let _ = stdin.write_all(&request_owned);
    });
    // Read stdout and stderr concurrently, each bounded to avoid unbounded
    // memory growth from a misbehaving broker.
    let stdout_handle = std::thread::spawn(move || read_capped(&mut stdout, MAX_RESPONSE_BYTES));
    let stderr_handle = std::thread::spawn(move || read_capped(&mut stderr, 64 * 1024));

    let status = child_process::wait_bounded(&mut child, REQUEST_TIMEOUT)
        .map_err(|error| format!("broker wait failed: {error}"))?;
    let _ = write_handle.join();
    let out = stdout_handle
        .join()
        .map_err(|_| "broker stdout reader panicked".to_owned())?;
    let err = stderr_handle
        .join()
        .map_err(|_| "broker stderr reader panicked".to_owned())?;

    let Some(status) = status else {
        // wait_bounded already terminated and reaped the child on timeout.
        return Err("broker request timed out".to_owned());
    };
    if !status.success() {
        let detail = String::from_utf8_lossy(&err).trim().to_owned();
        return Err(format!(
            "broker exited with {}: {}",
            status.code().unwrap_or(-1),
            if detail.is_empty() {
                "no stderr".to_owned()
            } else {
                detail
            }
        ));
    }
    if out.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "broker response exceeds {MAX_RESPONSE_BYTES} bytes"
        ));
    }
    serde_json::from_slice(&out).map_err(|error| format!("invalid broker response JSON: {error}"))
}

/// Read up to `max_bytes` from a reader, retaining the first `max_bytes` and
/// draining the rest so the child never blocks on a full pipe.
fn read_capped(reader: &mut impl Read, max_bytes: usize) -> Vec<u8> {
    let mut retained = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if retained.len() < max_bytes {
                    let keep = count.min(max_bytes - retained.len());
                    retained.extend_from_slice(&buffer[..keep]);
                }
            }
            Err(_) => break,
        }
    }
    retained
}

fn read_bounded_stdin(max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut stdin = std::io::stdin();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = stdin
            .read(&mut buffer)
            .map_err(|error| format!("cannot read stdin: {error}"))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > max_bytes {
            return Err(format!("request exceeds {max_bytes} bytes"));
        }
    }
    Ok(bytes)
}

fn write_response(value: &Value) -> bool {
    let bytes = match serde_json::to_vec(value) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    if bytes.len() > MAX_RESPONSE_BYTES {
        return false;
    }
    let mut stdout = std::io::stdout();
    stdout.write_all(&bytes).is_ok() && stdout.flush().is_ok()
}

fn broker_error(code: &str, message: &str) -> Value {
    json!({
        "ok": false,
        "code": code,
        "message": message,
    })
}

fn ensure_secure_dir(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(format!("{} must not be a symlink", path.display()));
    }
    std::fs::create_dir_all(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure {}: {error}", path.display()))?;
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(format!("{} must not be a symlink", path.display()));
    }
    let temp = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("herdr-mcp-broker"),
        std::process::id(),
        now_ms_i64()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(mode);
    let mut file = options
        .open(&temp)
        .map_err(|error| format!("cannot create {}: {error}", temp.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("cannot write {}: {error}", temp.display()))?;
    file.sync_all()
        .map_err(|error| format!("cannot sync {}: {error}", temp.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot chmod {}: {error}", temp.display()))?;
    std::fs::rename(&temp, path)
        .map_err(|error| format!("cannot replace {}: {error}", path.display()))?;
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{} must be a regular file", path.display()));
    }
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
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

fn now_ms_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::fs;
    use std::process::Command;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-tcc-broker-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_snapshot(root: &Path) -> Value {
        json!({
            "workspaces": [
                {
                    "id": "ws-1",
                    "name": "test",
                    "panes": [
                        {
                            "pane_id": "pane-1",
                            "cwd": root.to_string_lossy(),
                            "agent_status": "idle"
                        }
                    ]
                }
            ],
            "panes": [
                {
                    "pane_id": "pane-1",
                    "cwd": root.to_string_lossy(),
                    "agent_status": "idle"
                }
            ],
            "agents": []
        })
    }

    fn init_git_repo(root: &Path) {
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn request(op: &str, snapshot: &Value, args: Value) -> Value {
        json!({
            "protocol": "herdr-tcc-broker",
            "version": PROTOCOL_VERSION,
            "op": op,
            "snapshot": snapshot,
            "args": args,
        })
    }

    #[test]
    fn broker_path_is_outside_runtime_generations() {
        let config = PathBuf::from("/tmp/herdr-config");
        let path = broker_path(&config);
        assert_eq!(path, config.join("tcc-broker").join("herdr-mcp-broker"));
        let text = path.to_string_lossy();
        assert!(!text.contains("runtime"));
        assert!(!text.contains("generations"));
        assert!(!text.contains("rust-"));
    }

    #[test]
    fn same_broker_revision_preserves_existing_identity_across_runtime_byte_changes() {
        let dir = temp_dir("install");
        let config = dir.join("config");
        let target = broker_path(&config);
        // A v0.4.2 pre-metadata broker is compatibility revision 1. Runtime
        // rebuilds can have different bytes without implying a broker upgrade.
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"existing-broker-bytes").unwrap();
        let result = install(&config, false);
        assert!(result.is_ok());
        assert_eq!(fs::read(&target).unwrap(), b"existing-broker-bytes");
        assert_eq!(
            installed_compat_revision(&config),
            Some(BROKER_COMPAT_REVISION)
        );
        assert!(!broker_metadata_path(&config).is_file());

        // Even an explicit setup pass cannot replace the identity when the
        // broker implementation revision did not change.
        let result = install(&config, true);
        assert!(result.is_ok());
        assert_eq!(fs::read(&target).unwrap(), b"existing-broker-bytes");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compatibility_revision_controls_update_availability() {
        let dir = temp_dir("revision");
        let config = dir.join("config");
        let target = broker_path(&config);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"stable-broker").unwrap();
        fs::write(
            broker_metadata_path(&config),
            br#"{"schema_version":1,"compat_revision":0,"preferred_signing_identifier":"cc.agentforme.herdr.tcc-broker"}"#,
        )
        .unwrap();
        let status = upgrade_status(&config);
        assert_eq!(status.installed_revision, Some(0));
        assert_eq!(status.candidate_revision, BROKER_COMPAT_REVISION);
        assert!(status.update_available);
        // Synthetic/legacy bytes have no stable certificate requirement, so a
        // revision replacement must require explicit reauthorization.
        assert!(status.update_requires_reauthorization);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_broker_metadata_fails_closed_without_rewriting_identity() {
        let dir = temp_dir("invalid-metadata");
        let config = dir.join("config");
        let target = broker_path(&config);
        let metadata = broker_metadata_path(&config);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"stable-broker").unwrap();
        fs::write(&metadata, b"{not-json").unwrap();

        assert_eq!(installed_compat_revision(&config), None);
        let status = upgrade_status(&config);
        assert_eq!(status.installed_revision, None);
        assert!(status.metadata_invalid.is_some());
        assert_eq!(status.identity_compatible, None);
        assert!(!status.update_available);
        assert!(status.update_requires_reauthorization);

        for force in [false, true] {
            let error = install(&config, force).unwrap_err();
            assert!(error.contains("metadata is invalid"));
            assert!(error.contains("explicitly migrate"));
            assert_eq!(fs::read(&target).unwrap(), b"stable-broker");
            assert_eq!(fs::read(&metadata).unwrap(), b"{not-json");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn broker_metadata_validation_rejects_wrong_schema_revision_and_identifier() {
        let cases: &[&[u8]] = &[
            br#"{"schema_version":99,"compat_revision":1,"preferred_signing_identifier":"cc.agentforme.herdr.tcc-broker"}"#,
            br#"{"schema_version":1,"compat_revision":4294967296,"preferred_signing_identifier":"cc.agentforme.herdr.tcc-broker"}"#,
            br#"{"schema_version":1,"compat_revision":1,"preferred_signing_identifier":"unexpected.identifier"}"#,
        ];
        for metadata in cases {
            assert!(parse_broker_metadata(metadata).is_err());
        }
    }

    #[test]
    fn adhoc_cdhash_requirement_is_not_stable_for_broker_replacement() {
        let adhoc = BrokerCodeIdentity {
            mode: "adhoc".to_owned(),
            identifier: Some("legacy.herdr".to_owned()),
            team: None,
            designated_requirement: Some("cdhash H\"deadbeef\"".to_owned()),
        };
        assert_eq!(adhoc.stable_requirement(), None);
        let stable = BrokerCodeIdentity {
            mode: "certificate".to_owned(),
            identifier: Some(BROKER_SIGNING_IDENTIFIER.to_owned()),
            team: Some("ABCDE12345".to_owned()),
            designated_requirement: Some(
                "identifier \"cc.agentforme.herdr.tcc-broker\" and anchor apple generic".to_owned(),
            ),
        };
        assert!(stable.stable_requirement().is_some());
    }

    #[test]
    fn rejects_malformed_and_oversized_requests() {
        // Not an object.
        assert!(handle_request_bytes(b"[]").is_err());
        // Unknown top-level key.
        let bad = json!({"protocol": "herdr-tcc-broker", "version": 1, "op": "fs_read", "snapshot": {}, "args": {}, "extra": 1});
        assert!(handle_request_bytes(&serde_json::to_vec(&bad).unwrap()).is_err());
        // Unknown operation.
        let bad_op = request("rm_rf", &json!({}), json!({}));
        assert!(handle_request_bytes(&serde_json::to_vec(&bad_op).unwrap()).is_err());
        // Wrong protocol.
        let bad_proto =
            json!({"protocol": "other", "version": 1, "op": "fs_read", "snapshot": {}, "args": {}});
        assert!(handle_request_bytes(&serde_json::to_vec(&bad_proto).unwrap()).is_err());
        // Wrong version.
        let bad_ver = json!({"protocol": "herdr-tcc-broker", "version": 99, "op": "fs_read", "snapshot": {}, "args": {}});
        assert!(handle_request_bytes(&serde_json::to_vec(&bad_ver).unwrap()).is_err());
        // Oversized.
        let big = vec![b' '; MAX_REQUEST_BYTES + 1];
        assert!(handle_request_bytes(&big).is_err());
    }

    #[test]
    fn dispatches_real_git_repo_and_rejects_unsafe_paths() {
        let dir = temp_dir("dispatch");
        let root = dir.join("repo");
        fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);
        fs::write(root.join("hello.txt"), "hello world\n").unwrap();
        let snapshot = make_snapshot(&root);

        // fs_read inside root.
        let read = request(
            "fs_read",
            &snapshot,
            json!({"path": root.join("hello.txt")}),
        );
        let out = handle_request_bytes(&serde_json::to_vec(&read).unwrap()).unwrap();
        assert_eq!(out["ok"].as_bool(), Some(true));
        assert!(out["content"].as_str().unwrap().contains("hello world"));

        // fs_list.
        let list = request("fs_list", &snapshot, json!({"path": root}));
        let out = handle_request_bytes(&serde_json::to_vec(&list).unwrap()).unwrap();
        assert_eq!(out["ok"].as_bool(), Some(true));

        // git status.
        let git = request("git", &snapshot, json!({"root": root, "action": "status"}));
        let out = handle_request_bytes(&serde_json::to_vec(&git).unwrap()).unwrap();
        assert_eq!(out["ok"].as_bool(), Some(true));

        // Outside managed roots.
        let outside = request("fs_read", &snapshot, json!({"path": "/etc/hosts"}));
        let out = handle_request_bytes(&serde_json::to_vec(&outside).unwrap()).unwrap();
        assert_eq!(out["ok"].as_bool(), Some(false));
        assert_eq!(out["reason"].as_str(), Some("outside_managed_roots"));

        // Secret path denied.
        let secret = request(
            "fs_read",
            &snapshot,
            json!({"path": root.join(".git/config")}),
        );
        let out = handle_request_bytes(&serde_json::to_vec(&secret).unwrap()).unwrap();
        assert_eq!(out["ok"].as_bool(), Some(false));
        assert_eq!(out["reason"].as_str(), Some("secret_path_denied"));

        // Symlink escape: a symlink inside root pointing outside.
        let outside_file = dir.join("outside.txt");
        fs::write(&outside_file, "secret\n").unwrap();
        let link = root.join("escape.txt");
        std::os::unix::fs::symlink(&outside_file, &link).unwrap();
        let symlink = request("fs_read", &snapshot, json!({"path": link}));
        let out = handle_request_bytes(&serde_json::to_vec(&symlink).unwrap()).unwrap();
        assert_eq!(out["ok"].as_bool(), Some(false));
        assert_eq!(out["reason"].as_str(), Some("symlink_escape"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_response_round_trips_through_broker_shape() {
        let dir = temp_dir("image");
        let root = dir.join("repo");
        fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);
        // 1x1 red PNG.
        let png = STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==")
            .unwrap();
        fs::write(root.join("pixel.png"), &png).unwrap();
        let snapshot = make_snapshot(&root);
        let img = request(
            "fs_image",
            &snapshot,
            json!({"path": root.join("pixel.png")}),
        );
        let out = handle_request_bytes(&serde_json::to_vec(&img).unwrap()).unwrap();
        assert_eq!(out["ok"].as_bool(), Some(true));
        assert_eq!(out["kind"].as_str(), Some("image"));
        assert_eq!(out["mime_type"].as_str(), Some("image/png"));
        let tool = image_tool_result_from_broker(&out).unwrap();
        assert_eq!(tool["content"][1]["mimeType"].as_str(), Some("image/png"));
        assert!(tool["content"][1]["data"].as_str().is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_tool_result_rejects_invalid_mime() {
        let bad =
            json!({"ok": true, "kind": "image", "meta": {}, "data": "x", "mime_type": "text/html"});
        assert!(image_tool_result_from_broker(&bad).is_err());
        let not_image =
            json!({"ok": true, "kind": "other", "meta": {}, "data": "x", "mime_type": "image/png"});
        assert!(image_tool_result_from_broker(&not_image).is_err());
    }

    #[test]
    fn contract_has_18_tools() {
        assert_eq!(crate::contract::tool_names().len(), 18);
    }
}
