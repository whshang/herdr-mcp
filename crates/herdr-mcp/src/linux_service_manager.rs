#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::cli::ServiceCommand;
use crate::config::Config;
use crate::paths::RuntimePaths;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

const SERVICE_UNIT: &str = "herdr-mcp.service";
const LINK_UNIT: &str = "herdr-mcp-link.service";
const HEALTH_BUDGET: Duration = Duration::from_secs(12);

#[derive(Debug, Clone)]
struct LinuxPaths {
    home: PathBuf,
    config_dir: PathBuf,
    runtime_root: PathBuf,
    generations_dir: PathBuf,
    current_link: PathBuf,
    current_binary: PathBuf,
    runtime_env: PathBuf,
    systemd_dir: PathBuf,
    service_unit: PathBuf,
    link_unit: PathBuf,
    port: u16,
    herdr_socket: PathBuf,
}

impl LinuxPaths {
    fn discover() -> Result<Self, String> {
        let runtime = RuntimePaths::discover()?;
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is required for Linux installation".to_owned())?;
        let systemd_root = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let runtime_root = runtime.config_dir.join("runtime");
        let current_link = runtime_root.join("current");
        Ok(Self {
            home: home.clone(),
            config_dir: runtime.config_dir.clone(),
            generations_dir: runtime_root.join("generations"),
            current_binary: current_link.join("herdr-mcp"),
            current_link,
            runtime_env: runtime.config_dir.join("runtime.env"),
            systemd_dir: systemd_root.join("systemd").join("user"),
            service_unit: systemd_root.join("systemd").join("user").join(SERVICE_UNIT),
            link_unit: systemd_root.join("systemd").join("user").join(LINK_UNIT),
            port: runtime.instance.default_port(),
            herdr_socket: runtime
                .herdr_socket
                .unwrap_or_else(|| home.join(".config/herdr/herdr.sock")),
            runtime_root,
        })
    }
}

pub fn run(command: ServiceCommand) -> Result<ExitCode, String> {
    match command {
        ServiceCommand::Install { adopt_node } => {
            if adopt_node {
                return Err("service install --adopt-node is only supported on macOS".to_owned());
            }
            install()?;
        }
        ServiceCommand::Status => print_json(&status_value()?)?,
        ServiceCommand::Start => {
            systemctl(&["start", SERVICE_UNIT])?;
            print_json(&status_value()?)?;
        }
        ServiceCommand::Stop => {
            systemctl(&["stop", SERVICE_UNIT])?;
            print_json(&status_value()?)?;
        }
        ServiceCommand::Restart => {
            systemctl(&["restart", SERVICE_UNIT])?;
            wait_for_health(&LinuxPaths::discover()?)?;
            print_json(&status_value()?)?;
        }
        ServiceCommand::Uninstall => uninstall()?,
        ServiceCommand::Rollback => {
            return Err("Linux service rollback is not yet exposed as a public operation; failed installs restore the prior generation transactionally".to_owned());
        }
        ServiceCommand::Guardian { .. } => {
            return Err("service guardian is a macOS launchd recovery primitive".to_owned());
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn doctor_status() -> Result<Value, String> {
    let paths = LinuxPaths::discover()?;
    let loaded = unit_active(SERVICE_UNIT);
    let healthy = health_once(paths.port);
    Ok(json!({
        "ok": loaded && healthy,
        "implementation": "rust-systemd-user",
        "loaded": loaded,
        "healthy": healthy,
        "label": SERVICE_UNIT,
        "generation": current_generation(&paths).unwrap_or_else(|| "-".to_owned()),
        "unit_path": paths.service_unit,
    }))
}

pub fn doctor_runtime_token() -> Result<Option<String>, String> {
    let paths = LinuxPaths::discover()?;
    match read_runtime_token(&paths.runtime_env) {
        Ok(token) => Ok(Some(token)),
        Err(error) if error.contains("not found") => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn ensure_link_installed() -> Result<(), String> {
    let paths = LinuxPaths::discover()?;
    if !paths.current_binary.exists() {
        return Err(
            "Linux runtime is not installed; run `herdr-mcp install` before activating the Link"
                .to_owned(),
        );
    }
    let config_path = RuntimePaths::discover()?.config_file;
    let config = Config::load(&config_path)?;
    let device_id = config
        .edge_device_id
        .as_deref()
        .ok_or_else(|| "Linux Link activation requires an enrolled edge.device_id".to_owned())?;
    let service = config.edge_link_keychain_service().ok_or_else(|| {
        "Linux Link activation cannot derive the device credential key".to_owned()
    })?;
    let account = current_account()?;
    crate::credential_store::load(&service, &account).map_err(|error| {
        format!("Linux Link activation cannot load credential for {device_id}: {error}")
    })?;
    read_runtime_token(&paths.runtime_env)?;
    ensure_secure_dir(&paths.systemd_dir, 0o700)?;
    atomic_write(
        &paths.link_unit,
        link_unit_contents(&paths)?.as_bytes(),
        0o600,
    )?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", LINK_UNIT])?;
    wait_for_unit_active(LINK_UNIT, Duration::from_secs(10)).map_err(|error| {
        let detail = journal_tail(LINK_UNIT);
        format!("{error}; recent journal: {detail}")
    })?;
    Ok(())
}

pub fn reconcile_link() -> Result<(), String> {
    let config = Config::load(&RuntimePaths::discover()?.config_file)?;
    if config.edge_device_id.is_none() {
        return Ok(());
    }
    ensure_link_installed()
}

pub fn runtime_token_for_link() -> Result<String, String> {
    read_runtime_token(&LinuxPaths::discover()?.runtime_env)
}

fn install() -> Result<(), String> {
    let paths = LinuxPaths::discover()?;
    ensure_secure_dir(&paths.config_dir, 0o700)?;
    ensure_secure_dir(&paths.runtime_root, 0o700)?;
    ensure_secure_dir(&paths.generations_dir, 0o700)?;
    ensure_secure_dir(&paths.systemd_dir, 0o700)?;

    let source = env::current_exe()
        .map_err(|error| format!("cannot resolve current herdr-mcp binary: {error}"))?;
    let sha = file_sha256(&source)?;
    let generation_id = format!("rust-{}", &sha[..16]);
    let generation_dir = paths.generations_dir.join(&generation_id);
    let generation_binary = generation_dir.join("herdr-mcp");
    if generation_binary.exists() {
        let existing = file_sha256(&generation_binary)?;
        if existing != sha {
            return Err(format!(
                "immutable Linux generation {generation_id} has unexpected content"
            ));
        }
    } else {
        ensure_secure_dir(&generation_dir, 0o700)?;
        let temp = generation_dir.join(format!(".herdr-mcp-{}", std::process::id()));
        fs::copy(&source, &temp)
            .map_err(|error| format!("cannot stage Linux runtime generation: {error}"))?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure Linux runtime generation: {error}"))?;
        let staged_sha = file_sha256(&temp)?;
        if staged_sha != sha {
            let _ = fs::remove_file(&temp);
            return Err("Linux runtime generation hash changed while copying".to_owned());
        }
        fs::rename(&temp, &generation_binary).map_err(|error| {
            format!("cannot activate immutable Linux runtime generation: {error}")
        })?;
    }

    let previous_current = fs::read_link(&paths.current_link).ok();
    let previous_unit = read_optional(&paths.service_unit)?;
    let runtime_env_existed = paths.runtime_env.exists();
    if !runtime_env_existed {
        let token = secure_token_hex()?;
        atomic_write(
            &paths.runtime_env,
            format!("HERDR_MCP_TOKEN={token}\n").as_bytes(),
            0o600,
        )?;
    } else {
        read_runtime_token(&paths.runtime_env)?;
    }

    switch_current(&paths.current_link, &generation_dir)?;
    let link_report = crate::user_cli::ensure_link(&paths.home, &paths.current_binary)?;
    atomic_write(
        &paths.service_unit,
        service_unit_contents(&paths, &generation_id)?.as_bytes(),
        0o600,
    )?;

    let activation = (|| -> Result<(), String> {
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", SERVICE_UNIT])?;
        wait_for_health(&paths)?;
        Ok(())
    })();
    if let Err(error) = activation {
        let _ = systemctl(&["disable", "--now", SERVICE_UNIT]);
        restore_optional_file(&paths.service_unit, previous_unit.as_deref(), 0o600);
        restore_current(&paths.current_link, previous_current.as_deref());
        let _ = systemctl(&["daemon-reload"]);
        if !runtime_env_existed {
            let _ = fs::remove_file(&paths.runtime_env);
        }
        return Err(format!(
            "Linux service install failed and prior service state was restored: {error}"
        ));
    }

    print_json(&json!({
        "ok": true,
        "action": "service_install",
        "implementation": "rust-systemd-user",
        "generation": generation_id,
        "runtime_current": paths.current_binary,
        "service_unit": paths.service_unit,
        "user_cli": link_report.path,
        "user_cli_changed": link_report.changed,
        "runtime_token_printed": false,
    }))
}

fn uninstall() -> Result<(), String> {
    let paths = LinuxPaths::discover()?;
    let _ = systemctl(&["disable", "--now", LINK_UNIT]);
    let _ = systemctl(&["disable", "--now", SERVICE_UNIT]);
    remove_regular_if_exists(&paths.link_unit)?;
    remove_regular_if_exists(&paths.service_unit)?;
    let _ = systemctl(&["daemon-reload"]);
    let cli_removed = crate::user_cli::remove_link_if_owned(&paths.home, &paths.current_binary)?;
    print_json(&json!({
        "ok": true,
        "action": "service_uninstall",
        "implementation": "rust-systemd-user",
        "service_unit_removed": true,
        "link_unit_removed": true,
        "user_cli_removed": cli_removed,
        "credentials_preserved": true,
        "runtime_generations_preserved": true,
    }))
}

fn status_value() -> Result<Value, String> {
    let paths = LinuxPaths::discover()?;
    Ok(json!({
        "ok": true,
        "implementation": "rust-systemd-user",
        "label": SERVICE_UNIT,
        "loaded": unit_active(SERVICE_UNIT),
        "healthy": health_once(paths.port),
        "generation": current_generation(&paths),
        "runtime_current": paths.current_binary,
        "service_unit": paths.service_unit,
        "link_loaded": unit_active(LINK_UNIT),
        "link_unit": paths.link_unit,
    }))
}

fn service_unit_contents(paths: &LinuxPaths, generation_id: &str) -> Result<String, String> {
    let current = quote(&paths.current_binary)?;
    let config_dir = quote(&paths.config_dir)?;
    let env_file = quote(&paths.runtime_env)?;
    Ok(format!(
        "[Unit]\nDescription=Herdr MCP Runtime\nAfter=network.target\n\n[Service]\nType=simple\nExecStart={current} candidate --port {}\nWorkingDirectory={config_dir}\nEnvironmentFile={env_file}\nEnvironment=\"HOME={}\"\nEnvironment=\"HERDR_MCP_HOST=127.0.0.1\"\nEnvironment=\"HERDR_MCP_PORT={}\"\nEnvironment=\"HERDR_MCP_STATE_DIR={}\"\nEnvironment=\"HERDR_MCP_CONFIG_DIR={}\"\nEnvironment=\"HERDR_MCP_CONTRACT_PROFILE=epoch2\"\nEnvironment=\"HERDR_SKILL_NETWORK=1\"\nEnvironment=\"HERDR_SOCKET_PATH={}\"\nEnvironment=\"HERDR_MCP_RUNTIME_GENERATION={}\"\nEnvironment=\"HERDR_MCP_SERVICE_IMPL=rust-systemd-user\"\nEnvironment=\"PATH={}/.local/bin:/usr/local/bin:/usr/bin:/bin\"\nRestart=always\nRestartSec=3\nUMask=0077\n\n[Install]\nWantedBy=default.target\n",
        paths.port,
        systemd_env_value(&paths.home)?,
        paths.port,
        systemd_env_value(&paths.config_dir)?,
        systemd_env_value(&paths.config_dir)?,
        systemd_env_value(&paths.herdr_socket)?,
        generation_id,
        systemd_env_value(&paths.home)?,
    ))
}

fn link_unit_contents(paths: &LinuxPaths) -> Result<String, String> {
    let current = quote(&paths.current_binary)?;
    let config_dir = quote(&paths.config_dir)?;
    Ok(format!(
        "[Unit]\nDescription=Herdr MCP Edge Link\nAfter=network-online.target {SERVICE_UNIT}\nWants=network-online.target\nRequires={SERVICE_UNIT}\n\n[Service]\nType=simple\nExecStart={current} link run\nWorkingDirectory={config_dir}\nEnvironment=\"HOME={}\"\nEnvironment=\"HERDR_MCP_CONFIG_DIR={}\"\nEnvironment=\"HERDR_MCP_STATE_DIR={}\"\nEnvironment=\"HERDR_SOCKET_PATH={}\"\nEnvironment=\"PATH={}/.local/bin:/usr/local/bin:/usr/bin:/bin\"\nRestart=always\nRestartSec=5\nUMask=0077\n\n[Install]\nWantedBy=default.target\n",
        systemd_env_value(&paths.home)?,
        systemd_env_value(&paths.config_dir)?,
        systemd_env_value(&paths.config_dir)?,
        systemd_env_value(&paths.herdr_socket)?,
        systemd_env_value(&paths.home)?,
    ))
}

fn systemd_env_value(path: &Path) -> Result<String, String> {
    let raw = path
        .to_str()
        .ok_or_else(|| format!("systemd path is not UTF-8: {}", path.display()))?;
    if raw.contains(['\n', '\r', '\0', '"']) {
        return Err(format!(
            "systemd path contains unsupported characters: {}",
            path.display()
        ));
    }
    Ok(raw.replace('\\', "\\\\"))
}

fn quote(path: &Path) -> Result<String, String> {
    Ok(format!("\"{}\"", systemd_env_value(path)?))
}

fn current_account() -> Result<String, String> {
    if let Ok(user) = env::var("USER")
        && !user.trim().is_empty()
        && !user.chars().any(char::is_control)
    {
        return Ok(user);
    }
    let output = Command::new("id")
        .arg("-un")
        .output()
        .map_err(|error| format!("cannot resolve current Linux account: {error}"))?;
    if !output.status.success() {
        return Err("cannot resolve current Linux account with `id -un`".to_owned());
    }
    let user = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if user.is_empty() || user.chars().any(char::is_control) {
        return Err("current Linux account is invalid".to_owned());
    }
    Ok(user)
}

fn read_runtime_token(path: &Path) -> Result<String, String> {
    let meta = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "Linux runtime token file not found or unreadable {}: {error}",
            path.display()
        )
    })?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err("Linux runtime token file must be a regular non-symlink file".to_owned());
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(
            "Linux runtime token file must not be accessible by group or other users".to_owned(),
        );
    }
    if meta.len() > 8192 {
        return Err("Linux runtime token file is unexpectedly large".to_owned());
    }
    let content = fs::read_to_string(path).map_err(|error| {
        format!(
            "cannot read Linux runtime token file {}: {error}",
            path.display()
        )
    })?;
    let token = content
        .lines()
        .find_map(|line| line.strip_prefix("HERDR_MCP_TOKEN="))
        .filter(|value| {
            !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| "Linux runtime token file is missing HERDR_MCP_TOKEN".to_owned())?;
    Ok(token.to_owned())
}

fn wait_for_health(paths: &LinuxPaths) -> Result<(), String> {
    let deadline = Instant::now() + HEALTH_BUDGET;
    while Instant::now() < deadline {
        if health_once(paths.port) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "{SERVICE_UNIT} did not become healthy on 127.0.0.1:{}; recent journal: {}",
        paths.port,
        journal_tail(SERVICE_UNIT)
    ))
}

fn health_once(port: u16) -> bool {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .and_then(|client| client.get(format!("http://127.0.0.1:{port}/health")).send())
        .is_ok_and(|response| response.status().is_success())
}

fn wait_for_unit_active(unit: &str, budget: Duration) -> Result<(), String> {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if unit_active(unit) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(format!("systemd user unit {unit} did not become active"))
}

fn unit_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", unit])
        .status()
        .is_ok_and(|status| status.success())
}

fn systemctl(args: &[&str]) -> Result<(), String> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|error| format!("cannot execute `systemctl --user`: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(format!(
        "`systemctl --user {}` failed: {}. A Debian user systemd manager is required; run from a normal login/SSH session for this user (no root required)",
        args.join(" "),
        if stderr.is_empty() {
            "unknown systemd error"
        } else {
            &stderr
        }
    ))
}

fn journal_tail(unit: &str) -> String {
    Command::new("journalctl")
        .args(["--user", "-u", unit, "-n", "8", "--no-pager", "-o", "cat"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).replace('\n', " | "))
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn switch_current(link: &Path, target: &Path) -> Result<(), String> {
    if let Ok(meta) = fs::symlink_metadata(link)
        && !meta.file_type().is_symlink()
    {
        return Err(format!(
            "refusing to replace non-symlink runtime/current at {}",
            link.display()
        ));
    }
    let parent = link
        .parent()
        .ok_or_else(|| "runtime/current has no parent".to_owned())?;
    let temp = parent.join(format!(".current-{}", std::process::id()));
    let _ = fs::remove_file(&temp);
    symlink(target, &temp)
        .map_err(|error| format!("cannot stage Linux runtime/current symlink: {error}"))?;
    fs::rename(&temp, link).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("cannot switch Linux runtime/current: {error}")
    })
}

fn restore_current(link: &Path, target: Option<&Path>) {
    match target {
        Some(target) => {
            let _ = switch_current(link, target);
        }
        None => {
            let _ = fs::remove_file(link);
        }
    }
}

fn current_generation(paths: &LinuxPaths) -> Option<String> {
    fs::read_link(&paths.current_link).ok().and_then(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    })
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let meta = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!("{} must be a regular file", path.display()));
    }
    let mut file =
        File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|error| format!("cannot hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hash.update(&buf[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn secure_token_hex() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| format!("cannot read secure random token: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn ensure_secure_dir(path: &Path, mode: u32) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    let meta = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(format!("{} must be a real directory", path.display()));
    }
    if meta.permissions().mode() & 0o777 != mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|error| format!("cannot set mode on {}: {error}", path.display()))?;
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    if let Ok(meta) = fs::symlink_metadata(path)
        && (meta.file_type().is_symlink() || !meta.is_file())
    {
        return Err(format!(
            "refusing to replace non-regular file {}",
            path.display()
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let temp = parent.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(mode);
    let mut file = options
        .open(&temp)
        .map_err(|error| format!("cannot create {}: {error}", temp.display()))?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(format!("cannot persist {}: {error}", temp.display()));
    }
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("cannot replace {}: {error}", path.display())
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot secure {}: {error}", path.display()))
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

fn restore_optional_file(path: &Path, bytes: Option<&[u8]>, mode: u32) {
    match bytes {
        Some(bytes) => {
            let _ = atomic_write(path, bytes, mode);
        }
        None => {
            let _ = fs::remove_file(path);
        }
    }
}

fn remove_regular_if_exists(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => Err(format!(
            "refusing to remove non-regular file {}",
            path.display()
        )),
        Ok(_) => fs::remove_file(path)
            .map_err(|error| format!("cannot remove {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
    }
}

fn print_json(value: &Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|error| format!("cannot encode Linux service result: {error}"))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_units_never_inline_runtime_or_device_secrets() {
        let home = PathBuf::from("/home/tester");
        let paths = LinuxPaths {
            home: home.clone(),
            config_dir: home.join(".config/herdr-mcp"),
            runtime_root: home.join(".config/herdr-mcp/runtime"),
            generations_dir: home.join(".config/herdr-mcp/runtime/generations"),
            current_link: home.join(".config/herdr-mcp/runtime/current"),
            current_binary: home.join(".config/herdr-mcp/runtime/current/herdr-mcp"),
            runtime_env: home.join(".config/herdr-mcp/runtime.env"),
            systemd_dir: home.join(".config/systemd/user"),
            service_unit: home.join(".config/systemd/user/herdr-mcp.service"),
            link_unit: home.join(".config/systemd/user/herdr-mcp-link.service"),
            port: 8772,
            herdr_socket: home.join(".config/herdr/herdr.sock"),
        };
        let server = service_unit_contents(&paths, "rust-deadbeef").unwrap();
        let link = link_unit_contents(&paths).unwrap();
        assert!(server.contains("EnvironmentFile=\"/home/tester/.config/herdr-mcp/runtime.env\""));
        assert!(!server.contains("HERDR_MCP_TOKEN="));
        assert!(!link.contains("devsec_"));
        assert!(link.contains(" link run"));
        assert!(server.contains(" candidate --port 8772"));
    }

    #[test]
    fn systemd_path_rejects_newline_and_quote() {
        assert!(systemd_env_value(Path::new("/home/good/.config")).is_ok());
        assert!(systemd_env_value(Path::new("/home/bad\npath")).is_err());
        assert!(systemd_env_value(Path::new("/home/\"bad\"")).is_err());
    }
}
