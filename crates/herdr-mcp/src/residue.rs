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

use crate::instance::{
    DEFAULT_HEALTH_WATCHDOG_LABEL, DEFAULT_HERDR_SUPERVISOR_LABEL, DEFAULT_SERVICE_LABEL,
    DEFAULT_WATCHDOG_LABEL, InstanceId,
};
use std::fs;
use std::path::{Path, PathBuf};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchdLoadState {
    Loaded,
    NotLoaded,
    Unknown,
}

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

impl ResidueReport {
    pub fn is_clean(&self) -> bool {
        self.stale_named.is_empty()
            && self.backup_artifacts.is_empty()
            && self.unknown_load_default.is_empty()
            && self.unknown_load_named.is_empty()
    }
}

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
}
