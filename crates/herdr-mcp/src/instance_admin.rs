use crate::cli::InstanceCommand;
use crate::instance::InstanceId;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub fn run(command: InstanceCommand) -> Result<ExitCode, String> {
    match command {
        InstanceCommand::List => {
            let home = home_dir()?;
            let instances = discover_named_instances(&home, launchd_loaded);
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
            let id = InstanceId::parse(&name)?;
            if !id.is_named() {
                return Err("the default instance cannot be reaped".to_owned());
            }
            let home = home_dir()?;
            let before = discover_named_instances(&home, launchd_loaded);
            if !before.iter().any(|instance| instance.name == name) {
                return Err(format!("named instance '{name}' was not found"));
            }

            // Reuse the product-lifecycle ownership/preflight transaction. It
            // refuses path-shape-only deletion and keeps named-instance cleanup
            // separate from the default install and immutable release store.
            unsafe { std::env::set_var("HERDR_MCP_INSTANCE", &name) };
            let code = crate::product_lifecycle::uninstall()?;
            let after = discover_named_instances(&home, launchd_loaded);
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
struct NamedInstanceInventory {
    name: String,
    label: String,
    config_dir: String,
    plist: String,
    config_present: bool,
    plist_present: bool,
    loaded: bool,
    state: &'static str,
}

fn discover_named_instances(
    home: &Path,
    loaded: impl Fn(&str) -> bool,
) -> Vec<NamedInstanceInventory> {
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

    names
        .into_iter()
        .map(|name| {
            let id = InstanceId::parse(&name).expect("discovery already validated instance name");
            let label = id.service_label();
            let config_dir = config_parent.join(id.config_leaf());
            let plist = launch_agents.join(format!("{label}.plist"));
            let config_present = path_present(&config_dir);
            let plist_present = path_present(&plist);
            let state = match (config_present, plist_present) {
                (true, true) => "installed",
                (true, false) => "orphan_config",
                (false, true) => "orphan_plist",
                (false, false) => "absent",
            };
            NamedInstanceInventory {
                name,
                label: label.clone(),
                config_dir: config_dir.to_string_lossy().into_owned(),
                plist: plist.to_string_lossy().into_owned(),
                config_present,
                plist_present,
                loaded: loaded(&label),
                state,
            }
        })
        .collect()
}

fn path_present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "cannot determine user home directory".to_owned())
}

#[cfg(target_os = "macos")]
fn launchd_loaded(label: &str) -> bool {
    let target = format!("gui/{}/{}", unsafe { libc::getuid() }, label);
    std::process::Command::new("launchctl")
        .args(["print", &target])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(target_os = "macos"))]
fn launchd_loaded(_label: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn inventory_finds_named_instances_and_ignores_reserved_dev_root() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::temp_dir().join(format!(
            "herdr-mcp-instance-inventory-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(home.join(".config/herdr-mcp-uat043")).unwrap();
        fs::create_dir_all(home.join(".config/herdr-mcp-dev")).unwrap();
        fs::create_dir_all(home.join("Library/LaunchAgents")).unwrap();
        fs::write(
            home.join("Library/LaunchAgents/dev.herdr-mcp.release042.server.plist"),
            b"plist",
        )
        .unwrap();

        let inventory = discover_named_instances(&home, |label| label.contains("release042"));
        assert_eq!(inventory.len(), 2);
        assert_eq!(inventory[0].name, "release042");
        assert_eq!(inventory[0].state, "orphan_plist");
        assert!(inventory[0].loaded);
        assert_eq!(inventory[1].name, "uat043");
        assert_eq!(inventory[1].state, "orphan_config");
        assert!(!inventory[1].loaded);

        fs::remove_dir_all(home).unwrap();
    }
}
