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

use super::ownership::{
    LINK_LABEL, LINK_PROD_LABEL, assess_agent, evaluate_production_ready_gates,
};
use super::run::LINK_RUN_WIRED;

/// Env guard required before `link seal --execute`.
pub const SEAL_EXECUTE_ENV: &str = "HERDR_LINK_SEAL_I_UNDERSTAND";

const SEAL_SCHEMA_VERSION: u64 = 1;
const ACTIVE_SEAL_NAME: &str = "link-production-ready.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealMode {
    Status,
    RecordDualUat,
    RecordRollbackUat,
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
                        "Record dual-uat + rollback-uat evidence first.",
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

fn status_report(home: &Path, config_dir: &Path) -> Result<Value, String> {
    let plan = plan_seal(home, config_dir, false)?;
    Ok(json!({
        "ok": true,
        "action": "seal_status",
        "production_ready": production_ready_from_seal(config_dir),
        "active_seal_path": active_seal_path(config_dir).display().to_string(),
        "dual_uat_recorded": dual_uat_evidence_present(config_dir),
        "rollback_uat_recorded": rollback_uat_evidence_present(config_dir),
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
    if !rollback {
        blockers.push("rollback_uat_evidence_missing".to_owned());
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
        "gates": gates.iter().map(|g| json!({
            "id": g.id,
            "ok": g.ok,
            "detail": g.detail,
        })).collect::<Vec<_>>(),
        "notes": [
            "Seal never auto-flips from LaunchAgent ownership alone.",
            "Record dual-uat and rollback-uat evidence before --execute.",
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
    let rollback: Value = serde_json::from_slice(
        &fs::read(&rollback_path).map_err(|error| format!("read rollback-uat: {error}"))?,
    )
    .map_err(|error| error.to_string())?;

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
        "evidence": {
            "dual_uat": dual,
            "rollback_uat": rollback,
        },
        "notes": [
            "Auditable G5 seal. Cleared by link cutover --rollback.",
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
}
