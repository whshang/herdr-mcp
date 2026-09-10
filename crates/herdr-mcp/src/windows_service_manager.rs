use crate::cli::ServiceCommand;
use crate::config::Config;
use crate::paths::RuntimePaths;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

const IMPLEMENTATION: &str = "rust-windows-task-user";
const RUNTIME_TOKEN_SERVICE: &str = "herdr-mcp-local-runtime";
const HEALTH_BUDGET: Duration = Duration::from_secs(12);

#[derive(Debug, Clone)]
struct WindowsPaths {
    config_dir: PathBuf,
    generations_dir: PathBuf,
    current_dir: PathBuf,
    current_binary: PathBuf,
    current_generation: PathBuf,
    port: u16,
    herdr_socket: PathBuf,
    runtime_task: String,
    link_task: String,
}

impl WindowsPaths {
    fn discover() -> Result<Self, String> {
        let runtime = RuntimePaths::discover()?;
        let runtime_root = runtime.config_dir.join("runtime");
        let current_dir = runtime_root.join("current");
        let suffix = runtime
            .instance
            .name()
            .map(|name| format!(" - {name}"))
            .unwrap_or_default();
        Ok(Self {
            config_dir: runtime.config_dir.clone(),
            generations_dir: runtime_root.join("generations"),
            current_binary: current_dir.join("herdr-mcp.exe"),
            current_generation: current_dir.join("generation"),
            current_dir,
            port: runtime.instance.default_port(),
            herdr_socket: runtime
                .herdr_socket
                .ok_or_else(|| "Windows Herdr named-pipe path is unavailable".to_owned())?,
            runtime_task: format!("Herdr MCP Runtime{suffix}"),
            link_task: format!("Herdr MCP Link{suffix}"),
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
            let paths = WindowsPaths::discover()?;
            require_task(&paths.runtime_task)?;
            task_run(&paths.runtime_task)?;
            wait_for_health(&paths)?;
            if task_exists(&paths.link_task) {
                task_run(&paths.link_task)?;
            }
            print_json(&status_value()?)?;
        }
        ServiceCommand::Stop => {
            let paths = WindowsPaths::discover()?;
            task_end_if_present(&paths.link_task)?;
            task_end_if_present(&paths.runtime_task)?;
            print_json(&status_value()?)?;
        }
        ServiceCommand::Restart => {
            let paths = WindowsPaths::discover()?;
            require_task(&paths.runtime_task)?;
            task_end_if_present(&paths.link_task)?;
            task_end_if_present(&paths.runtime_task)?;
            task_run(&paths.runtime_task)?;
            wait_for_health(&paths)?;
            if task_exists(&paths.link_task) {
                task_run(&paths.link_task)?;
            }
            print_json(&status_value()?)?;
        }
        ServiceCommand::Uninstall => uninstall()?,
        ServiceCommand::Rollback => {
            return Err(
                "Windows service rollback is not yet exposed as a public operation; failed installs restore the previous active generation transactionally"
                    .to_owned(),
            );
        }
        ServiceCommand::Guardian { .. } => {
            return Err("service guardian is a macOS launchd recovery primitive".to_owned());
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn doctor_status() -> Result<Value, String> {
    let paths = WindowsPaths::discover()?;
    let loaded = task_exists(&paths.runtime_task);
    let healthy = health_once(paths.port);
    let link_loaded = task_exists(&paths.link_task);
    let generation = current_generation(&paths);
    Ok(json!({
        "ok": loaded && healthy,
        "implementation": IMPLEMENTATION,
        "loaded": loaded,
        "healthy": healthy,
        "label": paths.runtime_task,
        "generation": generation,
        "current_target": generation,
        "runtime_current": paths.current_binary,
        "link_loaded": link_loaded,
        "link_label": paths.link_task,
    }))
}

pub fn doctor_runtime_token() -> Result<Option<String>, String> {
    match runtime_token() {
        Ok(token) => Ok(Some(token)),
        Err(error) if error.contains("cannot load Windows device credential") => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn runtime_token_for_link() -> Result<String, String> {
    runtime_token()
}

pub fn prepare_candidate_environment(port: u16) -> Result<(), String> {
    if env::var_os("HERDR_MCP_TOKEN").is_some() {
        return Ok(());
    }
    let paths = WindowsPaths::discover()?;
    let token = runtime_token()?;
    let generation = current_generation(&paths)
        .ok_or_else(|| "Windows runtime/current generation marker is missing".to_owned())?;
    unsafe {
        env::set_var("HERDR_MCP_TOKEN", token);
        env::set_var("HERDR_MCP_HOST", "127.0.0.1");
        env::set_var("HERDR_MCP_PORT", port.to_string());
        env::set_var("HERDR_MCP_CONFIG_DIR", &paths.config_dir);
        env::set_var("HERDR_MCP_STATE_DIR", &paths.config_dir);
        env::set_var("HERDR_MCP_CONTRACT_PROFILE", "epoch2");
        env::set_var("HERDR_SKILL_NETWORK", "1");
        env::set_var("HERDR_SOCKET_PATH", &paths.herdr_socket);
        env::set_var("HERDR_MCP_RUNTIME_GENERATION", generation);
        env::set_var("HERDR_MCP_SERVICE_IMPL", IMPLEMENTATION);
    }
    Ok(())
}

pub fn install_link() -> Result<(), String> {
    let runtime = RuntimePaths::discover()?;
    let config = Config::load_for_instance(&runtime.config_file, &runtime.instance)?;
    if config.edge_device_id.is_none() {
        return Err(
            "Windows Link install requires an enrolled edge.device_id; run `herdr-mcp worker connect` first"
                .to_owned(),
        );
    }
    reconcile_link()
}

pub fn uninstall_link() -> Result<(), String> {
    let paths = WindowsPaths::discover()?;
    task_end_if_present(&paths.link_task)?;
    task_delete_if_present(&paths.link_task)
}

pub fn print_link_status() -> Result<ExitCode, String> {
    let paths = WindowsPaths::discover()?;
    print_json(&json!({
        "ok": true,
        "implementation": IMPLEMENTATION,
        "loaded": task_exists(&paths.link_task),
        "label": paths.link_task,
        "generation": current_generation(&paths),
    }))?;
    Ok(ExitCode::SUCCESS)
}

pub fn reconcile_link() -> Result<(), String> {
    let paths = WindowsPaths::discover()?;
    let runtime = RuntimePaths::discover()?;
    let config = Config::load_for_instance(&runtime.config_file, &runtime.instance)?;
    if config.edge_device_id.is_none() {
        return Ok(());
    }
    if !paths.current_binary.exists() {
        return Err(
            "Windows runtime is not installed; run `herdr-mcp install` before activating the Link"
                .to_owned(),
        );
    }
    let service = config.edge_link_keychain_service().ok_or_else(|| {
        "Windows Link activation cannot derive the device credential key".to_owned()
    })?;
    let account = current_account()?;
    crate::credential_store::load(&service, &account).map_err(|error| {
        format!("Windows Link activation cannot load enrolled device credential: {error}")
    })?;
    runtime_token()?;
    let command = task_command(&paths.current_binary, &["link", "run"])?;
    task_end_if_present(&paths.link_task)?;
    task_create(&paths.link_task, &command)?;
    task_run(&paths.link_task)
}

fn install() -> Result<(), String> {
    let paths = WindowsPaths::discover()?;
    fs::create_dir_all(&paths.generations_dir)
        .map_err(|error| format!("cannot create Windows generations directory: {error}"))?;
    fs::create_dir_all(&paths.current_dir)
        .map_err(|error| format!("cannot create Windows runtime/current directory: {error}"))?;

    let source = env::current_exe()
        .map_err(|error| format!("cannot resolve current herdr-mcp binary: {error}"))?;
    let sha = file_sha256(&source)?;
    let generation_id = format!("rust-{}", &sha[..16]);
    let generation_dir = paths.generations_dir.join(&generation_id);
    let generation_binary = generation_dir.join("herdr-mcp.exe");
    if generation_binary.exists() {
        if file_sha256(&generation_binary)? != sha {
            return Err(format!(
                "immutable Windows generation {generation_id} has unexpected content"
            ));
        }
    } else {
        fs::create_dir_all(&generation_dir)
            .map_err(|error| format!("cannot create Windows generation: {error}"))?;
        copy_verified(&source, &generation_binary, &sha)?;
    }

    ensure_runtime_token()?;
    let previous_generation = current_generation(&paths);

    task_end_if_present(&paths.link_task)?;
    task_end_if_present(&paths.runtime_task)?;
    activate_generation(&paths, &generation_binary, &generation_id, &sha)?;

    let service_command = task_command(
        &paths.current_binary,
        &["candidate", "--port", &paths.port.to_string()],
    )?;
    let activation = (|| -> Result<(), String> {
        task_create(&paths.runtime_task, &service_command)?;
        task_run(&paths.runtime_task)?;
        wait_for_health(&paths)?;
        reconcile_link()?;
        Ok(())
    })();
    if let Err(error) = activation {
        task_end_if_present(&paths.link_task).ok();
        task_end_if_present(&paths.runtime_task).ok();
        restore_generation(&paths, previous_generation.as_deref())?;
        if previous_generation.is_some() {
            task_create(
                &paths.runtime_task,
                &task_command(
                    &paths.current_binary,
                    &["candidate", "--port", &paths.port.to_string()],
                )?,
            )?;
            task_run(&paths.runtime_task)?;
            wait_for_health(&paths)?;
            reconcile_link()?;
        } else {
            task_delete_if_present(&paths.runtime_task)?;
            task_delete_if_present(&paths.link_task)?;
        }
        return Err(format!(
            "Windows service install failed and the previous active generation was restored: {error}"
        ));
    }

    print_json(&json!({
        "ok": true,
        "action": "service_install",
        "implementation": IMPLEMENTATION,
        "generation": generation_id,
        "runtime_current": paths.current_binary,
        "runtime_task": paths.runtime_task,
        "link_reconciled": task_exists(&paths.link_task),
        "runtime_token_printed": false,
    }))
}

fn uninstall() -> Result<(), String> {
    let paths = WindowsPaths::discover()?;
    task_end_if_present(&paths.link_task)?;
    task_end_if_present(&paths.runtime_task)?;
    task_delete_if_present(&paths.link_task)?;
    task_delete_if_present(&paths.runtime_task)?;
    print_json(&json!({
        "ok": true,
        "action": "service_uninstall",
        "implementation": IMPLEMENTATION,
        "runtime_task_removed": true,
        "link_task_removed": true,
        "credentials_preserved": true,
        "runtime_generations_preserved": true,
    }))
}

fn status_value() -> Result<Value, String> {
    let paths = WindowsPaths::discover()?;
    Ok(json!({
        "ok": true,
        "implementation": IMPLEMENTATION,
        "label": paths.runtime_task,
        "loaded": task_exists(&paths.runtime_task),
        "healthy": health_once(paths.port),
        "generation": current_generation(&paths),
        "current_target": current_generation(&paths),
        "runtime_current": paths.current_binary,
        "link_loaded": task_exists(&paths.link_task),
        "link_label": paths.link_task,
    }))
}

fn activate_generation(
    paths: &WindowsPaths,
    generation_binary: &Path,
    generation_id: &str,
    expected_sha: &str,
) -> Result<(), String> {
    let temp_binary = paths
        .current_dir
        .join(format!(".herdr-mcp-{}.exe", std::process::id()));
    let _ = fs::remove_file(&temp_binary);
    copy_verified(generation_binary, &temp_binary, expected_sha)?;
    if paths.current_binary.exists() {
        fs::remove_file(&paths.current_binary).map_err(|error| {
            format!("cannot replace stopped Windows runtime/current binary: {error}")
        })?;
    }
    fs::rename(&temp_binary, &paths.current_binary).map_err(|error| {
        let _ = fs::remove_file(&temp_binary);
        format!("cannot activate Windows runtime/current binary: {error}")
    })?;
    write_generation_marker(&paths.current_generation, generation_id)
}

fn restore_generation(paths: &WindowsPaths, generation: Option<&str>) -> Result<(), String> {
    let Some(generation) = generation else {
        let _ = fs::remove_file(&paths.current_binary);
        let _ = fs::remove_file(&paths.current_generation);
        return Ok(());
    };
    if !valid_generation_id(generation) {
        return Err("cannot restore invalid Windows generation id".to_owned());
    }
    let source = paths.generations_dir.join(generation).join("herdr-mcp.exe");
    let sha = file_sha256(&source)?;
    activate_generation(paths, &source, generation, &sha)
}

fn current_generation(paths: &WindowsPaths) -> Option<String> {
    fs::read_to_string(&paths.current_generation)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| valid_generation_id(value))
}

fn write_generation_marker(path: &Path, generation: &str) -> Result<(), String> {
    if !valid_generation_id(generation) {
        return Err("invalid Windows generation id".to_owned());
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temp, format!("{generation}\n"))
        .map_err(|error| format!("cannot stage Windows generation marker: {error}"))?;
    if path.exists() {
        fs::remove_file(path)
            .map_err(|error| format!("cannot replace Windows generation marker: {error}"))?;
    }
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("cannot activate Windows generation marker: {error}")
    })
}

fn valid_generation_id(value: &str) -> bool {
    value.starts_with("rust-")
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn ensure_runtime_token() -> Result<(), String> {
    if runtime_token().is_ok() {
        return Ok(());
    }
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("cannot generate Windows runtime token: {error}"))?;
    let token = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let account = current_account()?;
    crate::credential_store::store(RUNTIME_TOKEN_SERVICE, &account, &token)
}

fn runtime_token() -> Result<String, String> {
    let account = current_account()?;
    crate::credential_store::load(RUNTIME_TOKEN_SERVICE, &account)
}

fn current_account() -> Result<String, String> {
    env::var("USERNAME")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| {
            !value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| "USERNAME is required for Windows service management".to_owned())
}

fn task_command(binary: &Path, args: &[&str]) -> Result<String, String> {
    let binary = binary
        .to_str()
        .ok_or_else(|| "Windows runtime path is not valid UTF-8".to_owned())?;
    if binary.contains('"') {
        return Err("Windows runtime path contains an unsupported quote".to_owned());
    }
    let mut command = format!("\"{binary}\"");
    for arg in args {
        if arg.is_empty()
            || arg
                .bytes()
                .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
        {
            return Err(format!("unsupported Windows task argument: {arg}"));
        }
        command.push(' ');
        command.push_str(arg);
    }
    Ok(command)
}

fn task_create(name: &str, command: &str) -> Result<(), String> {
    schtasks(&[
        "/Create", "/SC", "ONLOGON", "/TN", name, "/TR", command, "/F",
    ])
}

fn task_run(name: &str) -> Result<(), String> {
    schtasks(&["/Run", "/TN", name])
}

fn task_exists(name: &str) -> bool {
    Command::new("schtasks.exe")
        .args(["/Query", "/TN", name])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn task_end_if_present(name: &str) -> Result<(), String> {
    if !task_exists(name) {
        return Ok(());
    }
    let _ = Command::new("schtasks.exe")
        .args(["/End", "/TN", name])
        .output();
    Ok(())
}

fn task_delete_if_present(name: &str) -> Result<(), String> {
    if task_exists(name) {
        schtasks(&["/Delete", "/TN", name, "/F"])?;
    }
    Ok(())
}

fn require_task(name: &str) -> Result<(), String> {
    if task_exists(name) {
        Ok(())
    } else {
        Err(format!(
            "Windows scheduled task '{name}' is not installed; run `herdr-mcp install`"
        ))
    }
}

fn schtasks(args: &[&str]) -> Result<(), String> {
    let output = Command::new("schtasks.exe")
        .args(args)
        .output()
        .map_err(|error| format!("cannot execute schtasks.exe: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    Err(format!(
        "schtasks.exe {} failed{}",
        args.first().copied().unwrap_or("operation"),
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    ))
}

fn wait_for_health(paths: &WindowsPaths) -> Result<(), String> {
    let deadline = Instant::now() + HEALTH_BUDGET;
    while Instant::now() < deadline {
        if health_once(paths.port) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "Windows Herdr MCP runtime did not become healthy on 127.0.0.1:{}",
        paths.port
    ))
}

fn health_once(port: u16) -> bool {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .and_then(|client| client.get(format!("http://127.0.0.1:{port}/health")).send())
        .is_ok_and(|response| response.status().is_success())
}

fn copy_verified(source: &Path, target: &Path, expected_sha: &str) -> Result<(), String> {
    if target.exists() {
        fs::remove_file(target)
            .map_err(|error| format!("cannot replace {}: {error}", target.display()))?;
    }
    fs::copy(source, target).map_err(|error| {
        format!(
            "cannot copy Windows runtime {} -> {}: {error}",
            source.display(),
            target.display()
        )
    })?;
    if file_sha256(target)? != expected_sha {
        let _ = fs::remove_file(target);
        return Err("Windows runtime copy failed SHA-256 verification".to_owned());
    }
    Ok(())
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
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash {}: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn print_json(value: &Value) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)
        .map_err(|error| format!("cannot encode Windows service result: {error}"))?;
    stdout
        .write_all(b"\n")
        .map_err(|error| format!("cannot write Windows service result: {error}"))
}
