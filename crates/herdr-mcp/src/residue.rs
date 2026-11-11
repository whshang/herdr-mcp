//! Read-only residue classification for macOS LaunchAgents and instance configs.
//!
//! Classifies LaunchAgent plists and instance configuration directories into:
//! - Default production cohort:
//!   * `loaded_default`: owned default plist present AND confirmed loaded in launchd.
//!   * `configured_not_loaded_default`: owned default plist present on disk AND confirmed not loaded in launchd.
//!   * `unknown_load_default`: owned default plist present on disk but load status could NOT be established.
//! - Legitimate named instances: valid instance name + matching config root present on disk.
//!   * `loaded_named`: legitimate named plist AND confirmed loaded in launchd.
//!   * `configured_not_loaded_named`: legitimate named plist AND confirmed not loaded in launchd.
//!   * `unknown_load_named`: legitimate named plist but load status could NOT be established.
//! - Stale/orphan named instances (missing config dir, broken pointer, or orphan config dir without plist).
//! - Non-loaded backup/temporary artifacts left under LaunchAgents.
//! - Independent Herdr LaunchAgents (`dev.herdr.*`).

use crate::instance::InstanceId;
#[cfg(any(target_os = "macos", test))]
use crate::instance::{
    DEFAULT_HEALTH_WATCHDOG_LABEL, DEFAULT_HERDR_SUPERVISOR_LABEL, DEFAULT_SERVICE_LABEL,
    DEFAULT_WATCHDOG_LABEL,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(any(target_os = "macos", target_os = "linux", test))]
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "macos", test))]
pub const DEFAULT_LABELS: &[&str] = &[
    DEFAULT_SERVICE_LABEL,
    "dev.herdr-mcp.link",
    "dev.herdr-mcp.link-prod",
    "dev.herdr-mcp.link-rust-candidate",
    "dev.herdr-mcp.auto-update",
    DEFAULT_HERDR_SUPERVISOR_LABEL,
    DEFAULT_WATCHDOG_LABEL,
    DEFAULT_HEALTH_WATCHDOG_LABEL,
];

/// Tri-state launchd load observation.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchdLoadState {
    Loaded,
    NotLoaded,
    Unknown,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResidueReport {
    /// Owned default plists present AND confirmed loaded in launchd.
    pub loaded_default: Vec<PathBuf>,
    /// Owned default plists present on disk AND confirmed not loaded in launchd.
    pub configured_not_loaded_default: Vec<PathBuf>,
    /// Owned default plists present on disk whose launchd load status is unknown / failed observation.
    pub unknown_load_default: Vec<(PathBuf, String)>,

    /// Legitimate named instances present on disk AND confirmed loaded in launchd: (instance_name, plist_path).
    pub loaded_named: Vec<(String, PathBuf)>,
    /// Legitimate named instances configured on disk AND confirmed not loaded in launchd: (instance_name, plist_path).
    pub configured_not_loaded_named: Vec<(String, PathBuf)>,
    /// Legitimate named instances whose launchd load status is unknown: (instance_name, plist_path, reason).
    pub unknown_load_named: Vec<(String, PathBuf, String)>,

    /// Stale or orphan named instances: (instance_name, path, reason).
    pub stale_named: Vec<(String, PathBuf, String)>,
    /// Non-loaded backup/temporary artifacts in LaunchAgents: (path, reason).
    pub backup_artifacts: Vec<(PathBuf, String)>,
    /// Independent Herdr LaunchAgents (dev.herdr.*): (path, load_state).
    pub independent_herdr: Vec<(PathBuf, LaunchdLoadState)>,
}

#[cfg(any(target_os = "macos", test))]
impl ResidueReport {
    pub fn is_clean(&self) -> bool {
        self.stale_named.is_empty()
            && self.backup_artifacts.is_empty()
            && self.unknown_load_default.is_empty()
            && self.unknown_load_named.is_empty()
    }
}

#[cfg(target_os = "macos")]
pub fn scan_residue(home: &Path) -> ResidueReport {
    let launch_agents = home.join("Library/LaunchAgents");
    let config_parent = home.join(".config");
    #[cfg(target_os = "macos")]
    {
        scan_residue_with(&launch_agents, &config_parent, |label| {
            launchd_load_state(label)
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        scan_residue_with(&launch_agents, &config_parent, |_| {
            (
                LaunchdLoadState::Unknown,
                "not supported on this platform".to_owned(),
            )
        })
    }
}

#[cfg(target_os = "macos")]
fn launchd_load_state(label: &str) -> (LaunchdLoadState, String) {
    let target = format!("gui/{}/{}", unsafe { libc::geteuid() }, label);
    let output = std::process::Command::new("/bin/launchctl")
        .args(["print", &target])
        .output();
    match output {
        Ok(out) => {
            if out.status.success() {
                (LaunchdLoadState::Loaded, "loaded".to_owned())
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let lower = stderr.to_ascii_lowercase();
                if lower.contains("could not find service")
                    || lower.contains("could not find specified service")
                    || lower.contains("service not found")
                    || lower.contains("no such process")
                {
                    (LaunchdLoadState::NotLoaded, "not loaded".to_owned())
                } else {
                    let reason = if stderr.trim().is_empty() {
                        format!("exit status {}", out.status)
                    } else {
                        stderr.trim().to_owned()
                    };
                    (LaunchdLoadState::Unknown, reason)
                }
            }
        }
        Err(err) => (
            LaunchdLoadState::Unknown,
            format!("cannot spawn launchctl: {err}"),
        ),
    }
}

#[cfg(any(target_os = "macos", test))]
pub fn scan_residue_with<Loaded: Fn(&str) -> (LaunchdLoadState, String)>(
    launch_agents_dir: &Path,
    config_parent: &Path,
    load_state_fn: Loaded,
) -> ResidueReport {
    let mut report = ResidueReport::default();

    if let Ok(entries) = fs::read_dir(launch_agents_dir) {
        let mut sorted_entries = entries.flatten().map(|e| e.path()).collect::<Vec<_>>();
        sorted_entries.sort();

        for path in sorted_entries {
            let Some(file_name_os) = path.file_name() else {
                continue;
            };
            let name = file_name_os.to_string_lossy().into_owned();

            // Check independent Herdr
            if (name.starts_with("dev.herdr.") || name == "dev.herdr.plist")
                && !name.starts_with("dev.herdr-mcp.")
            {
                let label = name.strip_suffix(".plist").unwrap_or(&name);
                let (state, _) = load_state_fn(label);
                report.independent_herdr.push((path, state));
                continue;
            }

            // Check if it's related to herdr-mcp
            let is_herdr_mcp_related = name.starts_with("dev.herdr-mcp.")
                || name.starts_with(".dev.herdr-mcp.")
                || name.contains(".herdr-mcp-")
                || name.contains(".herdr-backup");

            if !is_herdr_mcp_related {
                continue;
            }

            // Check backup / temporary artifacts
            if is_backup_or_temp_artifact(&name) {
                report.backup_artifacts.push((
                    path,
                    "non-loaded backup/temporary artifact in LaunchAgents".to_owned(),
                ));
                continue;
            }

            // Must end with .plist for normal LaunchAgents
            if !name.ends_with(".plist") {
                report
                    .backup_artifacts
                    .push((path, "non-plist artifact in LaunchAgents".to_owned()));
                continue;
            }

            let label = &name[..name.len() - ".plist".len()];

            // Check if it's default cohort
            if DEFAULT_LABELS.contains(&label) {
                let (state, reason) = load_state_fn(label);
                match state {
                    LaunchdLoadState::Loaded => report.loaded_default.push(path),
                    LaunchdLoadState::NotLoaded => report.configured_not_loaded_default.push(path),
                    LaunchdLoadState::Unknown => report.unknown_load_default.push((path, reason)),
                }
                continue;
            }

            // Check if it's a named instance: dev.herdr-mcp.<name>.<kind>
            if let Some(rest) = label.strip_prefix("dev.herdr-mcp.") {
                if let Some((instance_name, _kind)) = parse_named_label(rest) {
                    match InstanceId::parse(instance_name) {
                        Ok(instance) => {
                            let expected_config = config_parent.join(instance.config_leaf());
                            if expected_config.is_dir() {
                                let (state, reason) = load_state_fn(label);
                                match state {
                                    LaunchdLoadState::Loaded => {
                                        report.loaded_named.push((instance_name.to_owned(), path))
                                    }
                                    LaunchdLoadState::NotLoaded => report
                                        .configured_not_loaded_named
                                        .push((instance_name.to_owned(), path)),
                                    LaunchdLoadState::Unknown => report.unknown_load_named.push((
                                        instance_name.to_owned(),
                                        path,
                                        reason,
                                    )),
                                }
                            } else {
                                report.stale_named.push((
                                    instance_name.to_owned(),
                                    path,
                                    format!(
                                        "missing config directory {}",
                                        expected_config.display()
                                    ),
                                ));
                            }
                        }
                        Err(err) => {
                            report.stale_named.push((
                                instance_name.to_owned(),
                                path,
                                format!("invalid instance name: {err}"),
                            ));
                        }
                    }
                } else {
                    report.stale_named.push((
                        rest.to_owned(),
                        path,
                        "unrecognized named LaunchAgent label shape".to_owned(),
                    ));
                }
            }
        }
    }

    // Inspect config_parent for orphan named-instance config directories
    if let Ok(entries) = fs::read_dir(config_parent) {
        let mut sorted_entries = entries.flatten().map(|e| e.path()).collect::<Vec<_>>();
        sorted_entries.sort();

        for path in sorted_entries {
            if !path.is_dir() {
                continue;
            }
            let Some(file_name_os) = path.file_name() else {
                continue;
            };
            let name = file_name_os.to_string_lossy().into_owned();
            if let Some(instance_name) = name.strip_prefix("herdr-mcp-") {
                if instance_name == "dev" {
                    // herdr-mcp-dev is the managed dev_state_dir
                    continue;
                }
                if let Ok(_instance) = InstanceId::parse(instance_name) {
                    let plist_path = launch_agents_dir
                        .join(format!("dev.herdr-mcp.{instance_name}.server.plist"));
                    if !plist_path.exists() {
                        let already_recorded = report
                            .stale_named
                            .iter()
                            .any(|(n, _, _)| n == instance_name);
                        if !already_recorded {
                            report.stale_named.push((
                                instance_name.to_owned(),
                                path,
                                "orphan named instance config directory without LaunchAgent"
                                    .to_owned(),
                            ));
                        }
                    }
                }
            }
        }
    }

    report
}

#[cfg(any(target_os = "macos", test))]
fn is_backup_or_temp_artifact(name: &str) -> bool {
    name.starts_with('.')
        || name.ends_with(".bak")
        || name.ends_with(".backup")
        || name.ends_with(".old")
        || name.ends_with(".tmp")
        || name.contains(".bak.")
        || name.contains(".backup.")
        || name.contains(".herdr-backup")
        || name.contains(".pre-rust-cutover")
        || name.contains(".tmp-")
        || (name.contains(".plist.") && !name.ends_with(".plist"))
}

#[cfg(any(target_os = "macos", test))]
fn parse_named_label(rest: &str) -> Option<(&str, &str)> {
    if let Some(name) = rest.strip_suffix(".server") {
        Some((name, "server"))
    } else if let Some(name) = rest.strip_suffix(".watchdog") {
        Some((name, "watchdog"))
    } else if let Some(name) = rest.strip_suffix(".health-watchdog") {
        Some((name, "health-watchdog"))
    } else {
        None
    }
}

pub fn doctor_line() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let report = scan_residue(&home);
            return doctor_line_from_report(&report);
        }
        "LAYER lifecycle-residue unavailable reason=no_home".to_owned()
    }
    #[cfg(not(target_os = "macos"))]
    {
        format!(
            "LAYER lifecycle-residue not-applicable platform={}",
            std::env::consts::OS
        )
    }
}

pub fn status_line() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let report = scan_residue(&home);
            return status_line_from_report(&report);
        }
        "unavailable (no HOME)".to_owned()
    }
    #[cfg(not(target_os = "macos"))]
    {
        format!("not applicable on {}", std::env::consts::OS)
    }
}

#[cfg(any(target_os = "macos", test))]
pub fn doctor_line_from_report(report: &ResidueReport) -> String {
    let status = if report.is_clean() {
        "clean"
    } else {
        "residue_detected"
    };
    format!(
        "LAYER lifecycle-residue status={status} loaded_default={} configured_not_loaded_default={} unknown_load_default={} loaded_named={} configured_not_loaded_named={} unknown_load_named={} stale_named={} backup_artifacts={} independent_herdr={}",
        report.loaded_default.len(),
        report.configured_not_loaded_default.len(),
        report.unknown_load_default.len(),
        report.loaded_named.len(),
        report.configured_not_loaded_named.len(),
        report.unknown_load_named.len(),
        report.stale_named.len(),
        report.backup_artifacts.len(),
        report.independent_herdr.len(),
    )
}

#[cfg(any(target_os = "macos", test))]
pub fn status_line_from_report(report: &ResidueReport) -> String {
    if report.is_clean() {
        format!(
            "clean (loaded_default={}, configured_not_loaded_default={}, loaded_named={}, configured_not_loaded_named={})",
            report.loaded_default.len(),
            report.configured_not_loaded_default.len(),
            report.loaded_named.len(),
            report.configured_not_loaded_named.len(),
        )
    } else {
        format!(
            "residue detected (stale_named={}, backup_artifacts={}, unknown_load_default={}, unknown_load_named={}, loaded_default={}, configured_not_loaded_default={})",
            report.stale_named.len(),
            report.backup_artifacts.len(),
            report.unknown_load_default.len(),
            report.unknown_load_named.len(),
            report.loaded_default.len(),
            report.configured_not_loaded_default.len(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCandidateProcess {
    pub pid: u32,
    pub ppid: u32,
    pub executable: String,
    pub instance: Option<String>,
    pub configured_port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanCandidateProcess {
    pub pid: u32,
    pub ppid: u32,
    pub executable: String,
    pub instance: Option<String>,
    pub configured_port: Option<u16>,
    pub listening_ports: Vec<u16>,
    pub ports: Vec<u16>,
    pub reason: String,
    pub advice: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OrphanCandidateReport {
    pub supported: bool,
    pub probe_error: Option<String>,
    pub orphans: Vec<OrphanCandidateProcess>,
}

pub fn parse_ps_candidate_processes(ps_output: &str) -> Vec<ParsedCandidateProcess> {
    let mut results = Vec::new();
    for raw_line in ps_output.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(pid_str) = parts.next() else {
            continue;
        };
        let Some(ppid_str) = parts.next() else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid_str.parse::<u32>(), ppid_str.parse::<u32>()) else {
            continue;
        };
        let tokens: Vec<&str> = parts.collect();
        let Some(&executable) = tokens.first() else {
            continue;
        };
        let exe_leaf = executable.rsplit(['/', '\\']).next().unwrap_or(executable);
        if exe_leaf != "herdr-mcp" && exe_leaf != "herdr-mcp.exe" {
            continue;
        }

        let mut instance: Option<String> = None;
        let mut candidate_index: Option<usize> = None;
        let mut idx = 1;
        while idx < tokens.len() {
            let token = tokens[idx];
            if token == "--instance" {
                if let Some(&name) = tokens.get(idx + 1) {
                    instance = Some(name.to_owned());
                    idx += 2;
                    continue;
                }
                break;
            } else if let Some(name) = token.strip_prefix("--instance=") {
                if !name.is_empty() {
                    instance = Some(name.to_owned());
                }
                idx += 1;
                continue;
            } else if matches!(token, "-v" | "--verbose" | "--json") {
                idx += 1;
                continue;
            } else if token == "candidate" {
                candidate_index = Some(idx);
                break;
            } else {
                break;
            }
        }

        let Some(sub_idx) = candidate_index else {
            continue;
        };

        let mut configured_port: Option<u16> = None;
        let mut arg_idx = sub_idx + 1;
        while arg_idx < tokens.len() {
            let token = tokens[arg_idx];
            if token == "--port" {
                if let Some(port_str) = tokens.get(arg_idx + 1)
                    && let Ok(port) = port_str.parse::<u16>()
                {
                    configured_port = Some(port);
                }
                arg_idx += 2;
            } else if let Some(port_str) = token.strip_prefix("--port=") {
                if let Ok(port) = port_str.parse::<u16>() {
                    configured_port = Some(port);
                }
                arg_idx += 1;
            } else {
                arg_idx += 1;
            }
        }

        results.push(ParsedCandidateProcess {
            pid,
            ppid,
            executable: executable.to_owned(),
            instance,
            configured_port,
        });
    }
    results.sort_by_key(|entry| entry.pid);
    results
}

pub fn parse_launchctl_server_pids(launchctl_output: &str) -> BTreeMap<u32, String> {
    let mut map = BTreeMap::new();
    for raw_line in launchctl_output.lines() {
        let mut cols = raw_line.split_whitespace();
        let Some(pid_col) = cols.next() else {
            continue;
        };
        let Some(_status_col) = cols.next() else {
            continue;
        };
        let Some(label) = cols.next() else {
            continue;
        };
        let Ok(pid) = pid_col.parse::<u32>() else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        if label == "dev.herdr-mcp.server"
            || (label.starts_with("dev.herdr-mcp.") && label.ends_with(".server"))
        {
            map.insert(pid, label.to_owned());
        }
    }
    map
}

pub fn parse_lsof_listening_ports(lsof_output: &str) -> BTreeMap<u32, Vec<u16>> {
    let mut ports_by_pid: BTreeMap<u32, BTreeSet<u16>> = BTreeMap::new();
    let mut current_pid: Option<u32> = None;
    for raw_line in lsof_output.lines() {
        let line = raw_line.trim();
        if let Some(pid_str) = line.strip_prefix('p') {
            current_pid = pid_str.parse::<u32>().ok();
        } else if let Some(name_str) = line.strip_prefix('n')
            && let Some(pid) = current_pid
        {
            let endpoint = name_str.split("->").next().unwrap_or(name_str);
            if let Some((_, port_str)) = endpoint.rsplit_once(':')
                && let Ok(port) = port_str.parse::<u16>()
            {
                ports_by_pid.entry(pid).or_default().insert(port);
            }
        }
    }
    ports_by_pid
        .into_iter()
        .map(|(pid, set)| (pid, set.into_iter().collect()))
        .collect()
}

pub fn scan_orphan_candidates_macos_with<PortsFn: Fn(&[u32]) -> BTreeMap<u32, Vec<u16>>>(
    home: &Path,
    ps_output: &str,
    launchctl_output: &str,
    listening_ports_fn: PortsFn,
) -> OrphanCandidateReport {
    let candidates = parse_ps_candidate_processes(ps_output);
    let server_labels = parse_launchctl_server_pids(launchctl_output);
    let launch_agents = home.join("Library/LaunchAgents");
    let config_parent = home.join(".config");

    let mut preliminary: Vec<(ParsedCandidateProcess, String, String)> = Vec::new();
    for proc in candidates {
        match server_labels.get(&proc.pid) {
            Some(label) if label == "dev.herdr-mcp.server" => {
                let plist = launch_agents.join("dev.herdr-mcp.server.plist");
                let config_dir = config_parent.join("herdr-mcp");
                let canonical_exec = config_dir.join("runtime/current/herdr-mcp");
                if plist.is_file() && config_dir.is_dir() {
                    if Path::new(&proc.executable) != canonical_exec {
                        preliminary.push((
                            proc.clone(),
                            "noncanonical_launchd_binary".to_owned(),
                            format!(
                                "Reinstall the default service so launchd executes {} instead of {}",
                                canonical_exec.display(),
                                proc.executable
                            ),
                        ));
                    }
                } else {
                    preliminary.push((
                        proc.clone(),
                        "stale_default_launchd_service".to_owned(),
                        format!(
                            "Unload stale LaunchAgent '{label}' or reinstall the default service, then verify PID {} exited",
                            proc.pid
                        ),
                    ));
                }
            }
            Some(label) => {
                let instance_name = label
                    .strip_prefix("dev.herdr-mcp.")
                    .and_then(|rest| rest.strip_suffix(".server"));
                match instance_name
                    .and_then(|name| InstanceId::parse(name).ok().map(|id| (name, id)))
                {
                    Some((name, instance)) => {
                        let plist =
                            launch_agents.join(format!("dev.herdr-mcp.{name}.server.plist"));
                        let config_dir = config_parent.join(instance.config_leaf());
                        let canonical_exec = config_dir.join("runtime/current/herdr-mcp");
                        if plist.is_file() && config_dir.is_dir() {
                            if Path::new(&proc.executable) != canonical_exec {
                                preliminary.push((
                                    proc.clone(),
                                    format!("noncanonical_named_instance_binary:{name}"),
                                    format!(
                                        "Reinstall named instance '{name}' so launchd executes {} instead of {}",
                                        canonical_exec.display(),
                                        proc.executable
                                    ),
                                ));
                            }
                        } else {
                            preliminary.push((
                                proc.clone(),
                                format!("stale_named_launchd_service:{name}"),
                                format!(
                                    "Unload stale LaunchAgent '{label}' or run 'herdr-mcp instance uninstall --name {name}', then verify PID {} exited",
                                    proc.pid
                                ),
                            ));
                        }
                    }
                    None => {
                        preliminary.push((
                            proc.clone(),
                            format!("invalid_named_launchd_service:{label}"),
                            format!(
                                "Unload unrecognized LaunchAgent '{label}' and stop PID {} manually: kill {}",
                                proc.pid, proc.pid
                            ),
                        ));
                    }
                }
            }
            None => {
                preliminary.push((
                    proc.clone(),
                    "unmanaged_candidate_process".to_owned(),
                    format!(
                        "Verify PID {} is not an active test/worktree session, then stop it manually: kill {}",
                        proc.pid, proc.pid
                    ),
                ));
            }
        }
    }

    let orphan_pids: Vec<u32> = preliminary.iter().map(|(proc, _, _)| proc.pid).collect();
    let listening_by_pid = if orphan_pids.is_empty() {
        BTreeMap::new()
    } else {
        listening_ports_fn(&orphan_pids)
    };

    let orphans = preliminary
        .into_iter()
        .map(|(proc, reason, advice)| {
            let listening_ports = listening_by_pid.get(&proc.pid).cloned().unwrap_or_default();
            let mut merged_ports: BTreeSet<u16> = listening_ports.iter().copied().collect();
            if let Some(port) = proc.configured_port {
                merged_ports.insert(port);
            }
            OrphanCandidateProcess {
                pid: proc.pid,
                ppid: proc.ppid,
                executable: proc.executable,
                instance: proc.instance,
                configured_port: proc.configured_port,
                listening_ports,
                ports: merged_ports.into_iter().collect(),
                reason,
                advice,
            }
        })
        .collect();

    OrphanCandidateReport {
        supported: true,
        probe_error: None,
        orphans,
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn query_lsof_listening_ports(pids: &[u32]) -> BTreeMap<u32, Vec<u16>> {
    if pids.is_empty() {
        return BTreeMap::new();
    }
    let pid_csv = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    for bin in ["/usr/sbin/lsof", "/usr/bin/lsof", "lsof"] {
        if let Ok(output) = std::process::Command::new(bin)
            .args(["-Pan", "-p", &pid_csv, "-iTCP", "-sTCP:LISTEN", "-Fn"])
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            if !text.trim().is_empty() || output.status.success() {
                return parse_lsof_listening_ports(&text);
            }
        }
    }
    BTreeMap::new()
}

pub fn scan_orphan_candidates(home: &Path) -> OrphanCandidateReport {
    #[cfg(target_os = "macos")]
    {
        let ps_out = match std::process::Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid=,command="])
            .output()
        {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
            Ok(out) => {
                return OrphanCandidateReport {
                    supported: true,
                    probe_error: Some(format!("ps exited with {}", out.status)),
                    orphans: Vec::new(),
                };
            }
            Err(err) => {
                return OrphanCandidateReport {
                    supported: true,
                    probe_error: Some(format!("cannot spawn ps: {err}")),
                    orphans: Vec::new(),
                };
            }
        };
        let launchctl_out = match std::process::Command::new("/bin/launchctl")
            .arg("list")
            .output()
        {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
            Ok(out) => {
                return OrphanCandidateReport {
                    supported: true,
                    probe_error: Some(format!("launchctl list exited with {}", out.status)),
                    orphans: Vec::new(),
                };
            }
            Err(err) => {
                return OrphanCandidateReport {
                    supported: true,
                    probe_error: Some(format!("cannot spawn launchctl: {err}")),
                    orphans: Vec::new(),
                };
            }
        };
        scan_orphan_candidates_macos_with(home, &ps_out, &launchctl_out, query_lsof_listening_ports)
    }
    #[cfg(target_os = "linux")]
    {
        let ps_out = match std::process::Command::new("ps")
            .args(["-eo", "pid=,ppid=,args="])
            .output()
        {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
            Ok(out) => {
                return OrphanCandidateReport {
                    supported: true,
                    probe_error: Some(format!("ps exited with {}", out.status)),
                    orphans: Vec::new(),
                };
            }
            Err(err) => {
                return OrphanCandidateReport {
                    supported: true,
                    probe_error: Some(format!("cannot spawn ps: {err}")),
                    orphans: Vec::new(),
                };
            }
        };
        let mut managed_pids = BTreeSet::new();
        if let Ok(out) = std::process::Command::new("systemctl")
            .args([
                "--user",
                "show",
                "herdr-mcp.service",
                "-p",
                "MainPID",
                "--value",
            ])
            .output()
            && out.status.success()
            && let Ok(pid) = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>()
            && pid > 0
        {
            managed_pids.insert(pid);
        }
        let config_parent = home.join(".config");
        if let Ok(entries) = fs::read_dir(&config_parent) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                if name == "herdr-mcp" || name.starts_with("herdr-mcp-") {
                    let proc_file = path.join("runtime/service-process.json");
                    if let Ok(bytes) = fs::read(&proc_file)
                        && let Ok(val) = serde_json::from_slice::<Value>(&bytes)
                        && let Some(pid) = val
                            .get("server_pid")
                            .and_then(Value::as_u64)
                            .and_then(|v| u32::try_from(v).ok())
                        && pid > 0
                    {
                        managed_pids.insert(pid);
                    }
                }
            }
        }
        let candidates = parse_ps_candidate_processes(&ps_out);
        let preliminary: Vec<ParsedCandidateProcess> = candidates
            .into_iter()
            .filter(|proc| !managed_pids.contains(&proc.pid))
            .collect();
        let pids: Vec<u32> = preliminary.iter().map(|p| p.pid).collect();
        let listening_by_pid = query_lsof_listening_ports(&pids);
        let orphans = preliminary
            .into_iter()
            .map(|proc| {
                let listening_ports = listening_by_pid.get(&proc.pid).cloned().unwrap_or_default();
                let mut merged_ports: BTreeSet<u16> = listening_ports.iter().copied().collect();
                if let Some(port) = proc.configured_port {
                    merged_ports.insert(port);
                }
                OrphanCandidateProcess {
                    pid: proc.pid,
                    ppid: proc.ppid,
                    executable: proc.executable,
                    instance: proc.instance,
                    configured_port: proc.configured_port,
                    listening_ports,
                    ports: merged_ports.into_iter().collect(),
                    reason: "unmanaged_candidate_process".to_owned(),
                    advice: format!(
                        "Verify PID {} is not an active test/worktree session, then stop it manually: kill {}",
                        proc.pid, proc.pid
                    ),
                }
            })
            .collect();
        OrphanCandidateReport {
            supported: true,
            probe_error: None,
            orphans,
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = home;
        OrphanCandidateReport {
            supported: false,
            probe_error: None,
            orphans: Vec::new(),
        }
    }
}

pub fn format_ports_csv(ports: &[u16]) -> String {
    if ports.is_empty() {
        "none".to_owned()
    } else {
        ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }
}

pub fn orphan_candidates_doctor_line(report: &OrphanCandidateReport) -> String {
    if !report.supported {
        return format!(
            "LAYER orphan-candidates not-applicable platform={}",
            std::env::consts::OS
        );
    }
    if let Some(error) = &report.probe_error {
        return format!("LAYER orphan-candidates status=probe_failed detail={error}");
    }
    if report.orphans.is_empty() {
        return "LAYER orphan-candidates status=clean count=0".to_owned();
    }
    let pids = report
        .orphans
        .iter()
        .map(|entry| entry.pid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut all_ports = BTreeSet::new();
    for entry in &report.orphans {
        for &port in &entry.ports {
            all_ports.insert(port);
        }
    }
    let ports_csv = if all_ports.is_empty() {
        "none".to_owned()
    } else {
        all_ports
            .into_iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "LAYER orphan-candidates status=orphans_detected count={} pids={pids} ports={ports_csv}",
        report.orphans.len()
    )
}

pub fn orphan_candidates_warning_lines(report: &OrphanCandidateReport) -> Vec<String> {
    report
        .orphans
        .iter()
        .map(|entry| {
            format!(
                "WARN orphan-candidate pid={} ppid={} ports={} executable={} reason={} advice=\"{}\"",
                entry.pid,
                entry.ppid,
                format_ports_csv(&entry.ports),
                entry.executable,
                entry.reason,
                entry.advice
            )
        })
        .collect()
}

pub fn orphan_candidates_attention_lines(
    report: &OrphanCandidateReport,
    language: crate::locale::Locale,
) -> Vec<String> {
    report
        .orphans
        .iter()
        .map(|entry| {
            let ports = format_ports_csv(&entry.ports);
            match language {
                crate::locale::Locale::ZhCn => format!(
                    "检测到未受管的 herdr-mcp candidate 孤儿进程 pid={} 端口={} ({}, 原因={}) · 建议（只读诊断，不自动杀进程）：{}",
                    entry.pid, ports, entry.executable, entry.reason, entry.advice
                ),
                crate::locale::Locale::Ja => format!(
                    "管理対象外の herdr-mcp candidate プロセスを検出 pid={} ポート={} ({}, 理由={}) · 推奨（自動終了しません）：{}",
                    entry.pid, ports, entry.executable, entry.reason, entry.advice
                ),
                crate::locale::Locale::En => format!(
                    "unmanaged herdr-mcp candidate process pid={} ports={} ({}, reason={}) · advice (read-only, never auto-killed): {}",
                    entry.pid, ports, entry.executable, entry.reason, entry.advice
                ),
            }
        })
        .collect()
}

pub fn orphan_candidates_json(report: &OrphanCandidateReport) -> Value {
    if !report.supported {
        return json!({
            "state": "not_supported",
            "count": 0,
            "processes": [],
        });
    }
    if let Some(error) = &report.probe_error {
        return json!({
            "state": "probe_failed",
            "count": 0,
            "error": error,
            "processes": [],
        });
    }
    let processes: Vec<Value> = report
        .orphans
        .iter()
        .map(|entry| {
            json!({
                "pid": entry.pid,
                "ppid": entry.ppid,
                "executable": entry.executable,
                "instance": entry.instance,
                "configured_port": entry.configured_port,
                "listening_ports": entry.listening_ports,
                "ports": entry.ports,
                "reason": entry.reason,
                "advice": entry.advice,
            })
        })
        .collect();
    json!({
        "state": if processes.is_empty() { "clean" } else { "orphans_detected" },
        "count": processes.len(),
        "processes": processes,
        "advice": if processes.is_empty() {
            Value::Null
        } else {
            json!("Inspect unmanaged herdr-mcp candidate processes above and stop leftover processes manually (for example: kill <pid>). doctor never terminates processes automatically.")
        },
    })
}

#[cfg(test)]
pub fn doctor_line_with<Loaded: Fn(&str) -> (LaunchdLoadState, String)>(
    launch_agents_dir: &Path,
    config_parent: &Path,
    is_loaded: Loaded,
) -> String {
    let report = scan_residue_with(launch_agents_dir, config_parent, is_loaded);
    doctor_line_from_report(&report)
}

#[cfg(test)]
pub fn status_line_with<Loaded: Fn(&str) -> (LaunchdLoadState, String)>(
    launch_agents_dir: &Path,
    config_parent: &Path,
    is_loaded: Loaded,
) -> String {
    let report = scan_residue_with(launch_agents_dir, config_parent, is_loaded);
    status_line_from_report(&report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_test_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "herdr-residue-test-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn clean_environment_classifies_all_three_load_states() {
        let root = temp_test_dir("clean-tristate");
        let agents = root.join("Library/LaunchAgents");
        let config = root.join(".config");
        fs::create_dir_all(&agents).unwrap();
        fs::create_dir_all(&config).unwrap();

        // Default cohort files
        fs::write(agents.join("dev.herdr-mcp.server.plist"), b"<plist/>").unwrap();
        fs::write(agents.join("dev.herdr-mcp.link-prod.plist"), b"<plist/>").unwrap();
        fs::write(
            agents.join("dev.herdr-mcp.herdr-supervisor.plist"),
            b"<plist/>",
        )
        .unwrap();
        fs::write(agents.join("dev.herdr-mcp.auto-update.plist"), b"<plist/>").unwrap();

        // Legitimate named instances
        fs::write(agents.join("dev.herdr-mcp.uat.server.plist"), b"<plist/>").unwrap();
        fs::create_dir_all(config.join("herdr-mcp-uat")).unwrap();
        fs::write(
            agents.join("dev.herdr-mcp.staging.server.plist"),
            b"<plist/>",
        )
        .unwrap();
        fs::create_dir_all(config.join("herdr-mcp-staging")).unwrap();

        // Independent Herdr
        fs::write(agents.join("dev.herdr.server.plist"), b"<plist/>").unwrap();

        // Predicate mapping:
        // - server: Loaded
        // - link-prod: NotLoaded
        // - herdr-supervisor: Loaded
        // - auto-update: NotLoaded
        // - uat: Loaded
        // - staging: NotLoaded
        let load_fn = |label: &str| match label {
            "dev.herdr-mcp.server" => (LaunchdLoadState::Loaded, "loaded".to_owned()),
            "dev.herdr-mcp.link-prod" => (LaunchdLoadState::NotLoaded, "not loaded".to_owned()),
            "dev.herdr-mcp.herdr-supervisor" => (LaunchdLoadState::Loaded, "loaded".to_owned()),
            "dev.herdr-mcp.auto-update" => (LaunchdLoadState::NotLoaded, "not loaded".to_owned()),
            "dev.herdr-mcp.uat.server" => (LaunchdLoadState::Loaded, "loaded".to_owned()),
            "dev.herdr-mcp.staging.server" => {
                (LaunchdLoadState::NotLoaded, "not loaded".to_owned())
            }
            "dev.herdr.server" => (LaunchdLoadState::Loaded, "loaded".to_owned()),
            _ => (LaunchdLoadState::NotLoaded, "not loaded".to_owned()),
        };

        let report = scan_residue_with(&agents, &config, load_fn);
        assert!(report.is_clean());
        assert_eq!(report.loaded_default.len(), 2);
        assert_eq!(report.configured_not_loaded_default.len(), 2);
        assert_eq!(report.unknown_load_default.len(), 0);
        assert_eq!(report.loaded_named.len(), 1);
        assert_eq!(report.loaded_named[0].0, "uat");
        assert_eq!(report.configured_not_loaded_named.len(), 1);
        assert_eq!(report.configured_not_loaded_named[0].0, "staging");
        assert_eq!(report.unknown_load_named.len(), 0);
        assert_eq!(report.stale_named.len(), 0);
        assert_eq!(report.backup_artifacts.len(), 0);
        assert_eq!(report.independent_herdr.len(), 1);
        assert_eq!(report.independent_herdr[0].1, LaunchdLoadState::Loaded);

        let doc = doctor_line_with(&agents, &config, load_fn);
        assert!(doc.contains("status=clean"));
        assert!(doc.contains("loaded_default=2"));
        assert!(doc.contains("configured_not_loaded_default=2"));
        assert!(doc.contains("unknown_load_default=0"));
        assert!(doc.contains("loaded_named=1"));
        assert!(doc.contains("configured_not_loaded_named=1"));
        assert!(doc.contains("unknown_load_named=0"));

        let stat = status_line_with(&agents, &config, load_fn);
        assert!(stat.contains("clean"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn observation_failure_produces_unknown_not_configured_not_loaded() {
        let root = temp_test_dir("observation-failure");
        let agents = root.join("Library/LaunchAgents");
        let config = root.join(".config");
        fs::create_dir_all(&agents).unwrap();
        fs::create_dir_all(&config).unwrap();

        fs::write(agents.join("dev.herdr-mcp.server.plist"), b"<plist/>").unwrap();
        fs::write(agents.join("dev.herdr-mcp.uat.server.plist"), b"<plist/>").unwrap();
        fs::create_dir_all(config.join("herdr-mcp-uat")).unwrap();

        // Injected observation error (e.g., launchctl permission error or spawn timeout)
        let error_fn = |_label: &str| {
            (
                LaunchdLoadState::Unknown,
                "launchctl print: permission denied".to_owned(),
            )
        };

        let report = scan_residue_with(&agents, &config, error_fn);
        // Observation failures prevent declaring the environment clean
        assert!(!report.is_clean());
        // Must NOT be reported as confirmed loaded
        assert_eq!(report.loaded_default.len(), 0);
        assert_eq!(report.loaded_named.len(), 0);
        // Must NOT be falsely reported as confirmed not-loaded!
        assert_eq!(report.configured_not_loaded_default.len(), 0);
        assert_eq!(report.configured_not_loaded_named.len(), 0);
        // Must be reported as unknown
        assert_eq!(report.unknown_load_default.len(), 1);
        assert_eq!(report.unknown_load_named.len(), 1);
        assert_eq!(report.unknown_load_named[0].0, "uat");
        assert_eq!(
            report.unknown_load_default[0].1,
            "launchctl print: permission denied"
        );

        let doc = doctor_line_with(&agents, &config, error_fn);
        assert!(doc.contains("status=residue_detected"));
        assert!(doc.contains("loaded_default=0"));
        assert!(doc.contains("configured_not_loaded_default=0"));
        assert!(doc.contains("unknown_load_default=1"));
        assert!(doc.contains("unknown_load_named=1"));

        let stat = status_line_with(&agents, &config, error_fn);
        assert!(stat.contains("unknown_load_default=1"));
        assert!(stat.contains("unknown_load_named=1"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn detects_stale_named_instances_and_backup_artifacts() {
        let root = temp_test_dir("residue");
        let agents = root.join("Library/LaunchAgents");
        let config = root.join(".config");
        fs::create_dir_all(&agents).unwrap();
        fs::create_dir_all(&config).unwrap();

        // Default active cohort
        fs::write(agents.join("dev.herdr-mcp.server.plist"), b"<plist/>").unwrap();

        // Stale named instance (plist present, config missing)
        fs::write(
            agents.join("dev.herdr-mcp.orphan-uat.server.plist"),
            b"<plist/>",
        )
        .unwrap();

        // Orphan named instance (config present, plist missing)
        fs::create_dir_all(config.join("herdr-mcp-abandoned")).unwrap();

        // Backup / temp artifacts under LaunchAgents
        fs::write(agents.join("dev.herdr-mcp.server.plist.bak"), b"old-backup").unwrap();
        fs::write(
            agents.join(".dev.herdr-mcp.server.plist.tmp-123-456"),
            b"temp",
        )
        .unwrap();
        fs::write(
            agents.join("dev.herdr-mcp.link-prod.plist.pre-rust-cutover"),
            b"cutover-backup",
        )
        .unwrap();

        let load_fn = |label: &str| {
            if label == "dev.herdr-mcp.server" {
                (LaunchdLoadState::Loaded, "loaded".to_owned())
            } else {
                (LaunchdLoadState::NotLoaded, "not loaded".to_owned())
            }
        };

        let report = scan_residue_with(&agents, &config, load_fn);
        assert!(!report.is_clean());
        assert_eq!(report.loaded_default.len(), 1);
        assert_eq!(report.configured_not_loaded_default.len(), 0);
        assert_eq!(report.unknown_load_default.len(), 0);
        assert_eq!(report.loaded_named.len(), 0);
        assert_eq!(report.configured_not_loaded_named.len(), 0);
        assert_eq!(report.unknown_load_named.len(), 0);
        assert_eq!(report.stale_named.len(), 2);
        assert!(
            report
                .stale_named
                .iter()
                .any(|(name, _, _)| name == "orphan-uat")
        );
        assert!(
            report
                .stale_named
                .iter()
                .any(|(name, _, _)| name == "abandoned")
        );
        assert_eq!(report.backup_artifacts.len(), 3);

        let doc = doctor_line_with(&agents, &config, load_fn);
        assert!(doc.contains("status=residue_detected"));
        assert!(doc.contains("stale_named=2"));
        assert!(doc.contains("backup_artifacts=3"));

        let stat = status_line_with(&agents, &config, load_fn);
        assert!(stat.contains("residue detected"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn orphan_candidate_scan_distinguishes_managed_instances_from_unmanaged_and_stale_processes() {
        let root = temp_test_dir("orphan-candidates");
        let agents = root.join("Library/LaunchAgents");
        let config = root.join(".config");
        fs::create_dir_all(&agents).unwrap();
        fs::create_dir_all(config.join("herdr-mcp/runtime/current")).unwrap();
        fs::create_dir_all(config.join("herdr-mcp-dev698/runtime/current")).unwrap();
        fs::write(agents.join("dev.herdr-mcp.server.plist"), b"<plist/>").unwrap();
        fs::write(
            agents.join("dev.herdr-mcp.dev698.server.plist"),
            b"<plist/>",
        )
        .unwrap();

        let default_bin = config.join("herdr-mcp/runtime/current/herdr-mcp");
        let named_bin = config.join("herdr-mcp-dev698/runtime/current/herdr-mcp");
        let ps_output = format!(
            " 1033     1 {} herdr-supervisor run\n\
             25012     1 {} candidate --port 8772\n\
             25054     1 {} link run\n\
             17380     1 {} candidate --port 8991\n\
             48899     1 /bin/bash -c pkill -f \"herdr-mcp candidate --port 18731\"; /tmp/wt/target/debug/herdr-mcp candidate --port 18731\n\
             48903 48899 /tmp/wt/target/debug/herdr-mcp candidate --port 18731\n\
             49994     1 /tmp/wt/target/debug/herdr-mcp --instance=tmp candidate --port=59673\n\
             51000     1 /Users/test/.config/herdr-mcp-ghost/runtime/current/herdr-mcp candidate --port 9110\n",
            default_bin.display(),
            default_bin.display(),
            default_bin.display(),
            named_bin.display(),
        );
        let launchctl_output = "\
PID\tStatus\tLabel\n\
17380\t0\tdev.herdr-mcp.dev698.server\n\
-\t0\tdev.herdr-mcp.health-watchdog\n\
25054\t0\tdev.herdr-mcp.link-prod\n\
1033\t0\tdev.herdr-mcp.herdr-supervisor\n\
25012\t0\tdev.herdr-mcp.server\n\
51000\t0\tdev.herdr-mcp.ghost.server\n";
        let lsof_output = "\
p48903\n\
f15\n\
n127.0.0.1:18731\n\
p49994\n\
f17\n\
n127.0.0.1:59673\n\
n[::1]:59674\n\
p51000\n\
f18\n\
n127.0.0.1:9110\n";

        let report =
            scan_orphan_candidates_macos_with(&root, &ps_output, launchctl_output, |pids| {
                assert_eq!(pids, &[48903, 49994, 51000]);
                parse_lsof_listening_ports(lsof_output)
            });

        assert!(report.supported);
        assert_eq!(report.probe_error, None);
        assert_eq!(report.orphans.len(), 3);

        assert_eq!(report.orphans[0].pid, 48903);
        assert_eq!(report.orphans[0].ppid, 48899);
        assert_eq!(report.orphans[0].configured_port, Some(18731));
        assert_eq!(report.orphans[0].listening_ports, vec![18731]);
        assert_eq!(report.orphans[0].ports, vec![18731]);
        assert_eq!(report.orphans[0].reason, "unmanaged_candidate_process");
        assert!(report.orphans[0].advice.contains("kill 48903"));

        assert_eq!(report.orphans[1].pid, 49994);
        assert_eq!(report.orphans[1].instance.as_deref(), Some("tmp"));
        assert_eq!(report.orphans[1].configured_port, Some(59673));
        assert_eq!(report.orphans[1].listening_ports, vec![59673, 59674]);
        assert_eq!(report.orphans[1].ports, vec![59673, 59674]);
        assert_eq!(report.orphans[1].reason, "unmanaged_candidate_process");

        assert_eq!(report.orphans[2].pid, 51000);
        assert_eq!(report.orphans[2].ports, vec![9110]);
        assert_eq!(
            report.orphans[2].reason,
            "stale_named_launchd_service:ghost"
        );
        assert!(
            report.orphans[2]
                .advice
                .contains("herdr-mcp instance uninstall --name ghost")
        );

        let layer = orphan_candidates_doctor_line(&report);
        assert_eq!(
            layer,
            "LAYER orphan-candidates status=orphans_detected count=3 pids=48903,49994,51000 ports=9110,18731,59673,59674"
        );
        let warnings = orphan_candidates_warning_lines(&report);
        assert_eq!(warnings.len(), 3);
        assert!(warnings[0].contains("WARN orphan-candidate pid=48903 ppid=48899 ports=18731"));
        let attention = orphan_candidates_attention_lines(&report, crate::locale::Locale::ZhCn);
        assert_eq!(attention.len(), 3);
        assert!(attention[0].contains("不自动杀进程"));
        let json_val = orphan_candidates_json(&report);
        assert_eq!(json_val["state"], "orphans_detected");
        assert_eq!(json_val["count"], 3);
        assert_eq!(json_val["processes"][0]["pid"], 48903);
        assert_eq!(json_val["processes"][0]["ports"], json!([18731]));

        let clean_report = scan_orphan_candidates_macos_with(
            &root,
            &format!(
                "25012 1 {} candidate --port 8772\n17380 1 {} candidate --port 8991\n",
                default_bin.display(),
                named_bin.display()
            ),
            launchctl_output,
            |_| BTreeMap::new(),
        );
        assert!(clean_report.orphans.is_empty());
        assert_eq!(
            orphan_candidates_doctor_line(&clean_report),
            "LAYER orphan-candidates status=clean count=0"
        );
        assert_eq!(orphan_candidates_json(&clean_report)["state"], "clean");

        let _ = fs::remove_dir_all(root);
    }
}
