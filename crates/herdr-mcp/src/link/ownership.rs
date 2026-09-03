//! Read-only Link ownership classification and production cutover gates.
//!
//! This module never mutates launchd, plists, `runtime/current`, or credentials.
//! It answers: who owns Link today, and which gates still block
//! `production_ready=true` for G5.

use crate::config::Config;
use crate::instance::InstanceId;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::process::ExitCode;

/// LaunchAgent labels used by workstation Link today.
pub const LINK_LABEL: &str = "dev.herdr-mcp.link";
pub const LINK_PROD_LABEL: &str = "dev.herdr-mcp.link-prod";

/// Named gates that must all pass before health may flip `production_ready`.
pub const PRODUCTION_READY_GATE_IDS: [&str; 8] = [
    "rust_cli_link_run",
    "launchd_prod_program_is_rust_runtime",
    "launchd_not_repo_checkout",
    "runtime_control_generation_rust_compatible",
    "health_runtime_not_candidate",
    "user_cli_not_repo_bash_bridge",
    "node_link_not_required",
    "dual_verification_uat",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkImplementation {
    Node,
    Rust,
    Unknown,
    Absent,
}

impl LinkImplementation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Rust => "rust",
            Self::Unknown => "unknown",
            Self::Absent => "absent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkAgentView {
    pub label: String,
    pub plist_path: PathBuf,
    pub present: bool,
    pub loaded: bool,
    pub implementation: LinkImplementation,
    pub program_arguments: Vec<String>,
    pub edge_url: Option<String>,
    pub workstation_id: Option<String>,
    pub control_path: Option<String>,
    pub status_path: Option<String>,
    pub runtime_generation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateStatus {
    pub id: String,
    pub ok: bool,
    pub detail: String,
}

/// Classify LaunchAgent ProgramArguments as Node, Rust, unknown, or absent.
pub fn classify_program_arguments(args: &[String]) -> LinkImplementation {
    if args.is_empty() {
        return LinkImplementation::Absent;
    }

    let first = args[0].as_str();
    let first_name = Path::new(first)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(first);

    let looks_like_node = first_name == "node" || first_name == "nodejs";
    let node_daemon = args.iter().any(|arg| {
        let lower = arg.to_ascii_lowercase();
        lower.contains("macos-daemon")
            || lower.ends_with("/dist/link/macos-daemon.js")
            || lower.ends_with("\\dist\\link\\macos-daemon.js")
            || (lower.contains("/link/") && lower.ends_with(".js"))
    });
    if looks_like_node && node_daemon {
        return LinkImplementation::Node;
    }
    if looks_like_node {
        return LinkImplementation::Node;
    }

    let looks_like_herdr = first_name == "herdr-mcp";
    let has_link_subcommand = args.iter().skip(1).any(|arg| arg == "link");
    let has_run = args.iter().skip(1).any(|arg| arg == "run");
    if looks_like_herdr && has_link_subcommand && has_run {
        return LinkImplementation::Rust;
    }
    if looks_like_herdr && has_link_subcommand {
        return LinkImplementation::Rust;
    }

    LinkImplementation::Unknown
}

/// True when ProgramArguments still point at a repository checkout `dist/`.
pub fn program_points_at_repo_checkout(args: &[String]) -> bool {
    args.iter().any(|arg| {
        let lower = arg.replace('\\', "/").to_ascii_lowercase();
        lower.contains("/documents/") && lower.contains("/dist/link/")
            || lower.contains("/.herdr/worktrees/") && lower.contains("/dist/link/")
            || lower.contains("/herdr-mcp/dist/link/")
    })
}

/// True when ProgramArguments use the managed active-runtime binary path shape.
pub fn program_points_at_managed_runtime(args: &[String], home: &Path) -> bool {
    let expected = home
        .join(".config")
        .join("herdr-mcp")
        .join("runtime")
        .join("current")
        .join("herdr-mcp");
    args.first()
        .is_some_and(|first| Path::new(first) == expected.as_path())
}

/// Runtime-control generation ids that are still Node-era product versions.
pub fn generation_looks_node_era(generation: &str) -> bool {
    let trimmed = generation.trim();
    if trimmed.is_empty() || trimmed == "-" {
        return false;
    }
    trimmed.starts_with("stable-0.3.")
        || trimmed.starts_with("candidate-0.3.")
        || trimmed == "0.3.32"
}

/// Runtime-control generation ids that are Rust-era / compatible.
pub fn generation_looks_rust_compatible(generation: &str) -> bool {
    let trimmed = generation.trim();
    if trimmed.is_empty() || trimmed == "-" {
        return false;
    }
    if generation_looks_node_era(trimmed) {
        return false;
    }
    trimmed.starts_with("rust-")
        || trimmed.starts_with("local-mcp")
        || trimmed.starts_with("0.4.")
        || trimmed.contains("alpha.")
}

pub fn assess_agent(home: &Path, label: &str, loaded: bool) -> LinkAgentView {
    let plist_path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist"));
    if !plist_path.is_file() {
        return LinkAgentView {
            label: label.to_owned(),
            plist_path,
            present: false,
            loaded,
            implementation: LinkImplementation::Absent,
            program_arguments: Vec::new(),
            edge_url: None,
            workstation_id: None,
            control_path: None,
            status_path: None,
            runtime_generation: None,
        };
    }

    let fields = read_link_plist(&plist_path);
    let implementation = classify_program_arguments(&fields.program_arguments);
    LinkAgentView {
        label: label.to_owned(),
        plist_path,
        present: true,
        loaded,
        implementation,
        program_arguments: fields.program_arguments,
        edge_url: fields.edge_url,
        workstation_id: fields.workstation_id,
        control_path: fields.control_path,
        status_path: fields.status_path,
        runtime_generation: fields.runtime_generation,
    }
}

struct LinkPlistFields {
    program_arguments: Vec<String>,
    edge_url: Option<String>,
    workstation_id: Option<String>,
    control_path: Option<String>,
    status_path: Option<String>,
    runtime_generation: Option<String>,
}

fn read_link_plist(path: &Path) -> LinkPlistFields {
    let empty = || LinkPlistFields {
        program_arguments: Vec::new(),
        edge_url: None,
        workstation_id: None,
        control_path: None,
        status_path: None,
        runtime_generation: None,
    };
    let Ok(value) = plist::Value::from_file(path) else {
        return empty();
    };
    let Some(dict) = value.as_dictionary() else {
        return empty();
    };
    let program_arguments = dict
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
    let env = dict
        .get("EnvironmentVariables")
        .and_then(plist::Value::as_dictionary);
    let read_env = |key: &str| -> Option<String> {
        env.and_then(|map| map.get(key))
            .and_then(plist::Value::as_string)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    LinkPlistFields {
        program_arguments,
        edge_url: read_env("HERDR_EDGE_URL"),
        workstation_id: read_env("HERDR_WORKSTATION_ID"),
        control_path: read_env("HERDR_RUNTIME_CONTROL_PATH"),
        status_path: read_env("HERDR_RUNTIME_STATUS_PATH"),
        runtime_generation: read_env("HERDR_RUNTIME_GENERATION"),
    }
}

pub fn read_control_desired_active(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("desired_active")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub fn read_status_active_generation(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .pointer("/manager/active_generation")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn current_managed_runtime_generation(home: &Path) -> Option<String> {
    let target = fs::read_link(
        home.join(".config")
            .join("herdr-mcp")
            .join("runtime")
            .join("current"),
    )
    .ok()?;
    let generation = target.file_name()?.to_str()?.trim();
    generation_looks_rust_compatible(generation).then(|| generation.to_owned())
}

/// Compute which G5 gates are currently open. Live cutover is never performed here.
pub fn evaluate_production_ready_gates(
    home: &Path,
    config_dir: &Path,
    prod: &LinkAgentView,
    link: &LinkAgentView,
    rust_cli_has_link_run: bool,
) -> Vec<GateStatus> {
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
    let current = current_managed_runtime_generation(home);

    let prod_is_rust = prod.present
        && prod.implementation == LinkImplementation::Rust
        && program_points_at_managed_runtime(&prod.program_arguments, home);
    let checkout_refused =
        !prod.present || !program_points_at_repo_checkout(&prod.program_arguments);
    let generation_ok = matches!(
        (current.as_deref(), desired.as_deref(), active.as_deref()),
        (Some(current), Some(desired), Some(active))
            if current == desired
                && desired == active
                && generation_looks_rust_compatible(current)
    );

    let user_cli = home.join(".local").join("bin").join("herdr-mcp");
    let user_cli_ok = user_cli_points_at_managed_runtime(&user_cli, home);

    let node_not_required =
        prod_is_rust && !program_points_at_repo_checkout(&prod.program_arguments);
    let sealed = crate::link::seal::production_ready_from_seal(config_dir);
    let dual_uat = crate::link::seal::dual_uat_evidence_present(config_dir);

    vec![
        GateStatus {
            id: "rust_cli_link_run".to_owned(),
            ok: rust_cli_has_link_run,
            detail: if rust_cli_has_link_run {
                "herdr-mcp link run is present in this binary".to_owned()
            } else {
                "herdr-mcp link run is not wired yet; status-only prerequisite".to_owned()
            },
        },
        GateStatus {
            id: "launchd_prod_program_is_rust_runtime".to_owned(),
            ok: prod_is_rust,
            detail: format!(
                "label={} implementation={} program0={}",
                prod.label,
                prod.implementation.as_str(),
                prod.program_arguments
                    .first()
                    .map(String::as_str)
                    .unwrap_or("-")
            ),
        },
        GateStatus {
            id: "launchd_not_repo_checkout".to_owned(),
            ok: checkout_refused,
            detail: if checkout_refused {
                "prod ProgramArguments do not point at repo dist/link".to_owned()
            } else {
                "prod ProgramArguments still point at a checkout dist/link path".to_owned()
            },
        },
        GateStatus {
            id: "runtime_control_generation_rust_compatible".to_owned(),
            ok: generation_ok,
            detail: format!(
                "current={} desired={} active={}",
                current.as_deref().unwrap_or("-"),
                desired.as_deref().unwrap_or("-"),
                active.as_deref().unwrap_or("-")
            ),
        },
        GateStatus {
            id: "health_runtime_not_candidate".to_owned(),
            ok: sealed,
            detail: if sealed {
                "active link-production-ready seal present".to_owned()
            } else {
                "health keeps runtime=rust-candidate / production_ready=false until link seal --execute"
                    .to_owned()
            },
        },
        GateStatus {
            id: "user_cli_not_repo_bash_bridge".to_owned(),
            ok: user_cli_ok,
            detail: format!("~/.local/bin/herdr-mcp -> {}", describe_symlink(&user_cli)),
        },
        GateStatus {
            id: "node_link_not_required".to_owned(),
            ok: node_not_required,
            detail: format!(
                "prod={} canary/dev={} checkout_refused={} (canary Node soak is allowed)",
                prod.implementation.as_str(),
                link.implementation.as_str(),
                checkout_refused
            ),
        },
        GateStatus {
            id: "dual_verification_uat".to_owned(),
            ok: dual_uat,
            detail: if dual_uat {
                "dual-uat evidence recorded under seals/evidence/dual-uat.json".to_owned()
            } else {
                "requires herdr-mcp link seal record --dual-uat after independent Shell dual verification"
                    .to_owned()
            },
        },
    ]
}

fn prefer_existing(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|path| path.is_file()).cloned()
}

fn user_cli_points_at_managed_runtime(cli: &Path, home: &Path) -> bool {
    let expected = home
        .join(".config")
        .join("herdr-mcp")
        .join("runtime")
        .join("current")
        .join("herdr-mcp");
    match fs::symlink_metadata(cli) {
        Ok(meta) if meta.file_type().is_symlink() => fs::read_link(cli)
            .ok()
            .is_some_and(|target| target == expected),
        Ok(_) => cli == expected.as_path(),
        Err(_) => false,
    }
}

fn describe_symlink(path: &Path) -> String {
    match fs::read_link(path) {
        Ok(target) => target.display().to_string(),
        Err(_) if path.exists() => path.display().to_string(),
        Err(_) => "missing".to_owned(),
    }
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

pub(crate) fn parse_launchd_environment_value(output: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} =>");
    output.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix(&prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn launchd_loaded_environment_value(label: &str, key: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let target = format!("gui/{}/{label}", unsafe { libc::geteuid() });
        let output = Command::new("launchctl")
            .args(["print", target.as_str()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        parse_launchd_environment_value(&String::from_utf8_lossy(&output.stdout), key)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (label, key);
        None
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Collect a JSON ownership + gate report for `herdr-mcp link status`.
pub fn collect_status_report(home: &Path, config_dir: &Path) -> Value {
    let prod_loaded = launchd_label_loaded(LINK_PROD_LABEL);
    let link_loaded = launchd_label_loaded(LINK_LABEL);
    let candidate_loaded = launchd_label_loaded(crate::link::LINK_RUST_CANDIDATE_LABEL);
    let prod = assess_agent(home, LINK_PROD_LABEL, prod_loaded);
    let link = assess_agent(home, LINK_LABEL, link_loaded);
    let candidate = assess_agent(
        home,
        crate::link::LINK_RUST_CANDIDATE_LABEL,
        candidate_loaded,
    );
    let configured_prod_generation = prod.runtime_generation.clone();
    let loaded_prod_generation =
        launchd_loaded_environment_value(LINK_PROD_LABEL, "HERDR_RUNTIME_GENERATION");
    let current_generation = current_managed_runtime_generation(home);
    let status_path = prefer_existing(&[
        config_dir.join("runtime-status-prod.json"),
        config_dir.join("runtime-status.json"),
    ]);
    let active_generation = status_path
        .as_ref()
        .and_then(|path| read_status_active_generation(path));
    let configured_matches_current = matches!(
        (configured_prod_generation.as_deref(), current_generation.as_deref()),
        (Some(configured), Some(current)) if configured == current
    );
    let loaded_matches_current = matches!(
        (loaded_prod_generation.as_deref(), current_generation.as_deref()),
        (Some(loaded), Some(current)) if loaded == current
    );
    let runtime_control_active_matches_current = matches!(
        (active_generation.as_deref(), current_generation.as_deref()),
        (Some(active), Some(current)) if active == current
    );
    let loaded_environment_stale = prod_loaded && !loaded_matches_current;
    // Foreground `link run` is wired. Candidate LaunchAgent install is separate
    // from production cutover; production_ready stays false until all gates pass.
    let rust_cli_has_link_run = crate::link::LINK_RUN_WIRED;
    let gates =
        evaluate_production_ready_gates(home, config_dir, &prod, &link, rust_cli_has_link_run);
    let all_ok = gates.iter().all(|gate| gate.ok);
    let production_owner = if prod.implementation == LinkImplementation::Rust && prod.loaded {
        "rust"
    } else if (prod.implementation == LinkImplementation::Node && (prod.loaded || prod.present))
        || (link.implementation == LinkImplementation::Node && link.loaded)
    {
        "node"
    } else if prod.present || link.present {
        "mixed-or-unknown"
    } else {
        "absent"
    };

    let config_path = home.join(".config").join("herdr-mcp").join("config.toml");
    let config = Config::load_for_instance(&config_path, &InstanceId::default_instance()).ok();
    let edge_public_origin = config.as_ref().and_then(|c| c.edge_public_origin.clone());
    let link_upstream_origin = config
        .as_ref()
        .and_then(|c| c.edge_link_upstream_origin.clone());
    let transport_evidence = crate::link::collect_transport_evidence(
        edge_public_origin.as_deref(),
        link_upstream_origin.as_deref(),
    );

    json!({
        "ok": true,
        "cutover_performed": false,
        "production_owner": production_owner,
        "production_ready_eligible": all_ok,
        "edge_public_origin": edge_public_origin,
        "link_upstream_origin": link_upstream_origin,
        "transport": {
            "mcp_origin": transport_evidence.mcp_origin,
            "link_upstream": transport_evidence.link_upstream,
            "link_transport": transport_evidence.link_transport,
            "proxy_source": transport_evidence.proxy_source,
            "relay": transport_evidence.relay,
            "pool_source": transport_evidence.pool_source,
            "failover_ready": transport_evidence.failover_ready,
        },
        "gates": gates.iter().map(|gate| json!({
            "id": gate.id,
            "ok": gate.ok,
            "detail": gate.detail,
        })).collect::<Vec<_>>(),
        "agents": [
            agent_json(&prod, home),
            agent_json(&link, home),
            agent_json(&candidate, home),
        ],
        "production_runtime_alignment": {
            "current_generation": current_generation,
            "active_generation": active_generation,
            "configured_launchd_generation": configured_prod_generation,
            "loaded_launchd_generation": loaded_prod_generation,
            "configured_matches_current": configured_matches_current,
            "loaded_matches_current": loaded_matches_current,
            "runtime_control_active_matches_current": runtime_control_active_matches_current,
            "loaded_environment_stale": loaded_environment_stale,
            "detail": if loaded_environment_stale && runtime_control_active_matches_current {
                "runtime-control reports active=current, but the loaded launchd environment still carries a stale startup generation"
            } else if loaded_environment_stale {
                "loaded launchd generation is stale relative to runtime/current and runtime-control has not reported active=current"
            } else {
                "loaded/configured Link generation is aligned with runtime/current"
            },
        },
        "notes": [
            "Read-only report. Does not mutate launchd, plists, or Node Link.",
            "Candidate label is dev.herdr-mcp.link-rust-candidate (link install/uninstall); never confuses with live Node link/link-prod.",
            "Live production cutover requires independent dual verification; see docs/link-production-cutover.md",
        ],
    })
}

fn agent_json(agent: &LinkAgentView, home: &Path) -> Value {
    json!({
        "label": agent.label,
        "plist": agent.plist_path.display().to_string(),
        "present": agent.present,
        "loaded": agent.loaded,
        "implementation": agent.implementation.as_str(),
        "program_arguments": agent.program_arguments,
        "edge_url": agent.edge_url,
        "workstation_id": agent.workstation_id,
        "control_path": agent.control_path,
        "status_path": agent.status_path,
        "runtime_generation": agent.runtime_generation,
        "points_at_repo_checkout": program_points_at_repo_checkout(&agent.program_arguments),
        "points_at_managed_runtime": program_points_at_managed_runtime(&agent.program_arguments, home),
    })
}

/// Compact one-line doctor LAYER summary.
pub fn doctor_layer_summary(home: &Path, config_dir: &Path) -> String {
    let report = collect_status_report(home, config_dir);
    let owner = report
        .get("production_owner")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let eligible = report
        .get("production_ready_eligible")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let agents = report
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut prod_impl = "absent";
    let mut prod_loaded = false;
    let mut link_impl = "absent";
    let mut link_loaded = false;
    for agent in &agents {
        let label = agent.get("label").and_then(Value::as_str).unwrap_or("");
        let impl_name = agent
            .get("implementation")
            .and_then(Value::as_str)
            .unwrap_or("absent");
        let loaded = agent
            .get("loaded")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if label == LINK_PROD_LABEL {
            prod_impl = impl_name;
            prod_loaded = loaded;
        } else if label == LINK_LABEL {
            link_impl = impl_name;
            link_loaded = loaded;
        }
    }
    let ownership = if owner == "rust" {
        "owned"
    } else if owner == "absent" {
        "absent"
    } else {
        "unowned"
    };
    format!(
        "{ownership} production_owner={owner} prod_impl={prod_impl} prod_loaded={prod_loaded} link_impl={link_impl} link_loaded={link_loaded} candidate_label={} production_ready_eligible={eligible} remote-probe=edge-layer",
        crate::link::install::LINK_RUST_CANDIDATE_LABEL
    )
}

/// Static gate catalog embedded in health/migration metadata.
///
/// `production_ready` follows the auditable seal file when present; LaunchAgent
/// ownership alone never flips it.
pub fn production_ready_gate_catalog() -> Value {
    let sealed = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| {
            crate::link::seal::production_ready_from_seal(&home.join(".config").join("herdr-mcp"))
        })
        .unwrap_or(false);
    json!({
        "production_ready": sealed,
        "requires_all": PRODUCTION_READY_GATE_IDS,
        "cutover_doc": "docs/link-production-cutover.md",
        "note": "Gates are evaluated by herdr-mcp link status; production_ready follows link seal --execute (cleared by cutover --rollback)",
    })
}

pub fn run_status() -> Result<ExitCode, String> {
    let home = home_dir().ok_or_else(|| "HOME is required for link status".to_owned())?;
    let config_dir = home.join(".config").join("herdr-mcp");
    let report = collect_status_report(&home, &config_dir);
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn classifies_node_macos_daemon() {
        let program = args(&[
            "/usr/local/bin/node",
            "/Users/qingxian/Documents/herdr-mcp/dist/link/macos-daemon.js",
        ]);
        assert_eq!(
            classify_program_arguments(&program),
            LinkImplementation::Node
        );
        assert!(program_points_at_repo_checkout(&program));
    }

    #[test]
    fn classifies_rust_link_run_on_managed_runtime() {
        let home = PathBuf::from("/Users/example");
        let binary = home
            .join(".config")
            .join("herdr-mcp")
            .join("runtime")
            .join("current")
            .join("herdr-mcp");
        let program = args(&[binary.to_str().unwrap(), "link", "run"]);
        assert_eq!(
            classify_program_arguments(&program),
            LinkImplementation::Rust
        );
        assert!(program_points_at_managed_runtime(&program, &home));
        assert!(!program_points_at_repo_checkout(&program));
    }

    #[test]
    fn agent_json_exposes_positive_managed_runtime_ownership() {
        let home = PathBuf::from("/Users/example");
        let binary = home
            .join(".config")
            .join("herdr-mcp")
            .join("runtime")
            .join("current")
            .join("herdr-mcp");
        let managed = LinkAgentView {
            label: LINK_PROD_LABEL.to_owned(),
            plist_path: home
                .join("Library")
                .join("LaunchAgents")
                .join("dev.herdr-mcp.link-prod.plist"),
            present: true,
            loaded: true,
            implementation: LinkImplementation::Rust,
            program_arguments: args(&[binary.to_str().unwrap(), "link", "run"]),
            edge_url: None,
            workstation_id: None,
            control_path: None,
            status_path: None,
            runtime_generation: Some("rust-7d7db9d2063970d2".to_owned()),
        };
        let json = agent_json(&managed, &home);
        assert_eq!(json["points_at_managed_runtime"], true);
        assert_eq!(json["points_at_repo_checkout"], false);
        assert_eq!(json["implementation"], "rust");

        // A foreign Rust binary (same implementation, not the managed path) must
        // NOT be treated as owned: points_at_managed_runtime is the positive proof.
        let foreign = LinkAgentView {
            program_arguments: args(&["/opt/other/herdr-mcp", "link", "run"]),
            ..managed.clone()
        };
        let foreign_json = agent_json(&foreign, &home);
        assert_eq!(foreign_json["points_at_managed_runtime"], false);
        assert_eq!(foreign_json["implementation"], "rust");
    }

    #[test]
    fn parses_loaded_launchd_runtime_generation_from_print_output() {
        let output = r#"
environment = {
    HERDR_RUNTIME_VERSION => 0.4.3-dev
    HERDR_RUNTIME_GENERATION => rust-c286e4312263b688
    HERDR_WORKSTATION_ID => prod-real-runtime
}
"#;
        assert_eq!(
            parse_launchd_environment_value(output, "HERDR_RUNTIME_GENERATION").as_deref(),
            Some("rust-c286e4312263b688")
        );
        assert_eq!(
            parse_launchd_environment_value(output, "HERDR_EDGE_URL"),
            None
        );
    }

    #[test]
    fn generation_helpers_separate_node_and_rust_eras() {
        assert!(generation_looks_node_era("stable-0.3.32"));
        assert!(generation_looks_node_era("candidate-0.3.32-6bd5f2"));
        assert!(!generation_looks_rust_compatible("stable-0.3.32"));
        assert!(generation_looks_rust_compatible("rust-7ef4a3f7b328c3d2"));
        assert!(generation_looks_rust_compatible("local-mcp-active"));
        assert!(generation_looks_rust_compatible("0.4.0-alpha.9"));
        let current = |desired: Option<&str>, active: Option<&str>| match (desired, active) {
            (Some(desired), Some(active)) => {
                desired == active
                    && generation_looks_rust_compatible(desired)
                    && generation_looks_rust_compatible(active)
            }
            (Some(desired), None) => generation_looks_rust_compatible(desired),
            (None, Some(active)) => generation_looks_rust_compatible(active),
            (None, None) => false,
        };
        assert!(current(Some("rust-current"), Some("rust-current")));
        assert!(!current(Some("rust-desired"), Some("rust-stale")));
    }

    #[test]
    fn evaluate_gates_fail_closed_for_node_prod() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let root = std::env::temp_dir().join(format!("herdr-link-own-{stamp}"));
        let home = root.join("home");
        let config_dir = home.join(".config").join("herdr-mcp");
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

        let prod = LinkAgentView {
            label: LINK_PROD_LABEL.to_owned(),
            plist_path: home
                .join("Library")
                .join("LaunchAgents")
                .join("dev.herdr-mcp.link-prod.plist"),
            present: true,
            loaded: true,
            implementation: LinkImplementation::Node,
            program_arguments: args(&[
                "/usr/local/bin/node",
                "/Users/qingxian/Documents/herdr-mcp/dist/link/macos-daemon.js",
            ]),
            edge_url: Some("wss://example/ws".to_owned()),
            workstation_id: Some("prod-real-runtime".to_owned()),
            control_path: Some(
                config_dir
                    .join("runtime-control-prod.json")
                    .display()
                    .to_string(),
            ),
            status_path: Some(
                config_dir
                    .join("runtime-status-prod.json")
                    .display()
                    .to_string(),
            ),
            runtime_generation: Some("stable-0.3.32".to_owned()),
        };
        let link = LinkAgentView {
            label: LINK_LABEL.to_owned(),
            plist_path: home
                .join("Library")
                .join("LaunchAgents")
                .join("dev.herdr-mcp.link.plist"),
            present: true,
            loaded: true,
            implementation: LinkImplementation::Node,
            program_arguments: args(&[
                "/usr/local/bin/node",
                "/Users/qingxian/Documents/herdr-mcp/dist/link/macos-daemon.js",
            ]),
            edge_url: None,
            workstation_id: None,
            control_path: None,
            status_path: None,
            runtime_generation: None,
        };

        let gates = evaluate_production_ready_gates(&home, &config_dir, &prod, &link, false);
        assert_eq!(gates.len(), PRODUCTION_READY_GATE_IDS.len());
        assert!(gates.iter().all(|gate| !gate.ok));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn gate_catalog_keeps_production_ready_false() {
        let _env_guard = crate::test_env::lock();
        let root = env::temp_dir().join(format!(
            "herdr-link-catalog-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", &root);
        }
        let catalog = production_ready_gate_catalog();
        assert_eq!(catalog["production_ready"], false);
        assert_eq!(
            catalog["requires_all"].as_array().map(Vec::len),
            Some(PRODUCTION_READY_GATE_IDS.len())
        );
        unsafe {
            match previous_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
        }
        let _ = fs::remove_dir_all(root);
    }
}
