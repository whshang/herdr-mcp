//! Auditable `production_ready` seal for G5 Link cutover (P0-6).
//!
//! The seal is an operator-written evidence artifact under
//! `~/.config/herdr-mcp/seals/`. LaunchAgent ownership alone never flips
//! `production_ready`. Deliberate Node rollback clears the active seal.

use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use super::cutover::prod_plist_backup_path;
use super::cutover_execute::validate_node_rollback_source;
use super::ownership::{
    LINK_LABEL, LINK_PROD_LABEL, assess_agent, collect_status_report,
    evaluate_production_ready_gates,
};
use super::run::LINK_RUN_WIRED;

/// Env guard required before `link seal --execute`.
pub const SEAL_EXECUTE_ENV: &str = "HERDR_LINK_SEAL_I_UNDERSTAND";

const SEAL_SCHEMA_VERSION: u64 = 1;
const ACTIVE_SEAL_NAME: &str = "link-production-ready.json";
const ADOPTION_EVIDENCE_NAME: &str = "irreversible-rust-adoption.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealMode {
    Status,
    RecordDualUat,
    RecordRollbackUat,
    AdoptExistingRust { acknowledged: bool, reason: String },
    DryRun,
    Execute,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealEvidence {
    pub dual_uat_recorded: bool,
    pub rollback_uat_recorded: bool,
    pub active_seal: Option<Value>,
}

/// CLI entry for `herdr-mcp link seal ...`.
pub fn run(mode: SealMode) -> Result<ExitCode, String> {
    let home = home_dir().ok_or_else(|| "HOME is required for link seal".to_owned())?;
    let config_dir = home.join(".config").join("herdr-mcp");
    match mode {
        SealMode::Status => {
            let report = status_report(&home, &config_dir)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
            );
            Ok(ExitCode::SUCCESS)
        }
        SealMode::RecordDualUat => {
            let note = env::var("HERDR_LINK_SEAL_NOTE").unwrap_or_else(|_| {
                "dual self-UAT recorded by operator (independent Shell)".to_owned()
            });
            let path = record_evidence(&config_dir, "dual-uat", &note)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "action": "record_dual_uat",
                    "path": path.display().to_string(),
                    "note": note,
                }))
                .map_err(|error| error.to_string())?
            );
            Ok(ExitCode::SUCCESS)
        }
        SealMode::RecordRollbackUat => {
            let note = env::var("HERDR_LINK_SEAL_NOTE").unwrap_or_else(|_| {
                "deliberate Node rollback UAT recorded by operator (independent Shell)".to_owned()
            });
            let path = record_evidence(&config_dir, "rollback-uat", &note)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "action": "record_rollback_uat",
                    "path": path.display().to_string(),
                    "note": note,
                }))
                .map_err(|error| error.to_string())?
            );
            Ok(ExitCode::SUCCESS)
        }
        SealMode::AdoptExistingRust {
            acknowledged,
            reason,
        } => {
            let report = adopt_existing_rust(&home, &config_dir, acknowledged, &reason)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
            );
            if report.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(2))
            }
        }
        SealMode::DryRun => {
            let report = plan_seal(&home, &config_dir, false)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
            );
            if report
                .get("ready_for_seal")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(2))
            }
        }
        SealMode::Execute => {
            let understood = env::var_os(SEAL_EXECUTE_ENV)
                .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
            if !understood {
                let report = json!({
                    "ok": false,
                    "mode": "execute",
                    "error": format!(
                        "link seal --execute is refused without {SEAL_EXECUTE_ENV}=1"
                    ),
                    "notes": [
                        "No seal mutation occurred.",
                        "Record dual-uat plus rollback-uat or explicit irreversible adoption evidence first.",
                        "Then: HERDR_LINK_SEAL_I_UNDERSTAND=1 herdr-mcp link seal --execute",
                    ],
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
                );
                return Ok(ExitCode::from(2));
            }
            let report = execute_seal(&home, &config_dir)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
            );
            if report.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(2))
            }
        }
    }
}

pub fn seals_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("seals")
}

pub fn evidence_dir(config_dir: &Path) -> PathBuf {
    seals_dir(config_dir).join("evidence")
}

pub fn active_seal_path(config_dir: &Path) -> PathBuf {
    seals_dir(config_dir).join(ACTIVE_SEAL_NAME)
}

pub fn read_active_seal(config_dir: &Path) -> Option<Value> {
    let path = active_seal_path(config_dir);
    let bytes = fs::read(&path).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    if value.get("production_ready").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    if value.get("schema_version").and_then(Value::as_u64) != Some(SEAL_SCHEMA_VERSION) {
        return None;
    }
    Some(value)
}

pub fn production_ready_from_seal(config_dir: &Path) -> bool {
    read_active_seal(config_dir).is_some()
}

pub fn dual_uat_evidence_present(config_dir: &Path) -> bool {
    evidence_dir(config_dir).join("dual-uat.json").is_file()
}

pub fn rollback_uat_evidence_present(config_dir: &Path) -> bool {
    evidence_dir(config_dir).join("rollback-uat.json").is_file()
}

pub fn irreversible_adoption_evidence_path(config_dir: &Path) -> PathBuf {
    evidence_dir(config_dir).join(ADOPTION_EVIDENCE_NAME)
}

pub fn read_irreversible_adoption_evidence(config_dir: &Path) -> Option<Value> {
    let bytes = fs::read(irreversible_adoption_evidence_path(config_dir)).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(SEAL_SCHEMA_VERSION)
        || value.get("kind").and_then(Value::as_str) != Some("irreversible-rust-adoption")
        || value.get("rollback_available").and_then(Value::as_bool) != Some(false)
    {
        return None;
    }
    Some(value)
}

pub fn irreversible_adoption_evidence_present(config_dir: &Path) -> bool {
    read_irreversible_adoption_evidence(config_dir).is_some()
}

fn rollback_source_status(home: &Path) -> Value {
    let path = prod_plist_backup_path(home);
    match validate_node_rollback_source(&path) {
        Ok(program) => json!({
            "rollback_available": true,
            "path": path.display().to_string(),
            "program_arguments": program,
        }),
        Err(error) => json!({
            "rollback_available": false,
            "path": path.display().to_string(),
            "error": error,
        }),
    }
}

/// Clear the active seal (used by deliberate Node rollback). Keeps versioned copies.
pub fn clear_active_seal(config_dir: &Path) -> Result<Value, String> {
    let path = active_seal_path(config_dir);
    if !path.is_file() {
        return Ok(json!({
            "ok": true,
            "cleared": false,
            "detail": "no active seal present",
        }));
    }
    let bytes = fs::read(&path)
        .map_err(|error| format!("cannot read active seal {}: {error}", path.display()))?;
    let stamp = now_stamp();
    let archive = seals_dir(config_dir).join(format!("link-production-ready.cleared-{stamp}.json"));
    if let Some(parent) = archive.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    fs::write(&archive, &bytes)
        .map_err(|error| format!("cannot archive seal {}: {error}", archive.display()))?;
    fs::remove_file(&path)
        .map_err(|error| format!("cannot remove active seal {}: {error}", path.display()))?;
    Ok(json!({
        "ok": true,
        "cleared": true,
        "archived": archive.display().to_string(),
    }))
}

fn record_evidence(config_dir: &Path, kind: &str, note: &str) -> Result<PathBuf, String> {
    let dir = evidence_dir(config_dir);
    fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let path = dir.join(format!("{kind}.json"));
    let body = json!({
        "schema_version": SEAL_SCHEMA_VERSION,
        "kind": kind,
        "recorded_at": now_rfc3339(),
        "recorded_at_ms": now_ms(),
        "note": note,
        "source": "herdr-mcp link seal record",
    });
    atomic_write(
        &path,
        serde_json::to_vec_pretty(&body).map_err(|error| error.to_string())?,
        0o600,
    )?;
    Ok(path)
}

fn adopt_existing_rust(
    home: &Path,
    config_dir: &Path,
    acknowledged: bool,
    reason: &str,
) -> Result<Value, String> {
    let status = collect_status_report(home, config_dir);
    adopt_existing_rust_with_status(home, config_dir, acknowledged, reason, &status)
}

fn adopt_existing_rust_with_status(
    home: &Path,
    config_dir: &Path,
    acknowledged: bool,
    reason: &str,
    status: &Value,
) -> Result<Value, String> {
    if let Some(existing) = read_irreversible_adoption_evidence(config_dir) {
        return Ok(json!({
            "ok": true,
            "action": "adopt_existing_rust",
            "recorded": false,
            "already_recorded": true,
            "rollback_available": false,
            "evidence": existing,
        }));
    }

    let reason = reason.trim();
    let rollback = rollback_source_status(home);
    let alignment = status
        .get("production_runtime_alignment")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut blockers = Vec::new();
    if !acknowledged {
        blockers.push("explicit_acknowledgement_required");
    }
    if reason.is_empty() {
        blockers.push("operator_reason_required");
    }
    if production_ready_from_seal(config_dir) {
        blockers.push("already_sealed");
    }
    if rollback_uat_evidence_present(config_dir) {
        blockers.push("rollback_uat_evidence_already_present");
    }
    if status.get("production_owner").and_then(Value::as_str) != Some("rust") {
        blockers.push("production_owner_not_loaded_rust");
    }
    if status.get("operational_ready").and_then(Value::as_bool) != Some(true) {
        blockers.push("data_plane_not_operationally_ready");
    }
    for key in [
        "desired_matches_current",
        "runtime_control_active_matches_current",
        "loaded_matches_current",
    ] {
        if alignment.get(key).and_then(Value::as_bool) != Some(true) {
            blockers.push("runtime_generation_not_aligned");
            break;
        }
    }
    if rollback.get("rollback_available").and_then(Value::as_bool) == Some(true) {
        blockers.push("valid_node_rollback_backup_exists");
    }

    if !blockers.is_empty() {
        return Ok(json!({
            "ok": false,
            "action": "adopt_existing_rust",
            "recorded": false,
            "blockers": blockers,
            "production_owner": status.get("production_owner").cloned(),
            "operational_ready": status.get("operational_ready").cloned(),
            "runtime_alignment": alignment,
            "rollback": rollback,
        }));
    }

    let path = irreversible_adoption_evidence_path(config_dir);
    let evidence = json!({
        "schema_version": SEAL_SCHEMA_VERSION,
        "kind": "irreversible-rust-adoption",
        "recorded_at": now_rfc3339(),
        "recorded_at_ms": now_ms(),
        "reason": reason,
        "acknowledgement": "operator explicitly accepts that no valid Node rollback baseline remains",
        "rollback_available": false,
        "rollback_source": rollback,
        "production_owner": "rust",
        "operational_ready": true,
        "runtime_alignment": alignment,
        "source": "herdr-mcp link seal adopt-existing-rust",
    });
    atomic_write(
        &path,
        serde_json::to_vec_pretty(&evidence).map_err(|error| error.to_string())?,
        0o600,
    )?;
    Ok(json!({
        "ok": true,
        "action": "adopt_existing_rust",
        "recorded": true,
        "rollback_available": false,
        "path": path.display().to_string(),
        "evidence": evidence,
    }))
}

fn status_report(home: &Path, config_dir: &Path) -> Result<Value, String> {
    let plan = plan_seal(home, config_dir, false)?;
    let rollback = rollback_source_status(home);
    let adoption = read_irreversible_adoption_evidence(config_dir);
    Ok(json!({
        "ok": true,
        "action": "seal_status",
        "production_ready": production_ready_from_seal(config_dir),
        "active_seal_path": active_seal_path(config_dir).display().to_string(),
        "dual_uat_recorded": dual_uat_evidence_present(config_dir),
        "rollback_uat_recorded": rollback_uat_evidence_present(config_dir),
        "rollback_available": rollback.get("rollback_available").cloned().unwrap_or(Value::Bool(false)),
        "rollback_source": rollback,
        "irreversible_adoption_recorded": adoption.is_some(),
        "irreversible_adoption": adoption,
        "ready_for_seal": plan.get("ready_for_seal").cloned().unwrap_or(Value::Bool(false)),
        "blockers": plan.get("blockers").cloned().unwrap_or_else(|| json!([])),
        "active_seal": read_active_seal(config_dir),
    }))
}

fn plan_seal(home: &Path, config_dir: &Path, executing: bool) -> Result<Value, String> {
    let prod = assess_agent(home, LINK_PROD_LABEL, true);
    let link = assess_agent(home, LINK_LABEL, true);
    let gates = evaluate_production_ready_gates(home, config_dir, &prod, &link, LINK_RUN_WIRED);
    let dual = dual_uat_evidence_present(config_dir);
    let rollback = rollback_uat_evidence_present(config_dir);
    let adoption = irreversible_adoption_evidence_present(config_dir);
    let rollback_source = rollback_source_status(home);
    let rust_owner = prod.implementation.as_str() == "rust"
        && prod
            .program_arguments
            .first()
            .is_some_and(|p| p.contains("/.config/herdr-mcp/runtime/current/herdr-mcp"));

    let mut blockers = Vec::new();
    if !rust_owner {
        blockers.push("production_owner_not_rust".to_owned());
    }
    for gate in &gates {
        // Seal-owned gates are evaluated from evidence/seal files below.
        if matches!(
            gate.id.as_str(),
            "health_runtime_not_candidate" | "dual_verification_uat"
        ) {
            continue;
        }
        if !gate.ok {
            blockers.push(gate.id.clone());
        }
    }
    if !dual {
        blockers.push("dual_uat_evidence_missing".to_owned());
    }
    if !rollback && !adoption {
        blockers.push("rollback_or_adoption_evidence_missing".to_owned());
    }
    if production_ready_from_seal(config_dir) && !executing {
        blockers.push("already_sealed".to_owned());
    }

    Ok(json!({
        "ok": true,
        "mode": if executing { "execute" } else { "dry-run" },
        "ready_for_seal": blockers.is_empty(),
        "blockers": blockers,
        "production_owner": prod.implementation.as_str(),
        "dual_uat_recorded": dual,
        "rollback_uat_recorded": rollback,
        "irreversible_adoption_recorded": adoption,
        "rollback_available": rollback_source.get("rollback_available").cloned().unwrap_or(Value::Bool(false)),
        "rollback_source": rollback_source,
        "gates": gates.iter().map(|g| json!({
            "id": g.id,
            "ok": g.ok,
            "detail": g.detail,
        })).collect::<Vec<_>>(),
        "notes": [
            "Seal never auto-flips from LaunchAgent ownership alone.",
            "Record dual-uat plus either real rollback-uat evidence or explicit irreversible-rust-adoption evidence before --execute.",
            "link cutover --rollback clears the active seal.",
        ],
    }))
}

fn execute_seal(home: &Path, config_dir: &Path) -> Result<Value, String> {
    let plan = plan_seal(home, config_dir, true)?;
    if plan.get("ready_for_seal").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({
            "ok": false,
            "mode": "execute",
            "error": "seal criteria not met",
            "plan": plan,
        }));
    }

    let dual_path = evidence_dir(config_dir).join("dual-uat.json");
    let rollback_path = evidence_dir(config_dir).join("rollback-uat.json");
    let dual: Value = serde_json::from_slice(
        &fs::read(&dual_path).map_err(|error| format!("read dual-uat: {error}"))?,
    )
    .map_err(|error| error.to_string())?;
    let rollback = if rollback_path.is_file() {
        Some(
            serde_json::from_slice::<Value>(
                &fs::read(&rollback_path).map_err(|error| format!("read rollback-uat: {error}"))?,
            )
            .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let adoption = read_irreversible_adoption_evidence(config_dir);
    if rollback.is_none() && adoption.is_none() {
        return Err("seal plan passed without rollback or adoption evidence".to_owned());
    }
    let migration_evidence_kind = if rollback.is_some() {
        "rollback_uat"
    } else {
        "irreversible_adoption"
    };

    let generation = fs::read_link(
        home.join(".config")
            .join("herdr-mcp")
            .join("runtime")
            .join("current"),
    )
    .ok()
    .map(|target| target.display().to_string());

    let seal = json!({
        "schema_version": SEAL_SCHEMA_VERSION,
        "production_ready": true,
        "sealed_at": now_rfc3339(),
        "sealed_at_ms": now_ms(),
        "runtime_version": env!("CARGO_PKG_VERSION"),
        "generation": generation,
        "production_owner": "rust",
        "rollback_available": plan.get("rollback_available").cloned().unwrap_or(Value::Bool(false)),
        "migration_evidence_kind": migration_evidence_kind,
        "evidence": {
            "dual_uat": dual,
            "rollback_uat": rollback,
            "irreversible_adoption": adoption,
        },
        "notes": [
            "Auditable G5 seal. Cleared by link cutover --rollback.",
            "Rollback UAT and irreversible adoption remain distinct evidence classes.",
        ],
    });

    let dir = seals_dir(config_dir);
    fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let stamp = now_stamp();
    let versioned = dir.join(format!("link-production-ready-{stamp}.json"));
    let bytes = serde_json::to_vec_pretty(&seal).map_err(|error| error.to_string())?;
    atomic_write(&versioned, &bytes, 0o600)?;
    atomic_write(&active_seal_path(config_dir), &bytes, 0o600)?;

    Ok(json!({
        "ok": true,
        "mode": "execute",
        "production_ready": true,
        "active_seal": active_seal_path(config_dir).display().to_string(),
        "versioned_seal": versioned.display().to_string(),
        "seal": seal,
    }))
}

fn atomic_write(path: &Path, bytes: impl AsRef<[u8]>, mode: u32) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("seal"),
        std::process::id()
    ));
    fs::write(&tmp, bytes.as_ref())
        .map_err(|error| format!("cannot write {}: {error}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))
            .map_err(|error| format!("cannot chmod {}: {error}", tmp.display()))?;
    }
    fs::rename(&tmp, path).map_err(|error| {
        format!(
            "cannot rename {} -> {}: {error}",
            tmp.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn now_stamp() -> String {
    format!("{}", now_ms())
}

fn now_rfc3339() -> String {
    // Keep seal timestamps readable without pulling time formatting into this
    // module's critical path; ms + UTC marker is enough for audit correlation.
    format!("{}Z", now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aligned_rust_status() -> Value {
        json!({
            "production_owner": "rust",
            "operational_ready": true,
            "production_runtime_alignment": {
                "desired_matches_current": true,
                "runtime_control_active_matches_current": true,
                "loaded_matches_current": true,
            }
        })
    }

    fn write_valid_node_backup(home: &Path) {
        let node = home.join("node/bin/node");
        let daemon = home.join("legacy/link/macos-daemon.js");
        fs::create_dir_all(node.parent().unwrap()).unwrap();
        fs::create_dir_all(daemon.parent().unwrap()).unwrap();
        fs::write(&node, b"#!/bin/sh\n").unwrap();
        fs::write(&daemon, b"// legacy link\n").unwrap();
        let backup = prod_plist_backup_path(home);
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Label</key><string>{LINK_PROD_LABEL}</string>
<key>ProgramArguments</key><array>
<string>{}</string><string>{}</string>
</array></dict></plist>"#,
            node.display(),
            daemon.display()
        );
        fs::write(backup, xml).unwrap();
    }

    #[test]
    fn production_ready_requires_active_seal_file() {
        let dir = std::env::temp_dir().join(format!("herdr-seal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(evidence_dir(&dir)).unwrap();
        assert!(!production_ready_from_seal(&dir));
        let seal = json!({
            "schema_version": SEAL_SCHEMA_VERSION,
            "production_ready": true,
        });
        atomic_write(
            &active_seal_path(&dir),
            serde_json::to_vec_pretty(&seal).unwrap(),
            0o600,
        )
        .unwrap();
        assert!(production_ready_from_seal(&dir));
        let cleared = clear_active_seal(&dir).unwrap();
        assert_eq!(cleared["cleared"], true);
        assert!(!production_ready_from_seal(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn irreversible_adoption_requires_guardrails_and_refuses_valid_node_backup() {
        let home = std::env::temp_dir().join(format!(
            "herdr-seal-adopt-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&home);
        let config_dir = home.join(".config/herdr-mcp");
        fs::create_dir_all(&config_dir).unwrap();
        let status = aligned_rust_status();

        let missing_ack = adopt_existing_rust_with_status(
            &home,
            &config_dir,
            false,
            "legacy backup lost",
            &status,
        )
        .unwrap();
        assert_eq!(missing_ack["ok"], false);
        assert!(
            missing_ack["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "explicit_acknowledgement_required")
        );

        let adopted = adopt_existing_rust_with_status(
            &home,
            &config_dir,
            true,
            "legacy Node backup was independently confirmed unavailable",
            &status,
        )
        .unwrap();
        assert_eq!(adopted["ok"], true);
        assert_eq!(adopted["rollback_available"], false);
        assert!(irreversible_adoption_evidence_present(&config_dir));
        assert!(!rollback_uat_evidence_present(&config_dir));
        assert_eq!(
            read_irreversible_adoption_evidence(&config_dir).unwrap()["kind"],
            "irreversible-rust-adoption"
        );

        let home_with_backup = std::env::temp_dir().join(format!(
            "herdr-seal-adopt-backup-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&home_with_backup);
        let config_with_backup = home_with_backup.join(".config/herdr-mcp");
        fs::create_dir_all(&config_with_backup).unwrap();
        write_valid_node_backup(&home_with_backup);
        let refused = adopt_existing_rust_with_status(
            &home_with_backup,
            &config_with_backup,
            true,
            "should not adopt while rollback is available",
            &status,
        )
        .unwrap();
        assert_eq!(refused["ok"], false);
        assert!(
            refused["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "valid_node_rollback_backup_exists")
        );
        assert!(!irreversible_adoption_evidence_path(&config_with_backup).exists());

        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&home_with_backup);
    }
}
