//! Production Link cutover planner and guarded execute transaction.
//!
//! `herdr-mcp link cutover` (default `--dry-run`) reads Node prod + Rust
//! candidate LaunchAgents, validates AGENTS.md ownership rules for the
//! *planned* ProgramArguments, and prints the exact cutover + rollback steps.
//! Dry-run never mutates launchd, plists, `runtime/current`, or live Node
//! `link` / `link-prod`.
//!
//! `--execute` requires `HERDR_LINK_CUTOVER_I_UNDERSTAND=1` and runs a
//! PREPARE/ACTIVATE/VERIFY transaction that rewrites **only**
//! `dev.herdr-mcp.link-prod` to `runtime/current/herdr-mcp link run`, with
//! automatic ROLLBACK to the Node plist backup on failure. It never flips
//! `production_ready`, never touches `link` / `link-rust-candidate`, and never
//! schedules inferred launchd submission jobs.

use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::process::ExitCode;

use super::install::{
    LINK_RUST_CANDIDATE_LABEL, assert_safe_candidate_program, candidate_program_arguments,
    managed_runtime_binary, protected_live_link_labels, resolve_managed_runtime_binary,
};
use super::ownership::{
    LINK_LABEL, LINK_PROD_LABEL, LinkAgentView, LinkImplementation, assess_agent,
    evaluate_production_ready_gates, generation_looks_rust_compatible,
    program_points_at_managed_runtime, program_points_at_repo_checkout,
    read_control_desired_active, read_status_active_generation,
};
use super::run::LINK_RUN_WIRED;

/// Env guard required before `--execute` is even acknowledged (still no-ops).
pub const CUTOVER_EXECUTE_ENV: &str = "HERDR_LINK_CUTOVER_I_UNDERSTAND";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutoverMode {
    DryRun,
    Execute,
    /// Deliberate restore of Node link-prod from the preserved backup.
    Rollback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Precondition {
    pub id: String,
    pub ok: bool,
    pub detail: String,
}

/// CLI entry for `herdr-mcp link cutover`.
pub fn run(mode: CutoverMode) -> Result<ExitCode, String> {
    let home = home_dir().ok_or_else(|| "HOME is required for link cutover".to_owned())?;
    let config_dir = home.join(".config").join("herdr-mcp");

    match mode {
        CutoverMode::DryRun => {
            let report = plan_dry_run(&home, &config_dir);
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
            );
            if report
                .get("ready_for_execute")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(2))
            }
        }
        CutoverMode::Execute => run_execute(&home, &config_dir),
        CutoverMode::Rollback => run_rollback(&home, &config_dir),
    }
}

fn run_rollback(home: &Path, config_dir: &Path) -> Result<ExitCode, String> {
    let understood = env::var_os(CUTOVER_EXECUTE_ENV)
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    if !understood {
        let report = json!({
            "ok": false,
            "mode": "rollback",
            "error": format!(
                "link cutover --rollback is refused without {CUTOVER_EXECUTE_ENV}=1"
            ),
            "notes": [
                "No launchd/plist mutation occurred.",
                "Independent Shell only: HERDR_LINK_CUTOVER_I_UNDERSTAND=1 herdr-mcp link cutover --rollback",
            ],
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        );
        return Ok(ExitCode::from(2));
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (home, config_dir);
        Err("link cutover --rollback is macOS-only".to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        use super::cutover_execute::{RealLaunchd, rollback_cutover};
        let prod_plist = home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LINK_PROD_LABEL}.plist"));
        let backup_path = prod_plist_backup_path(home);
        // Prefer the authoritative pre-rust-cutover backup; fall back to the
        // timestamped Node backup if the primary is missing.
        let backup = if backup_path.is_file() {
            backup_path.clone()
        } else {
            let alt = config_dir
                .join("backups")
                .join("link-prod.plist.node-pre-cutover-20260827T230026");
            if alt.is_file() {
                alt
            } else {
                backup_path.clone()
            }
        };
        let rollback = rollback_cutover(&RealLaunchd, &prod_plist, &backup);
        let seal_clear = super::seal::clear_active_seal(config_dir)?;
        let report = json!({
            "ok": rollback.get("ok").and_then(Value::as_bool).unwrap_or(false),
            "mode": "rollback",
            "backup_plist": backup.display().to_string(),
            "rollback": rollback,
            "seal_cleared": seal_clear,
            "production_ready": false,
            "protected_labels_untouched": [LINK_LABEL, LINK_RUST_CANDIDATE_LABEL],
            "notes": [
                "Restored Node link-prod from backup via bootout/bootstrap (never the forbidden launchd submission path).",
                "Active production_ready seal cleared if present.",
                "Re-cut to Rust with HERDR_LINK_CUTOVER_I_UNDERSTAND=1 link cutover --execute after Node verify.",
            ],
        });
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

fn run_execute(home: &Path, config_dir: &Path) -> Result<ExitCode, String> {
    let understood = env::var_os(CUTOVER_EXECUTE_ENV)
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    if !understood {
        let report = json!({
            "ok": false,
            "mode": "execute",
            "cutover_performed": false,
            "execute_implemented": true,
            "error": format!(
                "link cutover --execute is refused without {CUTOVER_EXECUTE_ENV}=1"
            ),
            "protected_labels_untouched": protected_live_link_labels(),
            "notes": [
                "No launchd/plist mutation occurred.",
                "Use: herdr-mcp link cutover --dry-run",
                "Then, from an independent Shell only: HERDR_LINK_CUTOVER_I_UNDERSTAND=1 herdr-mcp link cutover --execute",
            ],
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        );
        return Ok(ExitCode::from(3));
    }

    let dry = plan_dry_run(home, config_dir);
    let preconditions = dry
        .get("preconditions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let technical: Vec<Precondition> = preconditions
        .iter()
        .filter_map(|item| {
            Some(Precondition {
                id: item.get("id")?.as_str()?.to_owned(),
                ok: item.get("ok")?.as_bool()?,
                detail: item
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            })
        })
        .collect();
    if !super::cutover_execute::technical_preconditions_ready(&technical) {
        let report = json!({
            "ok": false,
            "mode": "execute",
            "cutover_performed": false,
            "execute_implemented": true,
            "error": "technical cutover preconditions are not satisfied",
            "dry_run": dry,
            "protected_labels_untouched": protected_live_link_labels(),
            "notes": [
                "Env guard accepted, but execute refused before any launchd mutation.",
                "Fix technical preconditions from dry-run, then retry from an independent Shell.",
                "dual_verification_uat_recorded is a seal gate and does not block execute.",
            ],
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        );
        return Ok(ExitCode::from(2));
    }

    let prod_loaded = launchd_label_loaded(LINK_PROD_LABEL);
    let prod = assess_agent(home, LINK_PROD_LABEL, prod_loaded);
    let report = super::cutover_execute::execute_transaction(
        home,
        &prod,
        &super::cutover_execute::RealLaunchd,
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    if report.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(4))
    }
}

/// Build the dry-run plan + precondition report (no mutations).
pub fn plan_dry_run(home: &Path, config_dir: &Path) -> Value {
    let prod_loaded = launchd_label_loaded(LINK_PROD_LABEL);
    let link_loaded = launchd_label_loaded(LINK_LABEL);
    let candidate_loaded = launchd_label_loaded(LINK_RUST_CANDIDATE_LABEL);
    let prod = assess_agent(home, LINK_PROD_LABEL, prod_loaded);
    let link = assess_agent(home, LINK_LABEL, link_loaded);
    let candidate = assess_agent(home, LINK_RUST_CANDIDATE_LABEL, candidate_loaded);
    plan_dry_run_with_agents(home, config_dir, prod, link, candidate)
}

/// Testable planner entry that accepts already-assessed agent views.
pub fn plan_dry_run_with_agents(
    home: &Path,
    config_dir: &Path,
    prod: LinkAgentView,
    link: LinkAgentView,
    candidate: LinkAgentView,
) -> Value {
    let (planned_program, planned_program_error) = match candidate_program_arguments(home) {
        Ok(args) => (Some(args), None),
        Err(error) => (None, Some(error)),
    };

    let ownership = validate_planned_ownership(home, planned_program.as_deref());
    let preconditions = evaluate_cutover_preconditions(
        home,
        config_dir,
        &prod,
        &link,
        &candidate,
        planned_program.as_deref(),
        planned_program_error.as_deref(),
        &ownership,
    );
    let ready = super::cutover_execute::technical_preconditions_ready(&preconditions);
    let gates = evaluate_production_ready_gates(home, config_dir, &prod, &link, LINK_RUN_WIRED);

    let planned_steps = build_planned_cutover_steps(home, &prod, planned_program.as_deref());
    let rollback_steps = build_planned_rollback_steps(home, &prod);
    let seal_blockers = preconditions
        .iter()
        .filter(|item| item.id == "dual_verification_uat_recorded" && !item.ok)
        .map(|item| {
            json!({
                "id": item.id,
                "ok": item.ok,
                "detail": item.detail,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "ok": true,
        "mode": "dry-run",
        "cutover_performed": false,
        "ready_for_execute": ready,
        "execute_implemented": true,
        "preconditions": preconditions.iter().map(|item| json!({
            "id": item.id,
            "ok": item.ok,
            "detail": item.detail,
        })).collect::<Vec<_>>(),
        "seal_blockers": seal_blockers,
        "ownership_validation": ownership,
        "planned_program_arguments": planned_program,
        "planned_program_error": planned_program_error,
        "current": {
            "production_owner": production_owner_label(&prod, &link),
            "runtime_current": describe_runtime_current(home),
            "agents": {
                "prod": agent_summary(home, &prod),
                "link": agent_summary(home, &link),
                "candidate": agent_summary(home, &candidate),
            },
        },
        "gates_informational": gates.iter().map(|gate| json!({
            "id": gate.id,
            "ok": gate.ok,
            "detail": gate.detail,
        })).collect::<Vec<_>>(),
        "planned_cutover_steps": planned_steps,
        "planned_rollback_steps": rollback_steps,
        "protected_labels_never_mutated_by_dry_run": protected_live_link_labels(),
        "notes": [
            "Dry-run only. No launchd bootout/bootstrap and no plist writes occurred.",
            "ready_for_execute ignores dual_verification_uat_recorded (seal gate); execute still requires HERDR_LINK_CUTOVER_I_UNDERSTAND=1.",
            "Do not confuse candidate soak (dev.herdr-mcp.link-rust-candidate) with production cutover.",
            "Independent Shell dual verification remains mandatory before production_ready seal; see docs/history/ga/g5-link-production-cutover.md",
            "Dry-run helper landed does not equal G5 cutover complete or production_ready=true.",
        ],
    })
}

fn validate_planned_ownership(home: &Path, planned: Option<&[String]>) -> Value {
    let Some(args) = planned else {
        return json!({
            "ok": false,
            "detail": "planned ProgramArguments unavailable",
            "must_be_runtime_current": true,
            "must_not_be_checkout_or_target": true,
        });
    };

    let safe = assert_safe_candidate_program(home, args);
    let managed = program_points_at_managed_runtime(args, home);
    let checkout = program_points_at_repo_checkout(args);
    let ok = safe.is_ok() && managed && !checkout;
    json!({
        "ok": ok,
        "assert_safe_candidate_program": safe.as_ref().err().cloned(),
        "points_at_managed_runtime": managed,
        "points_at_repo_checkout": checkout,
        "program_arguments": args,
        "expected_binary": managed_runtime_binary(home).display().to_string(),
        "must_be_runtime_current": true,
        "must_not_be_checkout_or_target": true,
    })
}

#[allow(clippy::too_many_arguments)]
fn evaluate_cutover_preconditions(
    home: &Path,
    config_dir: &Path,
    prod: &LinkAgentView,
    link: &LinkAgentView,
    candidate: &LinkAgentView,
    planned: Option<&[String]>,
    planned_error: Option<&str>,
    ownership: &Value,
) -> Vec<Precondition> {
    let mut out = Vec::new();

    match resolve_managed_runtime_binary(home) {
        Ok(path) => out.push(Precondition {
            id: "runtime_current_managed".to_owned(),
            ok: true,
            detail: format!("managed binary {}", path.display()),
        }),
        Err(error) => out.push(Precondition {
            id: "runtime_current_managed".to_owned(),
            ok: false,
            detail: error,
        }),
    }

    out.push(Precondition {
        id: "rust_cli_link_run".to_owned(),
        ok: LINK_RUN_WIRED,
        detail: if LINK_RUN_WIRED {
            "herdr-mcp link run is wired in this binary".to_owned()
        } else {
            "herdr-mcp link run is not wired".to_owned()
        },
    });

    let user_cli = home.join(".local").join("bin").join("herdr-mcp");
    let expected = managed_runtime_binary(home);
    let user_cli_ok = match fs::symlink_metadata(&user_cli) {
        Ok(meta) if meta.file_type().is_symlink() => fs::read_link(&user_cli)
            .ok()
            .is_some_and(|target| target == expected),
        Ok(_) => user_cli == expected,
        Err(_) => false,
    };
    out.push(Precondition {
        id: "user_cli_managed_runtime".to_owned(),
        ok: user_cli_ok,
        detail: format!(
            "{} -> {}",
            user_cli.display(),
            fs::read_link(&user_cli)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| {
                    if user_cli.exists() {
                        user_cli.display().to_string()
                    } else {
                        "missing".to_owned()
                    }
                })
        ),
    });

    let candidate_program_ok = candidate.present
        && candidate.implementation == LinkImplementation::Rust
        && program_points_at_managed_runtime(&candidate.program_arguments, home)
        && assert_safe_candidate_program(home, &candidate.program_arguments).is_ok();
    let candidate_ok = candidate_program_ok && candidate.loaded;
    out.push(Precondition {
        id: "candidate_healthy".to_owned(),
        ok: candidate_ok,
        detail: format!(
            "label={} present={} loaded={} implementation={} program={:?}",
            candidate.label,
            candidate.present,
            candidate.loaded,
            candidate.implementation.as_str(),
            candidate.program_arguments
        ),
    });

    out.push(Precondition {
        id: "prod_plist_present_for_backup".to_owned(),
        ok: prod.present,
        detail: format!(
            "label={} present={} implementation={} plist={}",
            prod.label,
            prod.present,
            prod.implementation.as_str(),
            prod.plist_path.display()
        ),
    });

    let ownership_ok = ownership
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    out.push(Precondition {
        id: "planned_program_ownership".to_owned(),
        ok: ownership_ok && planned.is_some(),
        detail: planned_error.map(str::to_owned).unwrap_or_else(|| {
            if ownership_ok {
                "planned ProgramArguments are runtime/current link run".to_owned()
            } else {
                "planned ProgramArguments failed ownership validation".to_owned()
            }
        }),
    });

    let control_path = prefer_existing(&[
        config_dir.join("runtime-control-prod.json"),
        config_dir.join("runtime-control.json"),
    ]);
    let status_path = prefer_existing(&[
        config_dir.join("runtime-status-prod.json"),
        config_dir.join("runtime-status.json"),
    ]);
    let desired = control_path
        .as_ref()
        .and_then(|path| read_control_desired_active(path));
    let active = status_path
        .as_ref()
        .and_then(|path| read_status_active_generation(path))
        .or_else(|| prod.runtime_generation.clone());
    let generation_ok = active
        .as_deref()
        .or(desired.as_deref())
        .is_some_and(generation_looks_rust_compatible);
    out.push(Precondition {
        id: "runtime_control_generation_rust_compatible".to_owned(),
        ok: generation_ok,
        detail: format!(
            "desired={} active={}",
            desired.as_deref().unwrap_or("-"),
            active.as_deref().unwrap_or("-")
        ),
    });

    // Post-cutover / seal gates stay informational for ready_for_execute.
    // Dual UAT is required before production_ready seal, not before LaunchAgent cut.
    out.push(Precondition {
        id: "dual_verification_uat_recorded".to_owned(),
        ok: false,
        detail: "dual verification UAT is never auto-flipped; operator must record after execute (seal gate, not execute blocker)"
            .to_owned(),
    });

    let _ = link; // reserved for future canary-policy checks
    out
}

fn build_planned_cutover_steps(
    home: &Path,
    prod: &LinkAgentView,
    planned: Option<&[String]>,
) -> Vec<String> {
    let binary = managed_runtime_binary(home);
    let backup = prod_plist_backup_path(home);
    let argv = planned
        .map(|args| args.join(" "))
        .unwrap_or_else(|| format!("{} link run", binary.display()));
    vec![
        "0. Independent Shell only (never managed herdr_exec); refuse inferred launchd submission jobs.".to_owned(),
        format!(
            "1. Capture read-only preflight: link status, service status, launchctl list for link labels, PlistBuddy Print ProgramArguments on {}.",
            prod.plist_path.display()
        ),
        format!(
            "2. Backup Node prod plist bytes to {} (0600); never overwrite without backup.",
            backup.display()
        ),
        format!(
            "3. Write replacement prod plist for {} with ProgramArguments [{}] (runtime/current only; never checkout/target).",
            LINK_PROD_LABEL, argv
        ),
        format!(
            "4. launchctl bootout gui/$UID/{} then wait until absent (bounded).",
            LINK_PROD_LABEL
        ),
        format!(
            "5. launchctl bootstrap gui/$UID {} (bounded retry; never inferred submission).",
            prod.plist_path.display()
        ),
        "6. Verify prod ProgramArguments are runtime/current link run; candidate may remain for soak or be uninstalled separately.".to_owned(),
        "7. Preserve Keychain link secret + server plist MCP token; never print credentials.".to_owned(),
        "8. Independent dual verification UAT (Edge → Link → Rust runtime → Herdr); only then seal production_ready.".to_owned(),
        format!(
            "9. Leave {} untouched unless a later explicit canary cut is approved.",
            LINK_LABEL
        ),
    ]
}

fn build_planned_rollback_steps(home: &Path, prod: &LinkAgentView) -> Vec<String> {
    let backup = prod_plist_backup_path(home);
    vec![
        "0. Independent Shell only; do not rebuild a binary as rollback.".to_owned(),
        format!(
            "1. launchctl bootout gui/$UID/{} and wait until absent.",
            LINK_PROD_LABEL
        ),
        format!(
            "2. Restore Node prod plist from backup {} → {}.",
            backup.display(),
            prod.plist_path.display()
        ),
        format!(
            "3. launchctl bootstrap gui/$UID {} (bounded retry; never inferred submission).",
            prod.plist_path.display()
        ),
        "4. Verify Node ProgramArguments restored; Edge online; do not touch runtime/current generations.".to_owned(),
        format!(
            "5. Leave {} and {} alone unless they were explicitly changed.",
            LINK_LABEL, LINK_RUST_CANDIDATE_LABEL
        ),
    ]
}

pub fn prod_plist_backup_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("herdr-mcp")
        .join("backups")
        .join("link-prod.plist.pre-rust-cutover")
}

fn prefer_existing(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|path| path.is_file()).cloned()
}

fn production_owner_label(prod: &LinkAgentView, link: &LinkAgentView) -> &'static str {
    if prod.implementation == LinkImplementation::Rust && prod.loaded {
        "rust"
    } else if (prod.implementation == LinkImplementation::Node && (prod.loaded || prod.present))
        || (link.implementation == LinkImplementation::Node && link.loaded)
    {
        "node"
    } else if prod.present || link.present {
        "mixed-or-unknown"
    } else {
        "absent"
    }
}

fn describe_runtime_current(home: &Path) -> Value {
    let current = home
        .join(".config")
        .join("herdr-mcp")
        .join("runtime")
        .join("current");
    match fs::read_link(&current) {
        Ok(target) => json!({
            "path": current.display().to_string(),
            "symlink": true,
            "target": target.display().to_string(),
            "resolve_ok": resolve_managed_runtime_binary(home).is_ok(),
        }),
        Err(error) => json!({
            "path": current.display().to_string(),
            "symlink": false,
            "error": error.to_string(),
            "resolve_ok": false,
        }),
    }
}

fn agent_summary(home: &Path, agent: &LinkAgentView) -> Value {
    json!({
        "label": agent.label,
        "plist": agent.plist_path.display().to_string(),
        "present": agent.present,
        "loaded": agent.loaded,
        "implementation": agent.implementation.as_str(),
        "program_arguments": agent.program_arguments,
        "points_at_repo_checkout": program_points_at_repo_checkout(&agent.program_arguments),
        "points_at_managed_runtime": program_points_at_managed_runtime(
            &agent.program_arguments,
            home
        ),
        "edge_url": agent.edge_url,
        "workstation_id": agent.workstation_id,
        "runtime_generation": agent.runtime_generation,
    })
}

fn launchd_label_loaded(label: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("launchctl").arg("list").output();
        let Ok(output) = output else {
            return false;
        };
        if !output.status.success() {
            return false;
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.split_whitespace().nth(2) == Some(label))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = label;
        false
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_home() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-link-cutover-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn setup_managed_runtime(home: &Path) {
        let runtime = home.join(".config/herdr-mcp/runtime");
        let generation = runtime.join("generations/rust-testhashcutover01");
        fs::create_dir_all(&generation).unwrap();
        let binary = generation.join("herdr-mcp");
        fs::write(&binary, b"#!/bin/sh\necho fake\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        }
        symlink(
            "generations/rust-testhashcutover01",
            runtime.join("current"),
        )
        .unwrap();
    }

    fn write_plist(path: &Path, label: &str, program: &[&str]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let args = program
            .iter()
            .map(|value| format!("    <string>{value}</string>"))
            .collect::<Vec<_>>()
            .join("\n");
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{args}
  </array>
</dict>
</plist>
"#
        );
        fs::write(path, xml).unwrap();
    }

    #[test]
    fn dry_run_fails_closed_on_node_prod_without_candidate() {
        let home = test_home();
        setup_managed_runtime(&home);
        let config_dir = home.join(".config/herdr-mcp");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("runtime-control-prod.json"),
            r#"{"schema_version":1,"desired_active":"stable-0.3.32"}"#,
        )
        .unwrap();
        fs::write(
            config_dir.join("runtime-status-prod.json"),
            r#"{"schema_version":1,"manager":{"active_generation":"stable-0.3.32"}}"#,
        )
        .unwrap();

        let agents = home.join("Library/LaunchAgents");
        write_plist(
            &agents.join("dev.herdr-mcp.link-prod.plist"),
            LINK_PROD_LABEL,
            &[
                "/usr/local/bin/node",
                "/Users/qingxian/Documents/herdr-mcp/dist/link/macos-daemon.js",
            ],
        );

        let prod = assess_agent(&home, LINK_PROD_LABEL, true);
        let link = assess_agent(&home, LINK_LABEL, false);
        let candidate = assess_agent(&home, LINK_RUST_CANDIDATE_LABEL, false);
        let report = plan_dry_run_with_agents(&home, &config_dir, prod, link, candidate);
        assert_eq!(report["mode"], "dry-run");
        assert_eq!(report["cutover_performed"], false);
        assert_eq!(report["ready_for_execute"], false);
        assert_eq!(report["execute_implemented"], true);
        let steps = report["planned_cutover_steps"].as_array().unwrap();
        assert!(steps.len() >= 5);
        assert!(
            steps
                .iter()
                .any(|step| step.as_str().unwrap().contains("runtime/current"))
        );
        let rollback = report["planned_rollback_steps"].as_array().unwrap();
        assert!(
            rollback
                .iter()
                .any(|step| step.as_str().unwrap().contains("Restore Node prod plist"))
        );
        assert!(
            report["ownership_validation"]["ok"]
                .as_bool()
                .unwrap_or(false),
            "planned argv should still validate even when current prod is Node"
        );
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn planned_ownership_rejects_checkout_and_target_shapes() {
        let home = test_home();
        setup_managed_runtime(&home);
        let bad_checkout = vec![
            "/Users/qingxian/Documents/herdr-mcp/target/release/herdr-mcp".to_owned(),
            "link".to_owned(),
            "run".to_owned(),
        ];
        let ownership = validate_planned_ownership(&home, Some(&bad_checkout));
        assert_eq!(ownership["ok"], false);

        let good = candidate_program_arguments(&home).unwrap();
        let ownership = validate_planned_ownership(&home, Some(&good));
        assert_eq!(ownership["ok"], true);
        assert_eq!(ownership["points_at_managed_runtime"], true);
        assert_eq!(ownership["points_at_repo_checkout"], false);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn preconditions_require_candidate_and_rust_generation() {
        let home = test_home();
        setup_managed_runtime(&home);
        let config_dir = home.join(".config/herdr-mcp");
        fs::create_dir_all(home.join(".local/bin")).unwrap();
        symlink(
            managed_runtime_binary(&home),
            home.join(".local/bin/herdr-mcp"),
        )
        .unwrap();
        fs::write(
            config_dir.join("runtime-control-prod.json"),
            r#"{"schema_version":1,"desired_active":"rust-testhashcutover01"}"#,
        )
        .unwrap();
        fs::write(
            config_dir.join("runtime-status-prod.json"),
            r#"{"schema_version":1,"manager":{"active_generation":"rust-testhashcutover01"}}"#,
        )
        .unwrap();

        let binary = managed_runtime_binary(&home);
        let agents = home.join("Library/LaunchAgents");
        write_plist(
            &agents.join("dev.herdr-mcp.link-prod.plist"),
            LINK_PROD_LABEL,
            &[
                "/usr/local/bin/node",
                "/Users/qingxian/Documents/herdr-mcp/dist/link/macos-daemon.js",
            ],
        );
        write_plist(
            &agents.join("dev.herdr-mcp.link-rust-candidate.plist"),
            LINK_RUST_CANDIDATE_LABEL,
            &[binary.to_str().unwrap(), "link", "run"],
        );

        let prod = assess_agent(&home, LINK_PROD_LABEL, true);
        let link = assess_agent(&home, LINK_LABEL, false);
        let candidate = assess_agent(&home, LINK_RUST_CANDIDATE_LABEL, true);
        let report = plan_dry_run_with_agents(&home, &config_dir, prod, link, candidate);
        let preconditions = report["preconditions"].as_array().unwrap();
        let by_id = |id: &str| {
            preconditions
                .iter()
                .find(|item| item["id"] == id)
                .cloned()
                .unwrap()
        };
        assert_eq!(by_id("runtime_current_managed")["ok"], true);
        assert_eq!(by_id("rust_cli_link_run")["ok"], true);
        assert_eq!(by_id("user_cli_managed_runtime")["ok"], true);
        assert_eq!(by_id("planned_program_ownership")["ok"], true);
        assert_eq!(by_id("prod_plist_present_for_backup")["ok"], true);
        assert_eq!(
            by_id("runtime_control_generation_rust_compatible")["ok"],
            true
        );
        assert_eq!(by_id("candidate_healthy")["ok"], true);
        // Dual UAT remains a seal blocker but no longer blocks ready_for_execute.
        assert_eq!(by_id("dual_verification_uat_recorded")["ok"], false);
        assert_eq!(report["ready_for_execute"], true);
        assert_eq!(report["execute_implemented"], true);

        let unloaded = assess_agent(&home, LINK_RUST_CANDIDATE_LABEL, false);
        let report_unloaded = plan_dry_run_with_agents(
            &home,
            &config_dir,
            assess_agent(&home, LINK_PROD_LABEL, true),
            assess_agent(&home, LINK_LABEL, false),
            unloaded,
        );
        let candidate_gate = report_unloaded["preconditions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == "candidate_healthy")
            .unwrap();
        assert_eq!(candidate_gate["ok"], false);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn execute_requires_env_guard_and_dry_run_stays_non_mutating() {
        let _env_guard = crate::test_env::lock();
        let home = test_home();
        setup_managed_runtime(&home);
        let config_dir = home.join(".config/herdr-mcp");
        fs::create_dir_all(&config_dir).unwrap();

        // SAFETY: test process isolates env for this guard check.
        let previous = env::var_os(CUTOVER_EXECUTE_ENV);
        unsafe {
            env::remove_var(CUTOVER_EXECUTE_ENV);
        }
        let understood = env::var_os(CUTOVER_EXECUTE_ENV)
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
        assert!(!understood);

        unsafe {
            env::set_var(CUTOVER_EXECUTE_ENV, "1");
        }
        let dry = plan_dry_run(&home, &config_dir);
        assert_eq!(dry["cutover_performed"], false);
        assert_eq!(dry["mode"], "dry-run");
        assert_eq!(dry["execute_implemented"], true);

        match previous {
            Some(value) => unsafe { env::set_var(CUTOVER_EXECUTE_ENV, value) },
            None => unsafe { env::remove_var(CUTOVER_EXECUTE_ENV) },
        }
        let _ = fs::remove_dir_all(&home);
    }
}
