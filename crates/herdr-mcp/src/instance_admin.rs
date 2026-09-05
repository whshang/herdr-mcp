use crate::cli::InstanceCommand;
use crate::config::Config;
use crate::instance::InstanceId;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn run(command: InstanceCommand) -> Result<ExitCode, String> {
    match command {
        InstanceCommand::List => {
            let home = home_dir()?;
            let instances = discover_instances(&home, launchd_status);
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "instances": instances,
                }))
                .map_err(|error| format!("cannot encode named instance inventory: {error}"))?
            );
            Ok(ExitCode::SUCCESS)
        }
        InstanceCommand::Reap { name } => {
            if name == "default" {
                return Err("the default production instance cannot be reaped".to_owned());
            }
            let id = InstanceId::parse(&name)?;
            if !id.is_named() {
                return Err("the default instance cannot be reaped".to_owned());
            }
            let home = home_dir()?;
            let before = discover_instances(&home, launchd_status);
            if !before
                .iter()
                .any(|instance| instance.name == name && instance.reappable)
            {
                return Err(format!("named instance '{name}' was not found"));
            }

            // Reuse the product-lifecycle ownership/preflight transaction. It
            // refuses path-shape-only deletion and keeps named-instance cleanup
            // separate from the default install and immutable release store.
            unsafe { std::env::set_var("HERDR_MCP_INSTANCE", &name) };
            let code = crate::product_lifecycle::uninstall()?;
            let after = discover_instances(&home, launchd_status);
            if after.iter().any(|instance| instance.name == name) {
                return Err(format!(
                    "named instance '{name}' still has recognized residue after uninstall"
                ));
            }
            Ok(code)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct InstanceInventory {
    name: String,
    kind: &'static str,
    reappable: bool,
    label: String,
    port: Option<u16>,
    config_dir: String,
    plist: String,
    config_present: bool,
    plist_present: bool,
    loaded: bool,
    running: bool,
    pid: Option<u32>,
    age_ms: Option<u64>,
    age: String,
    state: &'static str,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LaunchdStatus {
    loaded: bool,
    running: bool,
    pid: Option<u32>,
}

fn discover_instances(
    home: &Path,
    launchd: impl Fn(&str) -> LaunchdStatus,
) -> Vec<InstanceInventory> {
    let mut names = BTreeSet::new();
    let config_parent = home.join(".config");
    if let Ok(entries) = fs::read_dir(&config_parent) {
        for entry in entries.flatten() {
            let entry_name = entry.file_name().to_string_lossy().into_owned();
            let Some(suffix) = entry_name.strip_prefix("herdr-mcp-") else {
                continue;
            };
            if InstanceId::parse(suffix).is_ok_and(|id| id.is_named()) {
                names.insert(suffix.to_owned());
            }
        }
    }

    let launch_agents = home.join("Library/LaunchAgents");
    if let Ok(entries) = fs::read_dir(&launch_agents) {
        for entry in entries.flatten() {
            let entry_name = entry.file_name().to_string_lossy().into_owned();
            let Some(suffix) = entry_name
                .strip_prefix("dev.herdr-mcp.")
                .and_then(|value| value.strip_suffix(".server.plist"))
            else {
                continue;
            };
            if InstanceId::parse(suffix).is_ok_and(|id| id.is_named()) {
                names.insert(suffix.to_owned());
            }
        }
    }

    let mut instances = Vec::with_capacity(names.len() + 1);
    let default = InstanceId::default_instance();
    let default_label = default.service_label();
    let default_config = config_parent.join(default.config_leaf());
    let default_plist = launch_agents.join(format!("{default_label}.plist"));
    let default_launchd = launchd(&default_label);
    if path_present(&default_config) || path_present(&default_plist) || default_launchd.loaded {
        instances.push(inventory_row(
            "default".to_owned(),
            default,
            false,
            &config_parent,
            &launch_agents,
            default_launchd,
        ));
    }

    instances.extend(names.into_iter().map(|name| {
        let id = InstanceId::parse(&name).expect("discovery already validated instance name");
        let status = launchd(&id.service_label());
        inventory_row(name, id, true, &config_parent, &launch_agents, status)
    }));
    instances
}

fn inventory_row(
    name: String,
    id: InstanceId,
    reappable: bool,
    config_parent: &Path,
    launch_agents: &Path,
    launchd: LaunchdStatus,
) -> InstanceInventory {
    let label = id.service_label();
    let config_dir = config_parent.join(id.config_leaf());
    let plist = launch_agents.join(format!("{label}.plist"));
    let config_present = path_present(&config_dir);
    let plist_present = path_present(&plist);
    let state = match (config_present, plist_present, launchd.loaded) {
        (true, true, true) => "installed",
        (true, true, false) => "installed_unloaded",
        (true, false, _) => "orphan_config",
        (false, true, false) => "orphan_plist",
        (false, true, true) => "loaded_without_config",
        (false, false, true) => "loaded_without_artifacts",
        (false, false, false) => "absent",
    };
    let config_file = config_dir.join("config.toml");
    let port = if config_present {
        Config::load_for_instance(&config_file, &id)
            .ok()
            .map(|config| config.runtime_port)
    } else {
        Some(id.default_port())
    };
    let first_seen_at_ms = [artifact_time_ms(&config_dir), artifact_time_ms(&plist)]
        .into_iter()
        .flatten()
        .min();
    let age_ms = first_seen_at_ms.map(|created| now_ms().saturating_sub(created));
    InstanceInventory {
        name,
        kind: if reappable { "named" } else { "default" },
        reappable,
        label,
        port,
        config_dir: config_dir.to_string_lossy().into_owned(),
        plist: plist.to_string_lossy().into_owned(),
        config_present,
        plist_present,
        loaded: launchd.loaded,
        running: launchd.running,
        pid: launchd.pid,
        age_ms,
        age: age_ms
            .map(format_age)
            .unwrap_or_else(|| "unknown".to_owned()),
        state,
    }
}

fn path_present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn artifact_time_ms(path: &Path) -> Option<u64> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let time = metadata.created().or_else(|_| metadata.modified()).ok()?;
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn format_age(age_ms: u64) -> String {
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if age_ms >= DAY {
        format!("{}d", age_ms / DAY)
    } else if age_ms >= HOUR {
        format!("{}h", age_ms / HOUR)
    } else if age_ms >= MINUTE {
        format!("{}m", age_ms / MINUTE)
    } else {
        format!("{}s", age_ms / 1000)
    }
}

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "cannot determine user home directory".to_owned())
}

#[cfg(target_os = "macos")]
fn launchd_status(label: &str) -> LaunchdStatus {
    let target = format!("gui/{}/{}", unsafe { libc::getuid() }, label);
    let Ok(output) = std::process::Command::new("launchctl")
        .args(["print", &target])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return LaunchdStatus::default();
    };
    if !output.status.success() {
        return LaunchdStatus::default();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let running = text.lines().any(|line| line.trim() == "state = running");
    let pid = text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("pid = ")
            .and_then(|value| value.parse::<u32>().ok())
    });
    LaunchdStatus {
        loaded: true,
        running,
        pid,
    }
}

#[cfg(not(target_os = "macos"))]
fn launchd_status(_label: &str) -> LaunchdStatus {
    LaunchdStatus::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn inventory_lists_default_and_named_instances_with_runtime_facts() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::temp_dir().join(format!(
            "herdr-mcp-instance-inventory-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(home.join(".config/herdr-mcp")).unwrap();
        fs::write(
            home.join(".config/herdr-mcp/config.toml"),
            b"[runtime]\nport = 8772\n",
        )
        .unwrap();
        fs::create_dir_all(home.join(".config/herdr-mcp-uat043")).unwrap();
        fs::create_dir_all(home.join(".config/herdr-mcp-dev")).unwrap();
        fs::create_dir_all(home.join("Library/LaunchAgents")).unwrap();
        fs::write(
            home.join("Library/LaunchAgents/dev.herdr-mcp.server.plist"),
            b"plist",
        )
        .unwrap();
        fs::write(
            home.join("Library/LaunchAgents/dev.herdr-mcp.release042.server.plist"),
            b"plist",
        )
        .unwrap();
        fs::write(
            home.join("Library/LaunchAgents/dev.herdr-mcp.stale.server.plist"),
            b"plist",
        )
        .unwrap();

        let inventory = discover_instances(&home, |label| match label {
            "dev.herdr-mcp.server" => LaunchdStatus {
                loaded: true,
                running: true,
                pid: Some(123),
            },
            label if label.contains("release042") => LaunchdStatus {
                loaded: true,
                running: true,
                pid: Some(456),
            },
            _ => LaunchdStatus::default(),
        });
        assert_eq!(inventory.len(), 4);
        assert_eq!(inventory[0].name, "default");
        assert_eq!(inventory[0].kind, "default");
        assert!(!inventory[0].reappable);
        assert_eq!(inventory[0].port, Some(8772));
        assert_eq!(inventory[0].state, "installed");
        assert!(inventory[0].loaded);
        assert!(inventory[0].running);
        assert_eq!(inventory[0].pid, Some(123));
        assert!(inventory[0].age_ms.is_some());
        assert_ne!(inventory[0].age, "unknown");
        assert_eq!(inventory[1].name, "release042");
        assert_eq!(inventory[1].kind, "named");
        assert!(inventory[1].reappable);
        assert_eq!(inventory[1].state, "loaded_without_config");
        assert!(inventory[1].loaded);
        assert!(inventory[1].running);
        assert_eq!(inventory[2].name, "stale");
        assert_eq!(inventory[2].state, "orphan_plist");
        assert!(!inventory[2].loaded);
        assert_eq!(inventory[3].name, "uat043");
        assert_eq!(inventory[3].state, "orphan_config");
        assert!(!inventory[3].loaded);
        assert!((8800..=8999).contains(&inventory[3].port.unwrap()));

        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn default_instance_is_inventory_only_and_never_reappable() {
        assert!(InstanceId::parse("default").is_err());
        assert_eq!(format_age(90_000), "1m");
        assert_eq!(format_age(3 * 60 * 60_000), "3h");
        assert_eq!(format_age(2 * 24 * 60 * 60_000), "2d");
    }
}
