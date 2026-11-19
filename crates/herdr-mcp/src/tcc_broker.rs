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
//! broker's MCP mode is a one-shot JSON-over-stdin/stdout process that
//! dispatches only a strict allowlist of bounded fs/git operations to the
//! existing security gates. Revision 2 also adds one private macOS-only Herdr
//! host mode: it may launch only the canonical `herdr server` dependency and
//! remain its parent so pane/Agent descendants inherit this stable TCC
//! responsibility. It is not a generic shell or arbitrary-command broker.
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
///
/// Revision 3 adds the read-only `git` identity action, so a runtime that
/// needs exact source provenance must ask the operator to upgrade the stable
/// broker instead of falling back to a direct Git read.
///
/// Revision 4 serves vcs-less operational roots (a directory exactly proven by
/// a live workspace/pane cwd) through the same validated-root boundary, so a
/// protected non-Git project like `~/Documents/<project>` is readable without
/// the rotating runtime ever becoming the TCC client. A revision-3 broker
/// still applies the Git-only boundary and reports `outside_managed_roots`, so
/// the operator must upgrade the broker with
/// `herdr-mcp permissions setup --upgrade-broker` for that case.
/// Bumped to 5 for the #844 Documents alias subcommands
/// (`__documents-alias-probe` / `__documents-alias-ensure`).
///
/// The bump marks an installed broker as out of date, but it does NOT make
/// [`install`] replace it: `install` returns early when the revisions are equal,
/// errors when they differ and `force` is false, and refuses an ad-hoc/cdhash
/// broker even with `force` because the candidate cannot satisfy the installed
/// stable code requirement. The migration that really swaps the bytes is
/// [`migrate_for_explicit_reauthorization`] (behind
/// `herdr-mcp permissions setup --upgrade-broker`), and the committed `update apply`
/// path now runs it once so the upgrade installs the new subcommands before a
/// request needs them. That costs one broker re-authorization on that upgrade,
/// which is the same at-most-one-prompt-per-upgrade budget #844 targets.
pub const BROKER_COMPAT_REVISION: u32 = 6; // 6: adds __tcc-broker-stream
/// First broker revision that implements the read-only `git` identity action.
#[cfg(target_os = "macos")]
pub(crate) const GIT_IDENTITY_MIN_COMPAT_REVISION: u32 = 3;
/// First broker revision that can serve vcs-less operational roots through the
/// shared validated-root boundary. This value is also used by the portable
/// denial annotation path, so it must remain available on every target.
pub(crate) const OPERATIONAL_ROOT_MIN_COMPAT_REVISION: u32 = 4;
/// First broker revision that can be a stable TCC parent for `herdr server`.
#[cfg(any(target_os = "macos", test))]
pub const HERDR_HOST_MIN_COMPAT_REVISION: u32 = 2;
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

fn previous_broker_path(config_dir: &Path) -> PathBuf {
    config_dir
        .join("tcc-broker")
        .join("herdr-mcp-broker.previous")
}

fn previous_broker_metadata_path(config_dir: &Path) -> PathBuf {
    config_dir.join("tcc-broker").join("metadata.previous.json")
}

/// Whether the installed stable broker contains the private Herdr-host
/// capability. Revision 1 remains valid for the original fs/git broker path;
/// callers must keep their pre-host fallback until an explicit broker upgrade
/// has installed revision 2 or newer.
#[cfg(any(target_os = "macos", test))]
pub fn installed_supports_herdr_host(config_dir: &Path) -> bool {
    status(&broker_path(config_dir)).is_some()
        && installed_compat_revision(config_dir)
            .is_some_and(|revision| revision >= HERDR_HOST_MIN_COMPAT_REVISION)
}

#[cfg(target_os = "macos")]
fn trusted_herdr_binary() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is required for the stable Herdr TCC host".to_owned())?;
    [
        home.join(".local/bin/herdr"),
        PathBuf::from("/opt/homebrew/bin/herdr"),
        PathBuf::from("/usr/local/bin/herdr"),
    ]
    .into_iter()
    .find(|path| {
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
    .ok_or_else(|| {
        "cannot locate a trusted standard Herdr binary for the stable TCC host".to_owned()
    })
}

#[cfg(target_os = "macos")]
fn same_canonical_file(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn herdr_host_can_wrap(config_dir: &Path, herdr_binary: &Path) -> bool {
    if !installed_supports_herdr_host(config_dir) {
        return false;
    }
    let Ok(trusted) = trusted_herdr_binary() else {
        return false;
    };
    if !same_canonical_file(herdr_binary, &trusted) {
        return false;
    }
    matches!(
        run_disclaimed_documents_probe(&broker_path(config_dir), Duration::from_secs(2)),
        Ok(Some(status)) if status.success()
    )
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

/// Run the persistent streaming broker mode (`__tcc-broker-stream`).
///
/// The process stays resident and serves newline-delimited JSON frames on
/// stdin/stdout. The parent sends one `hello` frame first; the broker answers
/// `hello_ok` and then processes one request frame per line:
/// `{op, seq, args, snapshot?}` -> `{seq, result}` where `result` is the same
/// envelope the one-shot protocol returns. A request carries `snapshot` only
/// when the parent-side routing fingerprint changed; the broker keeps the
/// last delivered snapshot and rejects a request that has neither an inline
/// snapshot nor a stored one. All filesystem I/O, canonicalization and the
/// symlink boundary stay inside this stable disclaimed process — the parent
/// only forwards frames.
///
/// Protocol errors produce a `protocol_error` result frame instead of closing
/// the stream, so one malformed request cannot wedge the resident worker. EOF
/// on stdin ends the loop cleanly. An installed broker older than the
/// streaming revision simply has no `__tcc-broker-stream` mode: it exits
/// immediately, which the parent classifies as "streaming unsupported" and
/// falls back to the one-shot protocol.
pub fn run_broker_stream() -> ExitCode {
    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = std::io::BufWriter::new(stdout.lock());
    match broker_stream_loop(&mut reader, &mut writer) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(4),
    }
}

const STREAM_PROTOCOL: &str = "herdr-tcc-broker-stream";
const STREAM_PROTOCOL_VERSION: u32 = 1;

/// Write one stream frame (single JSON line) respecting `MAX_RESPONSE_BYTES`.
/// An oversized value is replaced by a bounded error frame that preserves the
/// `{seq, result}` envelope — without `seq` the parent flags a protocol
/// desync and kills a healthy worker (P2, review 6997783732).
fn write_stream_frame(writer: &mut impl Write, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("cannot serialize stream frame: {error}"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        let fallback = serde_json::to_vec(&json!({
            "seq": value.get("seq").cloned().unwrap_or(Value::Null),
            "result": broker_error(
                "response_too_large",
                "broker response exceeds the bounded stream frame size",
            ),
        }))
        .map_err(|error| format!("cannot serialize fallback frame: {error}"))?;
        writer
            .write_all(&fallback)
            .and_then(|()| writer.write_all(b"\n"))
            .and_then(|()| writer.flush())
            .map_err(|error| format!("cannot write stream frame: {error}"))?;
        return Ok(());
    }
    writer
        .write_all(&bytes)
        .and_then(|()| writer.write_all(b"\n"))
        .and_then(|()| writer.flush())
        .map_err(|error| format!("cannot write stream frame: {error}"))
}

fn broker_stream_loop(
    reader: &mut impl std::io::BufRead,
    writer: &mut impl Write,
) -> Result<(), String> {
    // First frame must be the handshake. Anything else (an old broker would
    // never reach this code — the mode itself is missing) is a protocol error
    // surfaced to the parent so it can fall back to the one-shot protocol.
    let hello = match read_stream_frame(reader)? {
        Some(frame) => frame,
        None => return Err("stream closed before hello".to_owned()),
    };
    let hello_ok = hello.as_object().is_some_and(|object| {
        object.len() == 2
            && object.get("protocol").and_then(Value::as_str) == Some(STREAM_PROTOCOL)
            && object.get("version").and_then(Value::as_u64) == Some(STREAM_PROTOCOL_VERSION as u64)
    });
    if !hello_ok {
        write_stream_frame(
            writer,
            &json!({
                "seq": -1,
                "result": broker_error("protocol_error", "first stream frame must be a hello"),
            }),
        )?;
        return Err("first stream frame was not a valid hello".to_owned());
    }
    write_stream_frame(
        writer,
        &json!({
            "op": "hello_ok",
            "protocol": STREAM_PROTOCOL,
            "version": STREAM_PROTOCOL_VERSION,
        }),
    )?;

    let mut snapshot: Option<Value> = None;
    let mut expected_seq = 0_u64;
    while let Some(frame) = read_stream_frame(reader)? {
        let (response_seq, result) =
            match stream_request_result(&frame, &mut snapshot, &mut expected_seq) {
                Ok((seq, result)) => (seq, result),
                Err((seq, result)) => (seq, result),
            };
        write_stream_frame(writer, &json!({ "seq": response_seq, "result": result }))?;
    }
    Ok(())
}

/// Read one newline-delimited frame bounded by `MAX_REQUEST_BYTES`.
fn read_stream_frame(reader: &mut impl std::io::BufRead) -> Result<Option<Value>, String> {
    let mut line = Vec::new();
    let read = reader
        .read_until(b'\n', &mut line)
        .map_err(|error| format!("cannot read stream frame: {error}"))?;
    if read == 0 {
        return Ok(None);
    }
    if line.len() > MAX_REQUEST_BYTES {
        return Err(format!("stream frame exceeds {MAX_REQUEST_BYTES} bytes"));
    }
    let frame: Value = serde_json::from_slice(&line)
        .map_err(|error| format!("invalid stream frame JSON: {error}"))?;
    Ok(Some(frame))
}

/// Evaluate one request frame against the resident snapshot state. Returns
/// `(seq, result)` on every path — including protocol errors — so the caller
/// always emits exactly one reply frame per request.
fn stream_request_result(
    frame: &Value,
    snapshot: &mut Option<Value>,
    expected_seq: &mut u64,
) -> Result<(i64, Value), (i64, Value)> {
    let object = match frame.as_object() {
        Some(object) => object,
        None => {
            return Err((
                -1,
                broker_error("protocol_error", "stream frame must be a JSON object"),
            ));
        }
    };
    let seq = match object.get("seq").and_then(Value::as_i64) {
        Some(seq) if seq >= 0 => seq,
        _ => {
            return Err((
                -1,
                broker_error(
                    "protocol_error",
                    "stream frame seq must be a non-negative integer",
                ),
            ));
        }
    };
    let mut keys = object.keys().collect::<Vec<_>>();
    keys.sort();
    if keys != ["args", "op", "seq"] && keys != ["args", "op", "seq", "snapshot"] {
        return Err((
            seq,
            broker_error(
                "protocol_error",
                "stream frame must have exactly op, seq, args and an optional snapshot",
            ),
        ));
    }
    let op = match object.get("op").and_then(Value::as_str) {
        Some(op) if ALLOWED_OPS.contains(&op) => op.to_owned(),
        _ => {
            return Err((
                seq,
                broker_error(
                    "protocol_error",
                    "stream frame op must be one of the allowlisted operations",
                ),
            ));
        }
    };
    let args = match object.get("args").and_then(Value::as_object).cloned() {
        Some(args) => Value::Object(args),
        None => {
            return Err((
                seq,
                broker_error("protocol_error", "stream frame args must be an object"),
            ));
        }
    };
    if let Some(inline) = object.get("snapshot") {
        let Some(next) = inline.as_object() else {
            return Err((
                seq,
                broker_error("protocol_error", "stream frame snapshot must be an object"),
            ));
        };
        *snapshot = Some(Value::Object(next.clone()));
    }
    let Some(active_snapshot) = snapshot.clone() else {
        return Err((
            seq,
            broker_error(
                "missing_snapshot",
                "stream frame carries no snapshot and no topology has been delivered yet",
            ),
        ));
    };
    if seq as u64 != *expected_seq {
        // Resynchronize instead of dropping the connection: concurrent parent
        // callers are serialized per worker, so a gap means the parent dropped
        // a frame and must re-drive ordering on its side.
        *expected_seq = seq as u64 + 1;
        return Err((
            seq,
            broker_error(
                "protocol_error",
                &format!("stream frame seq {seq} out of order (resynchronized)"),
            ),
        ));
    }
    *expected_seq += 1;
    Ok((
        seq,
        dispatch(&Request {
            op,
            snapshot: active_snapshot,
            args,
        }),
    ))
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

/// Explicitly replace an older broker revision when macOS must authorize a
/// new code identity. Ordinary install/update paths must never call this: the
/// caller has to opt into the broker upgrade and complete `permissions verify`
/// afterwards. The prior broker bytes/metadata remain beside the new broker
/// until verification succeeds so a failed migration still has rollback
/// evidence instead of silently destroying the last authorized identity.
pub fn migrate_for_explicit_reauthorization(config_dir: &Path) -> Result<bool, String> {
    let upgrade = upgrade_status(config_dir);
    if !upgrade.update_available {
        return Ok(false);
    }

    let source = broker_candidate_path()?;
    let source_bytes = std::fs::read(&source).map_err(|error| {
        format!(
            "cannot read TCC broker upgrade candidate {}: {error}",
            source.display()
        )
    })?;
    let target = broker_path(config_dir);
    let metadata = broker_metadata_path(config_dir);
    let previous = previous_broker_path(config_dir);
    let previous_metadata = previous_broker_metadata_path(config_dir);
    let parent = target
        .parent()
        .ok_or_else(|| "broker path has no parent directory".to_owned())?;
    ensure_secure_dir(parent)?;

    let target_meta = std::fs::symlink_metadata(&target).map_err(|error| {
        format!(
            "cannot inspect existing broker {}: {error}",
            target.display()
        )
    })?;
    if target_meta.file_type().is_symlink() || !target_meta.is_file() {
        return Err(format!(
            "existing TCC broker {} must be a regular file and not a symlink",
            target.display()
        ));
    }
    if previous.exists() || previous_metadata.exists() {
        return Err(
            "a previous TCC broker migration backup is still present; complete or recover that migration before starting another"
                .to_owned(),
        );
    }

    std::fs::rename(&target, &previous).map_err(|error| {
        format!(
            "cannot preserve previous TCC broker {}: {error}",
            target.display()
        )
    })?;
    let metadata_was_present = metadata.exists();
    if metadata_was_present && let Err(error) = std::fs::rename(&metadata, &previous_metadata) {
        let _ = std::fs::rename(&previous, &target);
        return Err(format!(
            "cannot preserve previous TCC broker metadata {}: {error}",
            metadata.display()
        ));
    }

    let install_result = atomic_write(&target, &source_bytes, 0o700)
        .and_then(|()| write_broker_metadata(config_dir));
    if let Err(error) = install_result {
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(&metadata);
        let broker_restored = std::fs::rename(&previous, &target).is_ok();
        let metadata_restored =
            !metadata_was_present || std::fs::rename(&previous_metadata, &metadata).is_ok();
        return Err(format!(
            "TCC broker migration failed: {error}; rollback broker_restored={broker_restored} metadata_restored={metadata_restored}"
        ));
    }
    Ok(true)
}

/// Remove the rollback copy only after the newly installed broker has passed
/// the responsibility-isolated permission probe.
#[cfg(any(target_os = "macos", test))]
pub fn finalize_explicit_reauthorization(config_dir: &Path) -> Result<(), String> {
    for path in [
        previous_broker_path(config_dir),
        previous_broker_metadata_path(config_dir),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(format!(
                    "TCC broker migration backup {} must be a regular file",
                    path.display()
                ));
            }
            Ok(_) => std::fs::remove_file(&path)
                .map_err(|error| format!("cannot remove {}: {error}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("cannot inspect {}: {error}", path.display()));
            }
        }
    }
    Ok(())
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
/// enabled (caller falls back to direct execution). macOS linked-worktree grep
/// stays in the current runtime so it cannot re-enter protected Git metadata
/// through an older stable broker implementation. The broker is otherwise
/// spawned as a one-shot child of the current executable's stable broker path.
pub fn route_fs_git(op: &str, snapshot: &Value, args: &Value) -> Option<Result<Value, String>> {
    if op == "fs_grep" && fs_tools::grep_prefers_in_process(args) {
        return None;
    }
    if std::env::var("HERDR_MCP_TCC_BROKER").ok().as_deref() != Some("1") {
        return None;
    }
    Some(run_stable_broker(op, snapshot, args).map(annotate_operational_root_upgrade))
}

/// Explain an `outside_managed_roots` denial when the installed stable broker
/// predates operational-root support.
///
/// This is strictly additive on an already-failing result: an outdated broker
/// keeps denying, and the caller learns that a non-Git operational root cannot
/// be served until the broker is upgraded. It never turns a denial into a
/// success, and it does not need the topology (which the rotating runtime
/// cannot resolve for TCC-protected paths anyway).
fn annotate_operational_root_upgrade(value: Value) -> Value {
    let Ok(paths) = crate::paths::RuntimePaths::discover() else {
        return value;
    };
    annotate_operational_root_upgrade_with(value, &paths.config_dir)
}

fn annotate_operational_root_upgrade_with(value: Value, config_dir: &Path) -> Value {
    if value.get("reason").and_then(Value::as_str) != Some("outside_managed_roots") {
        return value;
    }
    let installed = installed_compat_revision(config_dir);
    if installed.is_some_and(|revision| revision >= OPERATIONAL_ROOT_MIN_COMPAT_REVISION) {
        return value;
    }
    let mut value = value;
    if let Some(object) = value.as_object_mut() {
        object.insert("broker_compat_revision".to_owned(), json!(installed));
        object.insert(
            "broker_upgrade_reason".to_owned(),
            json!(
                "the installed stable TCC broker predates operational-root support (compat revision 4); if this path is a non-Git directory exactly proven by a live workspace/pane cwd, this broker cannot serve it yet — run `herdr-mcp permissions setup --upgrade-broker`"
            ),
        );
    }
    value
}

/// Whether the installed stable broker can answer the read-only `git` identity
/// action. A missing or invalid compatibility revision fails closed.
#[cfg(target_os = "macos")]
pub(crate) fn installed_supports_git_identity(config_dir: &Path) -> bool {
    installed_compat_revision(config_dir)
        .is_some_and(|revision| revision >= GIT_IDENTITY_MIN_COMPAT_REVISION)
}

/// Resolve the exact `HEAD` commit and branch of a managed root through the
/// installed stable broker. The broker re-validates the root against the live
/// snapshot's managed roots, so the runtime never needs to name a `gitdir`
/// path that may live outside them.
#[cfg(target_os = "macos")]
pub(crate) fn git_identity_via_stable_broker(
    snapshot: &Value,
    root: &Path,
) -> Result<Value, String> {
    run_stable_broker(
        "git",
        snapshot,
        &json!({
            "root": root.to_string_lossy(),
            "action": "identity",
        }),
    )
}

/// Read bounded repository metadata through the already-installed stable TCC
/// broker. This reuses the existing `fs_read` wire operation; the caller must
/// still provide a live Herdr snapshot so the managed-root and secret-path
/// gates remain identical to an ordinary `herdr_fs_read`. It deliberately does
/// not depend on `HERDR_MCP_TCC_BROKER`, so a CLI that never inherited the
/// service's routing flag still reaches the long-lived broker identity.
#[cfg(target_os = "macos")]
pub(crate) fn fs_read_via_stable_broker(
    snapshot: &Value,
    path: &Path,
    max_bytes: usize,
) -> Result<Value, String> {
    run_stable_broker(
        "fs_read",
        snapshot,
        &json!({
            "path": path.to_string_lossy(),
            "start_line": 1,
            "end_line": 100_000,
            "max_bytes": max_bytes,
        }),
    )
}

/// List one bounded directory level through the already-installed stable TCC
/// broker. This reuses the existing `fs_list` wire operation; the caller must
/// still provide a live Herdr snapshot so the managed-root, secret-path and
/// symlink-confinement gates remain identical to an ordinary `herdr_fs_list`.
/// Like the other stable-broker helpers it deliberately does not depend on
/// `HERDR_MCP_TCC_BROKER`, so a CLI that never inherited the service's routing
/// flag still reaches the long-lived broker identity.
#[cfg(target_os = "macos")]
pub(crate) fn fs_list_via_stable_broker(
    snapshot: &Value,
    path: &Path,
    max_entries: usize,
) -> Result<Value, String> {
    run_stable_broker(
        "fs_list",
        snapshot,
        &json!({
            "path": path.to_string_lossy(),
            "recursive": false,
            "max_entries": max_entries,
        }),
    )
}

/// Read one bounded line window through the existing `fs_read` wire operation.
/// The caller must pass a live snapshot and an explicit `start_line`, and must
/// honour the returned `truncated`/`next_start_line` contract instead of
/// digesting a partial window.
#[cfg(target_os = "macos")]
pub(crate) fn fs_read_window_via_stable_broker(
    snapshot: &Value,
    path: &Path,
    start_line: usize,
    end_line: usize,
    max_bytes: usize,
) -> Result<Value, String> {
    run_stable_broker(
        "fs_read",
        snapshot,
        &json!({
            "path": path.to_string_lossy(),
            "start_line": start_line,
            "end_line": end_line,
            "max_bytes": max_bytes,
        }),
    )
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

/// Fail closed: refuse a broker path that is not a regular file (symlinks
/// included). Validated at the `run_stable_broker` entry so both the
/// streaming-pool and one-shot paths share one boundary, and re-checked in
/// `spawn_stream_worker` before each resident spawn.
fn validate_broker_file(broker: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(broker)
        .map_err(|error| format!("cannot inspect broker {}: {error}", broker.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "broker {} must be a regular file (not a symlink)",
            broker.display()
        ));
    }
    Ok(())
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
    // Regular-file (non-symlink) gate before either execution path: the
    // streaming pool used to spawn past this check entirely.
    validate_broker_file(&broker)?;
    #[cfg(target_os = "macos")]
    {
        // Prefer the resident streaming worker (#1068): zero spawn/env copies
        // and no repeated whole-snapshot transfer while the routing topology
        // is unchanged. `None` means the installed broker predates streaming —
        // fall through to the legacy one-shot child with identical semantics.
        if let Some(value) = run_broker_stream_pool(&broker, op, snapshot, args)? {
            return Ok(value);
        }
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
    // Re-check at the actual exec boundary (the entry already gated this).
    validate_broker_file(broker)?;

    #[cfg(target_os = "macos")]
    {
        run_broker_child_disclaimed(broker, request_bytes)
    }
    #[cfg(not(target_os = "macos"))]
    {
        run_broker_child_standard(broker, request_bytes)
    }
}

#[cfg(not(target_os = "macos"))]
fn run_broker_child_standard(broker: &Path, request_bytes: &[u8]) -> Result<Value, String> {
    use crate::child_process;
    use std::process::{Command, Stdio};

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

#[cfg(target_os = "macos")]
fn posix_spawn_ok(code: libc::c_int, context: &str) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!(
            "{context}: {}",
            std::io::Error::from_raw_os_error(code)
        ))
    }
}

/// Spawn one hidden mode of the stable broker while explicitly disclaiming
/// the rotating caller's macOS responsibility. The child becomes the TCC
/// responsible identity. This is the shared primitive for the real broker
/// path, permission verification, and the restricted Herdr host.
#[cfg(target_os = "macos")]
fn spawn_disclaimed_broker_mode(
    broker: &Path,
    mode: &str,
    stdin_fd: libc::c_int,
    stdout_fd: libc::c_int,
    stderr_fd: libc::c_int,
    broker_child_env: bool,
) -> Result<libc::pid_t, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::ptr;

    type ResponsibilitySpawnattrsSetdisclaim =
        unsafe extern "C" fn(*mut libc::posix_spawnattr_t, libc::c_int) -> libc::c_int;

    let broker_c = CString::new(broker.as_os_str().as_bytes())
        .map_err(|_| format!("broker path contains NUL: {}", broker.display()))?;
    let mode_c = CString::new(mode).map_err(|_| "broker mode contains NUL".to_owned())?;
    let mut argv = vec![
        broker_c.as_ptr().cast_mut(),
        mode_c.as_ptr().cast_mut(),
        ptr::null_mut(),
    ];
    let mut env_storage = std::env::vars_os()
        .filter(|(key, _)| key != BROKER_CHILD_ENV)
        .map(|(key, value)| {
            let mut bytes = key.as_os_str().as_bytes().to_vec();
            bytes.push(b'=');
            bytes.extend_from_slice(value.as_os_str().as_bytes());
            CString::new(bytes).map_err(|_| "environment contains NUL".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if broker_child_env {
        env_storage.push(
            CString::new(format!("{BROKER_CHILD_ENV}=1")).expect("static broker env has no NUL"),
        );
    }
    let mut envp = env_storage
        .iter()
        .map(|entry| entry.as_ptr().cast_mut())
        .collect::<Vec<_>>();
    envp.push(ptr::null_mut());

    let mut actions: libc::posix_spawn_file_actions_t = ptr::null_mut();
    posix_spawn_ok(
        unsafe { libc::posix_spawn_file_actions_init(&mut actions) },
        "cannot initialize broker spawn file actions",
    )?;
    let mut attrs: libc::posix_spawnattr_t = ptr::null_mut();
    if let Err(error) = posix_spawn_ok(
        unsafe { libc::posix_spawnattr_init(&mut attrs) },
        "cannot initialize broker spawn attributes",
    ) {
        unsafe {
            libc::posix_spawn_file_actions_destroy(&mut actions);
        }
        return Err(error);
    }

    let result = (|| {
        let stdio = [
            (stdin_fd, libc::STDIN_FILENO),
            (stdout_fd, libc::STDOUT_FILENO),
            (stderr_fd, libc::STDERR_FILENO),
        ];
        for (from, to) in stdio {
            posix_spawn_ok(
                unsafe { libc::posix_spawn_file_actions_adddup2(&mut actions, from, to) },
                "cannot configure broker stdio",
            )?;
        }
        let mut close_fds = Vec::new();
        for (from, to) in stdio {
            if from != to && !close_fds.contains(&from) {
                close_fds.push(from);
            }
        }
        for fd in close_fds {
            posix_spawn_ok(
                unsafe { libc::posix_spawn_file_actions_addclose(&mut actions, fd) },
                "cannot close broker inherited stdio fd",
            )?;
        }

        let flags = (libc::POSIX_SPAWN_SETPGROUP | libc::POSIX_SPAWN_CLOEXEC_DEFAULT) as i16;
        posix_spawn_ok(
            unsafe { libc::posix_spawnattr_setflags(&mut attrs, flags) },
            "cannot configure broker spawn flags",
        )?;
        posix_spawn_ok(
            unsafe { libc::posix_spawnattr_setpgroup(&mut attrs, 0) },
            "cannot configure broker process group",
        )?;

        let symbol_name = CString::new("responsibility_spawnattrs_setdisclaim")
            .expect("static responsibility symbol has no NUL");
        let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, symbol_name.as_ptr()) };
        if symbol.is_null() {
            return Err(
                "macOS responsibility_spawnattrs_setdisclaim is unavailable; refusing to launch stable TCC broker with rotating caller responsibility"
                    .to_owned(),
            );
        }
        let disclaim: ResponsibilitySpawnattrsSetdisclaim = unsafe { std::mem::transmute(symbol) };
        posix_spawn_ok(
            unsafe { disclaim(&mut attrs, 1) },
            "cannot disclaim rotating caller TCC responsibility for broker",
        )?;

        let mut pid = 0_i32;
        posix_spawn_ok(
            unsafe {
                libc::posix_spawn(
                    &mut pid,
                    broker_c.as_ptr(),
                    &actions,
                    &attrs,
                    argv.as_mut_ptr(),
                    envp.as_mut_ptr(),
                )
            },
            "cannot spawn disclaimed TCC broker",
        )?;
        Ok(pid)
    })();

    unsafe {
        libc::posix_spawnattr_destroy(&mut attrs);
        libc::posix_spawn_file_actions_destroy(&mut actions);
    }
    result
}

#[cfg(target_os = "macos")]
fn try_wait_pid(pid: libc::pid_t) -> std::io::Result<Option<std::process::ExitStatus>> {
    use std::os::unix::process::ExitStatusExt;
    let mut raw_status = 0_i32;
    let waited = unsafe { libc::waitpid(pid, &mut raw_status, libc::WNOHANG) };
    if waited == pid {
        Ok(Some(std::process::ExitStatus::from_raw(raw_status)))
    } else if waited == 0 {
        Ok(None)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn wait_pid_bounded(
    pid: libc::pid_t,
    timeout: Duration,
) -> Result<Option<std::process::ExitStatus>, String> {
    let started = std::time::Instant::now();
    loop {
        match try_wait_pid(pid) {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {}
            Err(error) => return Err(format!("broker waitpid failed: {error}")),
        }
        if started.elapsed() >= timeout {
            terminate_pid_group(pid);
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "macos")]
fn terminate_pid_group(pid: libc::pid_t) {
    let _ = unsafe { libc::kill(-pid, libc::SIGTERM) };
    let grace = std::time::Instant::now();
    while grace.elapsed() < Duration::from_millis(500) {
        match try_wait_pid(pid) {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return,
        }
    }
    let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
    let mut raw_status = 0_i32;
    let _ = unsafe { libc::waitpid(pid, &mut raw_status, 0) };
}

#[cfg(target_os = "macos")]
pub(crate) fn run_disclaimed_documents_probe(
    broker: &Path,
    timeout: Duration,
) -> Result<Option<std::process::ExitStatus>, String> {
    use std::os::fd::AsRawFd;
    let null = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(|error| format!("cannot open /dev/null for TCC probe: {error}"))?;
    let fd = null.as_raw_fd();
    let pid = spawn_disclaimed_broker_mode(broker, "__documents-probe", fd, fd, fd, false)?;
    let _registration = crate::child_process::register_owned_pid("documents-probe", pid as u32);
    wait_pid_bounded(pid, timeout)
}

#[cfg(target_os = "macos")]
pub(crate) struct MacosHerdrHostChild {
    pid: libc::pid_t,
    _registration: crate::child_process::OwnedChildRegistration,
}

#[cfg(target_os = "macos")]
impl MacosHerdrHostChild {
    pub(crate) fn id(&self) -> u32 {
        self.pid as u32
    }

    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        try_wait_pid(self.pid)
    }

    pub(crate) fn terminate_and_reap(&mut self) {
        terminate_pid_group(self.pid);
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn spawn_herdr_host(
    config_dir: &Path,
    stdout: &std::fs::File,
    stderr: &std::fs::File,
) -> Result<MacosHerdrHostChild, String> {
    use std::os::fd::AsRawFd;
    if !installed_supports_herdr_host(config_dir) {
        return Err("installed TCC broker does not support stable Herdr hosting".to_owned());
    }
    let broker = broker_path(config_dir);
    let null = std::fs::OpenOptions::new()
        .read(true)
        .open("/dev/null")
        .map_err(|error| format!("cannot open /dev/null for Herdr host: {error}"))?;
    let pid = spawn_disclaimed_broker_mode(
        &broker,
        "__tcc-herdr-host",
        null.as_raw_fd(),
        stdout.as_raw_fd(),
        stderr.as_raw_fd(),
        false,
    )?;
    let registration = crate::child_process::register_owned_pid("tcc-herdr-host", pid as u32);
    Ok(MacosHerdrHostChild {
        pid,
        _registration: registration,
    })
}

#[cfg(target_os = "macos")]
pub fn run_herdr_host() -> Result<ExitCode, String> {
    use std::process::{Command, Stdio};

    let paths = crate::paths::RuntimePaths::discover()?;
    if paths.instance.is_named() {
        return Err(
            "stable TCC Herdr host is owned only by the default production instance".to_owned(),
        );
    }
    let broker = broker_path(&paths.config_dir);
    let current = std::env::current_exe()
        .map_err(|error| format!("cannot resolve TCC Herdr host executable: {error}"))?;
    if !same_canonical_file(&current, &broker) {
        return Err(format!(
            "refusing TCC Herdr host outside installed stable broker {}",
            broker.display()
        ));
    }
    if !installed_supports_herdr_host(&paths.config_dir) {
        return Err("installed TCC broker metadata does not authorize Herdr host mode".to_owned());
    }
    let herdr = trusted_herdr_binary()?;
    let socket = paths
        .herdr_socket
        .as_deref()
        .ok_or_else(|| "stable TCC Herdr host requires a local Unix socket".to_owned())?;
    let mut command = Command::new(&herdr);
    command
        .arg("server")
        .env("HERDR_SOCKET_PATH", socket)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // Deliberately do not create a new process group and do not disclaim the
    // Herdr child: it must remain a normal descendant of the stable broker so
    // macOS TCC responsibility stays attached to the broker identity.
    let mut child = command
        .spawn()
        .map_err(|error| format!("stable TCC broker cannot start Herdr server: {error}"))?;
    let _registration = crate::child_process::register_owned_child("tcc-herdr-server", &child);
    let status = child
        .wait()
        .map_err(|error| format!("stable TCC broker cannot wait for Herdr server: {error}"))?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[cfg(not(target_os = "macos"))]
pub fn run_herdr_host() -> Result<ExitCode, String> {
    Err("stable TCC Herdr host is only available on macOS".to_owned())
}

#[cfg(target_os = "macos")]
fn run_broker_child_disclaimed(broker: &Path, request_bytes: &[u8]) -> Result<Value, String> {
    use crate::child_process;
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};

    fn pipe_pair() -> Result<(File, File), String> {
        let mut fds = [-1_i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(format!(
                "cannot create TCC broker pipe: {}",
                std::io::Error::last_os_error()
            ));
        }
        for fd in fds {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
            {
                unsafe {
                    libc::close(fds[0]);
                    libc::close(fds[1]);
                }
                return Err(format!(
                    "cannot secure TCC broker pipe: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
    }

    let (stdin_read, mut stdin_write) = pipe_pair()?;
    let (mut stdout_read, stdout_write) = pipe_pair()?;
    let (mut stderr_read, stderr_write) = pipe_pair()?;
    let pid = spawn_disclaimed_broker_mode(
        broker,
        "__tcc-broker",
        stdin_read.as_raw_fd(),
        stdout_write.as_raw_fd(),
        stderr_write.as_raw_fd(),
        true,
    )?;
    let _registration = child_process::register_owned_pid("tcc-broker", pid as u32);

    drop(stdin_read);
    drop(stdout_write);
    drop(stderr_write);

    let request_owned = request_bytes.to_vec();
    let write_handle = std::thread::spawn(move || {
        let _ = stdin_write.write_all(&request_owned);
    });
    let stdout_handle =
        std::thread::spawn(move || read_capped(&mut stdout_read, MAX_RESPONSE_BYTES));
    let stderr_handle = std::thread::spawn(move || read_capped(&mut stderr_read, 64 * 1024));

    let status = wait_pid_bounded(pid, REQUEST_TIMEOUT)?;
    let _ = write_handle.join();
    let out = stdout_handle
        .join()
        .map_err(|_| "broker stdout reader panicked".to_owned())?;
    let err = stderr_handle
        .join()
        .map_err(|_| "broker stderr reader panicked".to_owned())?;

    let Some(status) = status else {
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

// ---------------------------------------------------------------------------
// Persistent streaming broker pool (#1068, macOS only).
//
// One resident disclaimed `__tcc-broker-stream` worker replaces the per-call
// `posix_spawn` + three pipe pairs + full environment copy + whole-snapshot
// round trip. Requests serialize through a worker and carry only op/args once
// the routing fingerprint has been delivered; a changed fingerprint resends
// the topology in that frame. A request that times out or fails in flight
// kills the whole worker process group and the next acquire spawns a fresh
// one. An installed broker without the streaming mode exits during the hello
// handshake; the pool then latches "streaming unsupported" and every caller
// falls back to the legacy one-shot path with unchanged error codes.
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
const STREAM_POOL_MAX_WORKERS: usize = 4;
#[cfg(target_os = "macos")]
const STREAM_WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(target_os = "macos")]
const STREAM_HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Structural fingerprint of the routing snapshot. Hashing the JSON tree
/// avoids serializing the whole topology per call; a different fingerprint
/// means the next frame must carry the snapshot again.
#[cfg(any(target_os = "macos", test))]
fn topology_fingerprint(value: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    fn hash_value(value: &Value, state: &mut std::collections::hash_map::DefaultHasher) {
        match value {
            Value::Null => 0_u8.hash(state),
            Value::Bool(b) => (1_u8, b).hash(state),
            Value::Number(n) => (2_u8, n.to_string()).hash(state),
            Value::String(s) => (3_u8, s).hash(state),
            Value::Array(items) => {
                4_u8.hash(state);
                for item in items {
                    hash_value(item, state);
                }
            }
            Value::Object(map) => {
                5_u8.hash(state);
                for (key, item) in map {
                    key.hash(state);
                    hash_value(item, state);
                }
            }
        }
    }
    let mut state = std::collections::hash_map::DefaultHasher::new();
    hash_value(value, &mut state);
    state.finish()
}

/// Read one newline-delimited frame with a deadline. The frame is capped by
/// `MAX_RESPONSE_BYTES`; an oversized or empty (EOF) read returns `None`, and
/// an exhausted deadline returns `Err` — callers treat both as a dead worker.
#[cfg(target_os = "macos")]
fn read_stream_reply(reader: &std::fs::File, timeout: Duration) -> Result<Option<Vec<u8>>, String> {
    use std::os::fd::AsRawFd;
    // Wait and read on the calling thread via poll(2): no per-request thread
    // spawn, and no raw descriptor borrowed into another thread whose lifetime
    // could outlive the file or race a kill. `File` stays the sole fd owner;
    // EINTR retries against the recomputed remaining deadline.
    let fd = reader.as_raw_fd();
    let deadline = std::time::Instant::now() + timeout;
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 8192];
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err("stream reply deadline exceeded".to_owned());
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("stream reply poll failed: {error}"));
        }
        if ready == 0 {
            return Err("stream reply deadline exceeded".to_owned());
        }
        // POLLIN guarantees the read below cannot block; POLLHUP/POLLERR still
        // flow through it so EOF and errors resolve the same way.
        let count = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if count < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("stream reply read failed: {error}"));
        }
        if count == 0 {
            return Ok(None);
        }
        let count = count as usize;
        if let Some(newline) = chunk[..count].iter().position(|b| *b == b'\n') {
            buf.extend_from_slice(&chunk[..newline]);
            return Ok(Some(buf));
        }
        buf.extend_from_slice(&chunk[..count]);
        if buf.len() > MAX_RESPONSE_BYTES {
            return Ok(None);
        }
    }
}

/// Write one request frame with a deadline shared with the reply read. The
/// frame can reach MAX_REQUEST_BYTES (8 MiB), far beyond the pipe buffer: a
/// worker that keeps its read end open but stops draining would otherwise pin
/// the calling thread in `write_all` forever, never run the timeout, kill or
/// slot-return path, and exhaust all STREAM_POOL_MAX_WORKERS leases behind
/// `ready` (P1, review 7003833175). The fd is switched to `O_NONBLOCK` here —
/// the flag lives on the open file description and persists for later
/// requests on the same worker — and `poll(POLLOUT)` bounds every partial
/// write. EINTR and WouldBlock retry against the recomputed remaining
/// deadline; any other failure or an exhausted deadline is a dead worker.
#[cfg(target_os = "macos")]
fn write_stream_request(
    writer: &std::fs::File,
    bytes: &[u8],
    deadline: std::time::Instant,
) -> Result<(), String> {
    use std::os::fd::AsRawFd;

    let fd = writer.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(format!(
            "stream stdin nonblocking setup failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut written = 0_usize;
    while written < bytes.len() {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err("stream request write deadline exceeded".to_owned());
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("stream request poll failed: {error}"));
        }
        if ready == 0 {
            return Err("stream request write deadline exceeded".to_owned());
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err("stream worker stdin closed".to_owned());
        }
        let count =
            unsafe { libc::write(fd, bytes[written..].as_ptr().cast(), bytes.len() - written) };
        if count < 0 {
            let error = std::io::Error::last_os_error();
            if matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(format!("stream request write failed: {error}"));
        }
        if count == 0 {
            return Err("stream worker stdin closed".to_owned());
        }
        written += count as usize;
    }
    Ok(())
}

/// One resident broker worker process and its pipes.
#[cfg(target_os = "macos")]
struct StreamWorker {
    pid: libc::pid_t,
    stdin: std::fs::File,
    stdout: std::fs::File,
    _registration: crate::child_process::OwnedChildRegistration,
    last_used: std::time::Instant,
    fingerprint: u64,
    next_seq: u64,
    healthy: bool,
}

#[cfg(target_os = "macos")]
impl StreamWorker {
    fn kill(&mut self) {
        self.healthy = false;
        terminate_pid_group(self.pid);
    }
}

#[cfg(target_os = "macos")]
impl Drop for StreamWorker {
    fn drop(&mut self) {
        terminate_pid_group(self.pid);
    }
}

/// Spawn a resident stream worker and complete the hello handshake. Returns
/// `Ok(None)` when the broker binary does not implement the streaming mode —
/// the caller then permanently falls back to the one-shot protocol.
#[cfg(target_os = "macos")]
fn spawn_stream_worker(broker: &Path) -> Result<Option<StreamWorker>, String> {
    use std::os::fd::AsRawFd;

    // Every resident spawn re-checks the regular-file boundary: the installed
    // broker can be replaced between requests, and the pool path must keep the
    // same gate as the one-shot child.
    validate_broker_file(broker)?;

    let (stdin_read, mut stdin_write) = stream_pipe_pair()?;
    let (stdout_read, stdout_write) = stream_pipe_pair()?;
    // stderr goes to /dev/null, not a pipe: a resident worker that chatters
    // past the 64 KiB pipe capacity would otherwise block its writes and stall
    // every request (P2, review 6997783732).
    let devnull = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .map_err(|error| format!("cannot open /dev/null for broker stream: {error}"))?;
    let pid = spawn_disclaimed_broker_mode(
        broker,
        "__tcc-broker-stream",
        stdin_read.as_raw_fd(),
        stdout_write.as_raw_fd(),
        devnull.as_raw_fd(),
        true,
    )?;
    let registration = crate::child_process::register_owned_pid("tcc-broker-stream", pid as u32);
    drop(stdin_read);
    drop(stdout_write);
    drop(devnull);

    let hello = serde_json::to_vec(&json!({
        "protocol": STREAM_PROTOCOL,
        "version": STREAM_PROTOCOL_VERSION,
    }))
    .map_err(|error| format!("cannot serialize stream hello: {error}"))?;
    let worker_stdin = stdin_write
        .try_clone()
        .map_err(|error| format!("cannot duplicate broker stream stdin: {error}"))?;
    let mut worker = StreamWorker {
        pid,
        stdin: worker_stdin,
        stdout: stdout_read,
        _registration: registration,
        last_used: std::time::Instant::now(),
        fingerprint: 0,
        next_seq: 0,
        healthy: true,
    };
    if stdin_write.write_all(&hello).is_err()
        || stdin_write.write_all(b"\n").is_err()
        || stdin_write.flush().is_err()
    {
        worker.kill();
        return Ok(None);
    }
    match read_stream_reply(&worker.stdout, STREAM_HELLO_TIMEOUT) {
        Ok(Some(frame)) => {
            let parsed: Result<Value, _> = serde_json::from_slice(&frame);
            let ok = parsed
                .as_ref()
                .ok()
                .and_then(|value| value.get("op"))
                .and_then(Value::as_str)
                == Some("hello_ok");
            if ok {
                return Ok(Some(worker));
            }
            // The child answered but did not accept the protocol — treat it as
            // an old broker and degrade permanently.
            worker.kill();
            Ok(None)
        }
        Ok(None) => {
            // EOF or oversized frame: old broker exits on the unknown mode.
            worker.kill();
            Ok(None)
        }
        Err(error) => {
            worker.kill();
            Err(format!("stream hello handshake failed: {error}"))
        }
    }
}

#[cfg(target_os = "macos")]
fn stream_pipe_pair() -> Result<(std::fs::File, std::fs::File), String> {
    use std::os::fd::FromRawFd;
    let mut fds = [-1_i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(format!(
            "cannot create TCC broker stream pipe: {}",
            std::io::Error::last_os_error()
        ));
    }
    for fd in fds {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(format!(
                "cannot secure TCC broker stream pipe: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(unsafe {
        (
            std::fs::File::from_raw_fd(fds[0]),
            std::fs::File::from_raw_fd(fds[1]),
        )
    })
}

#[cfg(target_os = "macos")]
struct StreamPoolState {
    idle: Vec<StreamWorker>,
    live: usize,
    unsupported: bool,
}

/// `(dev, ino)` identity of the broker binary a pool was built against. A
/// reinstalled, upgraded or uninstalled broker changes this identity; the
/// running service must not keep serving from stale resident workers or a
/// latched `unsupported` verdict (P2, review 6997783732).
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BrokerFileId {
    dev: u64,
    ino: u64,
}

#[cfg(target_os = "macos")]
fn broker_file_id(broker: &Path) -> Result<BrokerFileId, String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(broker)
        .map_err(|error| format!("cannot stat broker {}: {error}", broker.display()))?;
    Ok(BrokerFileId {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

/// Bounded pool of resident broker workers. Requests run serially per worker
/// (frame order is enforced by the per-worker `next_seq`), which is the
/// isolation contract for concurrent callers: each in-flight request has a
/// whole worker — and therefore a private seq lane — to itself.
///
/// The lock only ever covers state transitions. `terminate_pid_group` (up to
/// ~500ms) and `spawn_stream_worker` (posix_spawn plus the up-to-10s hello
/// handshake) always run unlocked so completed leases and concurrent
/// requests are never queued behind a spawn or a reap (P1, review
/// 6997783732).
#[cfg(target_os = "macos")]
struct StreamPool {
    broker: PathBuf,
    /// `None` = unmanaged (tests); disk identity is not enforced.
    broker_id: Option<BrokerFileId>,
    request_timeout: Duration,
    state: std::sync::Mutex<StreamPoolState>,
    ready: std::sync::Condvar,
}

#[cfg(target_os = "macos")]
impl StreamPool {
    fn new(broker: &Path, broker_id: Option<BrokerFileId>) -> Self {
        Self {
            broker: broker.to_path_buf(),
            broker_id,
            request_timeout: REQUEST_TIMEOUT,
            state: std::sync::Mutex::new(StreamPoolState {
                idle: Vec::new(),
                live: 0,
                unsupported: false,
            }),
            ready: std::sync::Condvar::new(),
        }
    }

    /// Acquire a worker lease. `Ok(None)` means streaming is unsupported —
    /// either the installed broker cannot stream or the binary on disk no
    /// longer matches the one this pool was built for; the caller must run
    /// the one-shot path.
    fn acquire(self: std::sync::Arc<Self>) -> Result<Option<PoolLease>, String> {
        // Disk-awareness: never serve a request from a pool whose broker
        // binary was replaced or removed. The one-shot path re-validates and
        // executes the current file, and the next call rebuilds the pool.
        if let Some(expected) = self.broker_id
            && broker_file_id(&self.broker).ok() != Some(expected)
        {
            return Ok(None);
        }
        enum Step {
            Lease(StreamWorker),
            Spawn,
            Wait,
        }
        loop {
            let (step, stale) = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.unsupported {
                    return Ok(None);
                }
                let mut leased = None;
                let mut stale = Vec::new();
                while let Some(worker) = state.idle.pop() {
                    if worker.last_used.elapsed() > STREAM_WORKER_IDLE_TIMEOUT {
                        stale.push(worker);
                    } else {
                        leased = Some(worker);
                        break;
                    }
                }
                // Every popped stale worker frees its concurrency slot
                // immediately — leased or not — so interleaved last_used
                // updates can never leak a slot toward the permanent wait
                // deadlock (P1, review 7003213891). `stale` travels out of the
                // block and drops after `state`: StreamWorker::drop runs
                // terminate_pid_group (up to ~500ms) and must never run under
                // the pool lock.
                state.live -= stale.len();
                if let Some(worker) = leased {
                    (Step::Lease(worker), stale)
                } else if state.live < STREAM_POOL_MAX_WORKERS {
                    // Reserve the slot now; the expensive spawn runs unlocked.
                    state.live += 1;
                    (Step::Spawn, stale)
                } else {
                    state = self.ready.wait(state).unwrap_or_else(|e| e.into_inner());
                    (Step::Wait, stale)
                }
            };
            drop(stale);
            match step {
                Step::Lease(worker) => {
                    return Ok(Some(PoolLease {
                        pool: self.clone(),
                        worker: Some(worker),
                    }));
                }
                Step::Spawn => match spawn_stream_worker(&self.broker) {
                    Ok(Some(worker)) => {
                        return Ok(Some(PoolLease {
                            pool: self.clone(),
                            worker: Some(worker),
                        }));
                    }
                    Ok(None) => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.live -= 1;
                        state.unsupported = true;
                        drop(state);
                        self.ready.notify_all();
                        return Ok(None);
                    }
                    Err(error) => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.live -= 1;
                        drop(state);
                        self.ready.notify_all();
                        return Err(error);
                    }
                },
                Step::Wait => {}
            }
        }
    }
}

/// Checked-out worker. Returning it to the pool releases concurrency capacity;
/// dropping a dead worker frees a spawn slot. The lease holds an `Arc` so a
/// replaced pool stays alive until its last lease drains.
#[cfg(target_os = "macos")]
struct PoolLease {
    pool: std::sync::Arc<StreamPool>,
    worker: Option<StreamWorker>,
}

#[cfg(target_os = "macos")]
impl PoolLease {
    /// Drive one request frame. On success the worker stays healthy and the
    /// reply carries the tool envelope; any transport failure marks the worker
    /// dead so `Drop` kills the process and frees its pool slot.
    fn request(&mut self, op: &str, snapshot: &Value, args: &Value) -> Result<Value, String> {
        let worker = self
            .worker
            .as_mut()
            .ok_or_else(|| "broker stream lease has no worker".to_owned())?;
        let fingerprint = topology_fingerprint(snapshot);
        let mut frame = json!({
            "op": op,
            "seq": worker.next_seq,
            "args": args,
        });
        let include_snapshot = fingerprint != worker.fingerprint;
        if include_snapshot {
            frame["snapshot"] = snapshot.clone();
        }
        let bytes = match serde_json::to_vec(&frame) {
            Ok(bytes) => bytes,
            Err(error) => return Err(format!("cannot serialize stream frame: {error}")),
        };
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(format!("broker request exceeds {MAX_REQUEST_BYTES} bytes"));
        }
        // One deadline covers the write AND the reply: a stuck read end used
        // to pin this thread in write_all forever with no kill and no slot
        // return (P1, review 7003833175).
        let deadline = std::time::Instant::now() + self.pool.request_timeout;
        let mut payload = bytes;
        payload.push(b'\n');
        if let Err(error) = write_stream_request(&worker.stdin, &payload, deadline) {
            worker.kill();
            return Err(format!("broker stream worker write failed: {error}"));
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match read_stream_reply(&worker.stdout, remaining) {
            Ok(Some(line)) => {
                let reply: Value = serde_json::from_slice(&line).map_err(|error| {
                    worker.kill();
                    format!("invalid broker stream reply JSON: {error}")
                })?;
                if reply.get("seq").and_then(Value::as_i64) != Some(worker.next_seq as i64) {
                    worker.kill();
                    return Err("broker stream reply seq mismatch".to_owned());
                }
                worker.next_seq += 1;
                worker.fingerprint = fingerprint;
                worker.last_used = std::time::Instant::now();
                reply.get("result").cloned().ok_or_else(|| {
                    worker.kill();
                    "broker stream reply missing result".to_owned()
                })
            }
            Ok(None) => {
                worker.kill();
                Err("broker stream worker closed or produced an oversized reply".to_owned())
            }
            Err(error) => {
                worker.kill();
                Err(format!("broker request timed out: {error}"))
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for PoolLease {
    fn drop(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        let reaped = {
            let mut state = self.pool.state.lock().unwrap_or_else(|e| e.into_inner());
            if worker.healthy {
                state.idle.push(worker);
                None
            } else {
                // Worker was killed or already dead: free its slot for a
                // rebuild, but terminate it after releasing the lock — the
                // SIGTERM grace can take ~500ms and must not stall other
                // leases (P1, review 6997783732).
                state.live = state.live.saturating_sub(1);
                Some(worker)
            }
        };
        drop(reaped);
        self.pool.ready.notify_one();
    }
}

/// Process-resident pool slot. Kept inside a `Mutex` (never an immutable
/// `OnceLock`) so a reinstalled or upgraded broker binary — detected by its
/// `(dev, ino)` identity — swaps in a fresh pool instead of serving stale
/// resident workers or a permanently latched `unsupported` verdict.
#[cfg(target_os = "macos")]
static STREAM_POOL: std::sync::Mutex<Option<std::sync::Arc<StreamPool>>> =
    std::sync::Mutex::new(None);

/// The pool serving the current on-disk broker, rebuilding on identity drift.
#[cfg(target_os = "macos")]
fn current_stream_pool(broker: &Path) -> Result<std::sync::Arc<StreamPool>, String> {
    let id = broker_file_id(broker)?;
    let mut slot = STREAM_POOL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pool) = slot.as_ref()
        && pool.broker == broker
        && pool.broker_id == Some(id)
    {
        return Ok(pool.clone());
    }
    // Replacing the pool drops the old Arc; its resident workers keep running
    // until outstanding leases drain, then `StreamWorker::drop` reaps them.
    let pool = std::sync::Arc::new(StreamPool::new(broker, Some(id)));
    *slot = Some(pool.clone());
    Ok(pool)
}

/// Serve one fs/git call through the resident stream worker pool. `Ok(None)`
/// means the installed broker cannot stream and the caller must use the
/// one-shot child path.
#[cfg(target_os = "macos")]
fn run_broker_stream_pool(
    broker: &Path,
    op: &str,
    snapshot: &Value,
    args: &Value,
) -> Result<Option<Value>, String> {
    let pool = current_stream_pool(broker)?;
    let Some(mut lease) = pool.acquire()? else {
        return Ok(None);
    };
    lease.request(op, snapshot, args).map(Some)
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
    fn outdated_broker_denial_carries_the_operational_root_upgrade_reason() {
        let dir = temp_dir("operational-upgrade-reason");
        let config = dir.join("config");
        let target = broker_path(&config);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"stable-broker").unwrap();
        let denial = json!({"ok": false, "reason": "outside_managed_roots", "path": "/tmp/x"});

        // Revision 3 predates operational-root support: the denial explains why.
        fs::write(
            broker_metadata_path(&config),
            format!(
                "{{\"schema_version\":1,\"compat_revision\":3,\"preferred_signing_identifier\":\"{BROKER_SIGNING_IDENTIFIER}\"}}"
            ),
        )
        .unwrap();
        assert_eq!(installed_compat_revision(&config), Some(3));
        assert_eq!(
            OPERATIONAL_ROOT_MIN_COMPAT_REVISION, 4,
            "revision 4 is the first broker that serves operational roots"
        );
        let annotated = annotate_operational_root_upgrade_with(denial.clone(), &config);
        assert_eq!(annotated["reason"], "outside_managed_roots");
        assert_eq!(annotated["broker_compat_revision"], 3);
        assert!(
            annotated["broker_upgrade_reason"]
                .as_str()
                .unwrap()
                .contains("upgrade-broker")
        );

        // A current broker adds nothing, and non-denials are untouched.
        fs::write(
            broker_metadata_path(&config),
            format!(
                "{{\"schema_version\":1,\"compat_revision\":{BROKER_COMPAT_REVISION},\"preferred_signing_identifier\":\"{BROKER_SIGNING_IDENTIFIER}\"}}"
            ),
        )
        .unwrap();
        assert_eq!(
            annotate_operational_root_upgrade_with(denial.clone(), &config),
            denial
        );
        let success = json!({"ok": true, "content": "x"});
        assert_eq!(
            annotate_operational_root_upgrade_with(success.clone(), &config),
            success
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn herdr_host_capability_requires_revision_two_or_newer() {
        let dir = temp_dir("host-capability");
        let config = dir.join("config");
        let target = broker_path(&config);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"stable-broker").unwrap();
        fs::write(
            broker_metadata_path(&config),
            format!(
                "{{\"schema_version\":1,\"compat_revision\":1,\"preferred_signing_identifier\":\"{BROKER_SIGNING_IDENTIFIER}\"}}"
            ),
        )
        .unwrap();
        assert!(!installed_supports_herdr_host(&config));

        fs::write(
            broker_metadata_path(&config),
            format!(
                "{{\"schema_version\":1,\"compat_revision\":{HERDR_HOST_MIN_COMPAT_REVISION},\"preferred_signing_identifier\":\"{BROKER_SIGNING_IDENTIFIER}\"}}"
            ),
        )
        .unwrap();
        assert!(installed_supports_herdr_host(&config));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn disclaimed_broker_spawn_round_trips_json() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("disclaimed-spawn");
        let broker = dir.join("broker.sh");
        fs::write(&broker, b"#!/bin/sh\ncat\n").unwrap();
        fs::set_permissions(&broker, fs::Permissions::from_mode(0o700)).unwrap();
        let input = json!({"ok": true, "source": "disclaimed-test"});
        let output = run_broker_child(&broker, &serde_json::to_vec(&input).unwrap()).unwrap();
        assert_eq!(output, input);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_broker_revision_preserves_existing_identity_across_runtime_byte_changes() {
        let dir = temp_dir("install");
        let config = dir.join("config");
        let target = broker_path(&config);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"existing-broker-bytes").unwrap();
        fs::write(
            broker_metadata_path(&config),
            format!(
                "{{\"schema_version\":1,\"compat_revision\":{BROKER_COMPAT_REVISION},\"preferred_signing_identifier\":\"{BROKER_SIGNING_IDENTIFIER}\"}}"
            ),
        )
        .unwrap();

        // Runtime rebuilds can have different bytes without implying a broker
        // upgrade when the installed compatibility revision is still current.
        let result = install(&config, false);
        assert!(result.is_ok());
        assert_eq!(fs::read(&target).unwrap(), b"existing-broker-bytes");
        assert_eq!(
            installed_compat_revision(&config),
            Some(BROKER_COMPAT_REVISION)
        );
        assert!(broker_metadata_path(&config).is_file());

        // Even an explicit setup pass cannot replace the identity when the
        // broker implementation revision did not change.
        let result = install(&config, true);
        assert!(result.is_ok());
        assert_eq!(fs::read(&target).unwrap(), b"existing-broker-bytes");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_revision_one_requires_explicit_upgrade_after_host_revision_advance() {
        let dir = temp_dir("legacy-revision");
        let config = dir.join("config");
        let target = broker_path(&config);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"legacy-v1-broker").unwrap();

        assert_eq!(installed_compat_revision(&config), Some(1));
        let error = install(&config, false).unwrap_err();
        assert!(error.contains("revision 1 is installed"));
        assert!(error.contains("permissions setup --upgrade-broker"));
        assert_eq!(fs::read(&target).unwrap(), b"legacy-v1-broker");
        assert!(!broker_metadata_path(&config).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_reauthorization_migration_preserves_old_broker_until_verify() {
        let _guard = crate::test_env::lock();
        let dir = temp_dir("explicit-reauthorization");
        let config = dir.join("config");
        let target = broker_path(&config);
        let candidate = dir.join("candidate");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"revision-one-broker").unwrap();
        fs::write(&candidate, b"revision-two-broker").unwrap();
        let previous_candidate = std::env::var_os("HERDR_MCP_TCC_BROKER_CANDIDATE");
        unsafe {
            std::env::set_var("HERDR_MCP_TCC_BROKER_CANDIDATE", &candidate);
        }

        assert!(migrate_for_explicit_reauthorization(&config).unwrap());
        assert_eq!(fs::read(&target).unwrap(), b"revision-two-broker");
        assert_eq!(
            fs::read(previous_broker_path(&config)).unwrap(),
            b"revision-one-broker"
        );
        assert_eq!(
            installed_compat_revision(&config),
            Some(BROKER_COMPAT_REVISION)
        );

        finalize_explicit_reauthorization(&config).unwrap();
        assert!(!previous_broker_path(&config).exists());
        assert!(!previous_broker_metadata_path(&config).exists());

        unsafe {
            match previous_candidate {
                Some(value) => std::env::set_var("HERDR_MCP_TCC_BROKER_CANDIDATE", value),
                None => std::env::remove_var("HERDR_MCP_TCC_BROKER_CANDIDATE"),
            }
        }
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

    // ------------------------------------------------------------------
    // #1068 streaming broker: frame protocol, snapshot retention, seq
    // isolation, protocol-error resilience and legacy fallback behaviour.
    // ------------------------------------------------------------------

    fn drive_stream(input: &[&str]) -> Result<Vec<String>, String> {
        let mut reader = std::io::Cursor::new(input.concat().into_bytes());
        let mut writer = Vec::new();
        broker_stream_loop(&mut reader, &mut writer)?;
        let text = String::from_utf8(writer).map_err(|error| error.to_string())?;
        Ok(text.lines().map(str::to_owned).collect())
    }

    #[test]
    fn stream_protocol_requests_reuse_last_snapshot_across_frames() {
        let root = temp_dir("stream-reuse");
        init_git_repo(&root);
        let file = root.join("a.txt");
        fs::write(&file, "hello\n").unwrap();
        let snapshot = make_snapshot(&root).to_string();
        let path_json = serde_json::to_string(&file.to_string_lossy().to_string()).unwrap();

        let frames = [
            format!(
                "{{\"protocol\":\"{STREAM_PROTOCOL}\",\"version\":{STREAM_PROTOCOL_VERSION}}}\n"
            ),
            format!(
                "{{\"op\":\"fs_read\",\"seq\":0,\"args\":{{\"path\":{path_json}}},\"snapshot\":{snapshot}}}\n"
            ),
            // Same topology: no snapshot in this frame — the resident worker
            // must reuse the last delivered one.
            format!("{{\"op\":\"fs_read\",\"seq\":1,\"args\":{{\"path\":{path_json}}}}}\n"),
        ];
        let refs: Vec<&str> = frames.iter().map(String::as_str).collect();
        let replies = drive_stream(&refs).unwrap();
        assert_eq!(replies.len(), 3);
        let hello: Value = serde_json::from_str(&replies[0]).unwrap();
        assert_eq!(hello["op"], "hello_ok");
        for (index, expected_seq) in [(1usize, 0i64), (2, 1)] {
            let reply: Value = serde_json::from_str(&replies[index]).unwrap();
            assert_eq!(reply["seq"], expected_seq);
            assert_eq!(reply["result"]["ok"], true, "{replies:?}");
            assert_eq!(reply["result"]["content"], "hello\n");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stream_protocol_rejects_bad_op_missing_snapshot_and_seq_gap() {
        let root = temp_dir("stream-errors");
        init_git_repo(&root);
        let snapshot = make_snapshot(&root).to_string();
        let path_json =
            serde_json::to_string(&root.join("x").to_string_lossy().to_string()).unwrap();
        let frames = [
            format!(
                "{{\"protocol\":\"{STREAM_PROTOCOL}\",\"version\":{STREAM_PROTOCOL_VERSION}}}\n"
            ),
            // Non-allowlisted op: protocol_error result, stream stays alive.
            "{\"op\":\"shell\",\"seq\":0,\"args\":{}}\n".to_owned(),
            // No stored snapshot and none inline: missing_snapshot.
            format!("{{\"op\":\"fs_read\",\"seq\":0,\"args\":{{\"path\":{path_json}}}}}\n"),
            // Deliver the topology.
            format!(
                "{{\"op\":\"fs_read\",\"seq\":0,\"args\":{{\"path\":{path_json}}},\"snapshot\":{snapshot}}}\n"
            ),
            // Gap: expected seq 1, got 5 → resynchronizing protocol_error.
            format!("{{\"op\":\"fs_read\",\"seq\":5,\"args\":{{\"path\":{path_json}}}}}\n"),
            // After resync (expected becomes 6), seq 6 reuses the stored
            // snapshot — outside_managed_roots proves topology was applied.
            format!("{{\"op\":\"fs_read\",\"seq\":6,\"args\":{{\"path\":{path_json}}}}}\n"),
        ];
        let refs: Vec<&str> = frames.iter().map(String::as_str).collect();
        let replies = drive_stream(&refs).unwrap();
        assert_eq!(replies.len(), 6);
        let op_err: Value = serde_json::from_str(&replies[1]).unwrap();
        assert_eq!(op_err["seq"], 0);
        assert_eq!(op_err["result"]["ok"], false);
        assert_eq!(op_err["result"]["code"], "protocol_error");
        let missing: Value = serde_json::from_str(&replies[2]).unwrap();
        assert_eq!(missing["result"]["code"], "missing_snapshot");
        let delivered: Value = serde_json::from_str(&replies[3]).unwrap();
        assert_eq!(delivered["seq"], 0);
        let gap: Value = serde_json::from_str(&replies[4]).unwrap();
        assert_eq!(gap["seq"], 5);
        assert_eq!(gap["result"]["code"], "protocol_error");
        let resynced: Value = serde_json::from_str(&replies[5]).unwrap();
        assert_eq!(resynced["seq"], 6);
        // `not_found` proves the managed-root gate passed on the stored
        // snapshot — an empty topology would deny as outside_managed_roots.
        assert_eq!(resynced["result"]["reason"], "not_found", "{resynced}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stream_protocol_bad_hello_and_eof_fail_closed() {
        let mut reader = std::io::Cursor::new(b"{\"op\":\"fs_read\"}\n".to_vec());
        let mut writer = Vec::new();
        assert!(broker_stream_loop(&mut reader, &mut writer).is_err());
        let reply: Value = serde_json::from_str(String::from_utf8(writer).unwrap().trim()).unwrap();
        assert_eq!(reply["result"]["code"], "protocol_error");

        let mut reader = std::io::Cursor::new(Vec::<u8>::new());
        let mut writer = Vec::new();
        assert!(broker_stream_loop(&mut reader, &mut writer).is_err());
        assert!(writer.is_empty());
    }

    #[test]
    fn stream_topology_fingerprint_tracks_structural_equality() {
        let a = json!({"w": [{"id": 1}], "p": ["a", "b"]});
        let a_shuffled = json!({"p": ["a", "b"], "w": [{"id": 1}]});
        let b = json!({"w": [{"id": 2}], "p": ["a", "b"]});
        assert_eq!(topology_fingerprint(&a), topology_fingerprint(&a_shuffled));
        assert_ne!(topology_fingerprint(&a), topology_fingerprint(&b));
        assert_ne!(topology_fingerprint(&a), topology_fingerprint(&json!(null)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn stream_pool_falls_back_permanently_when_broker_cannot_stream() {
        // /bin/cat echoes stdin but never emits hello_ok — the handshake must
        // classify it as an old broker and latch permanent fallback instead of
        // retry-spawning on every request.
        let pool = std::sync::Arc::new(StreamPool::new(Path::new("/bin/cat"), None));
        assert!(pool.clone().acquire().unwrap().is_none());
        assert!(pool.clone().acquire().unwrap().is_none());
        let state = pool.state.lock().unwrap();
        assert!(state.unsupported);
        assert_eq!(state.live, 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn stream_reply_read_enforces_deadline_and_dead_worker_frees_slot() {
        // A held-open write end that never sends: poll must surface the
        // deadline without a per-request thread.
        let (read_end, write_end) = stream_pipe_pair().unwrap();
        let start = std::time::Instant::now();
        let reply = read_stream_reply(&read_end, Duration::from_millis(200));
        assert!(
            reply.is_err(),
            "a silent worker must hit the request deadline"
        );
        // Instant vs poll's deadline can disagree by the millisecond floor —
        // assert the deadline fired, not an exact wall duration.
        assert!(
            start.elapsed() >= Duration::from_millis(190),
            "returned far too early: {reply:?} elapsed={:?}",
            start.elapsed()
        );
        drop(write_end);

        // Dropping a lease whose worker was marked dead frees its spawn slot —
        // that is the rebuild path after a timeout kill.
        let pool = std::sync::Arc::new(StreamPool::new(Path::new("/bin/cat"), None));
        {
            let mut state = pool.state.lock().unwrap();
            state.live = 1;
        }
        let dead = StreamWorker {
            // Nonexistent pid: terminate_pid_group fails fast (ESRCH/ECHILD)
            // and never touches a real process.
            pid: i32::MAX - 8,
            stdin: stream_pipe_pair().unwrap().1,
            stdout: stream_pipe_pair().unwrap().0,
            _registration: crate::child_process::register_owned_pid("test", 0),
            last_used: std::time::Instant::now(),
            fingerprint: 0,
            next_seq: 0,
            healthy: false,
        };
        drop(PoolLease {
            pool: pool.clone(),
            worker: Some(dead),
        });
        let state = pool.state.lock().unwrap();
        assert_eq!(state.live, 0, "dead worker lease frees its pool slot");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn stream_pool_acquire_frees_stale_slots_alongside_lease() {
        // last_used is not stack-ordered: PoolLease::request refreshes it
        // unlocked, so an idle entry above a fresh one may already be stale.
        // When one scan pops stale then leases, the stale slots must still be
        // freed — otherwise live reaches STREAM_POOL_MAX_WORKERS and acquire
        // deadlocks on `ready` forever (P1, review 7003213891).
        fn worker(last_used: std::time::Instant, pid: i32) -> StreamWorker {
            StreamWorker {
                // Nonexistent pid: terminate_pid_group fails fast and never
                // touches a real process.
                pid,
                stdin: stream_pipe_pair().unwrap().1,
                stdout: stream_pipe_pair().unwrap().0,
                _registration: crate::child_process::register_owned_pid("test", 0),
                last_used,
                fingerprint: 0,
                next_seq: 0,
                healthy: true,
            }
        }
        let fresh_pid = i32::MAX - 8;
        let stale_pid = i32::MAX - 9;
        let pool = std::sync::Arc::new(StreamPool::new(Path::new("/bin/cat"), None));
        {
            let mut state = pool.state.lock().unwrap();
            state.live = 2;
            state
                .idle
                .push(worker(std::time::Instant::now(), fresh_pid));
            state.idle.push(worker(
                std::time::Instant::now() - STREAM_WORKER_IDLE_TIMEOUT - Duration::from_secs(1),
                stale_pid,
            ));
        }
        let lease = pool.clone().acquire().unwrap().unwrap();
        assert_eq!(
            lease.worker.as_ref().unwrap().pid,
            fresh_pid,
            "the fresh worker must be leased, not reaped"
        );
        {
            let state = pool.state.lock().unwrap();
            assert_eq!(
                state.live, 1,
                "the stale slot must be freed in the same scan that leases"
            );
            assert!(state.idle.is_empty());
        }
        drop(lease);
        let state = pool.state.lock().unwrap();
        assert_eq!(state.live, 1);
        assert_eq!(state.idle.len(), 1, "returned worker rejoins idle");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn stream_request_write_deadline_kills_stuck_worker_and_frees_slot() {
        // A worker that keeps its read end open but stops draining must not
        // pin the caller in an unbounded write_all: the request frame (up to
        // MAX_REQUEST_BYTES) far exceeds the pipe buffer, so the write must
        // hit the shared deadline, kill the worker, and return its pool slot
        // (P1, review 7003833175).
        let (stdin_read, stdin_write) = stream_pipe_pair().unwrap();
        let (stdout_read, _stdout_write) = stream_pipe_pair().unwrap();
        // Fill the pipe buffer so the request write cannot complete: the read
        // end stays open (worker alive but wedged), matching the review's
        // reproduction.
        unsafe {
            let fd = std::os::fd::AsRawFd::as_raw_fd(&stdin_write);
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        let filler = vec![b'x'; 1 << 20];
        let mut filled = 0;
        loop {
            let count = unsafe {
                libc::write(
                    std::os::fd::AsRawFd::as_raw_fd(&stdin_write),
                    filler.as_ptr().cast(),
                    filler.len(),
                )
            };
            if count <= 0 {
                break;
            }
            filled += count as usize;
        }
        assert!(filled > 0, "pipe buffer must be full before the request");
        let worker = StreamWorker {
            // Nonexistent pid: kill/terminate fails fast and never touches a
            // real process.
            pid: i32::MAX - 8,
            stdin: stdin_write,
            stdout: stdout_read,
            _registration: crate::child_process::register_owned_pid("test", 0),
            last_used: std::time::Instant::now(),
            fingerprint: 0,
            next_seq: 0,
            healthy: true,
        };
        let mut pool = StreamPool::new(Path::new("/bin/cat"), None);
        pool.request_timeout = Duration::from_millis(200);
        let pool = std::sync::Arc::new(pool);
        {
            let mut state = pool.state.lock().unwrap();
            state.live = 1;
        }
        let mut lease = PoolLease {
            pool: pool.clone(),
            worker: Some(worker),
        };
        let start = std::time::Instant::now();
        let result = lease.request("exec", &json!({}), &json!({"text": "x".repeat(1 << 20)}));
        assert!(result.is_err(), "a wedged worker must fail the request");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "write must return bounded by the shared deadline: {:?}",
            start.elapsed()
        );
        drop(lease);
        drop(stdin_read);
        let state = pool.state.lock().unwrap();
        assert_eq!(
            state.live, 0,
            "the killed worker's concurrency slot must be freed"
        );
    }

    #[test]
    fn stream_frame_too_large_fallback_keeps_seq_and_result() {
        // A serialized reply above MAX_RESPONSE_BYTES must degrade to a
        // bounded {seq, result} frame — the bare broker_error used to drop
        // `seq`, which the parent reads as a protocol desync and kills the
        // healthy worker over (P2, review 6997783732).
        let huge = "x".repeat(MAX_RESPONSE_BYTES);
        let oversized = json!({ "seq": 7, "result": { "ok": true, "content": huge } });
        let mut out = Vec::new();
        write_stream_frame(&mut out, &oversized).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.ends_with('\n'));
        assert!(
            text.len() < 4096,
            "fallback must stay bounded: {}",
            text.len()
        );
        let frame: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(frame["seq"], 7);
        assert_eq!(frame["result"]["ok"], false);
        assert_eq!(frame["result"]["code"], "response_too_large");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn stream_pool_refuses_after_broker_binary_replaced() {
        // The pool keys on (dev, ino): once the installed broker is swapped
        // (upgrade, reinstall, uninstall), acquire must decline so the caller
        // runs the one-shot path and the next call rebuilds the pool — never
        // serve from workers resident on the stale binary.
        let dir = temp_dir("pool-identity");
        let broker = dir.join("broker");
        fs::write(&broker, b"rev-a").unwrap();
        let id = broker_file_id(&broker).unwrap();
        let pool = std::sync::Arc::new(StreamPool::new(&broker, Some(id)));

        fs::remove_file(&broker).unwrap();
        fs::write(&broker, b"rev-b").unwrap();
        let new_id = broker_file_id(&broker).unwrap();
        assert_ne!(new_id, id, "replace must change the file identity");
        assert!(
            pool.acquire().unwrap().is_none(),
            "pool built against the old binary must decline"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn current_stream_pool_rebuilds_on_disk_identity_change() {
        let dir = temp_dir("pool-rebuild");
        let broker = dir.join("broker");
        fs::write(&broker, b"rev-a").unwrap();
        let first = current_stream_pool(&broker).unwrap();
        let again = current_stream_pool(&broker).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &again));

        fs::remove_file(&broker).unwrap();
        fs::write(&broker, b"rev-b").unwrap();
        let rebuilt = current_stream_pool(&broker).unwrap();
        assert!(
            !std::sync::Arc::ptr_eq(&first, &rebuilt),
            "disk replacement must swap in a fresh pool"
        );
        fs::remove_dir_all(&dir).unwrap();
        // Leave the process slot clean for other tests.
        let mut slot = STREAM_POOL.lock().unwrap();
        *slot = None;
    }
}
