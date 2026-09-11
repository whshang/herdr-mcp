use crate::cli::ServiceCommand;
use crate::config::Config;
use crate::paths::RuntimePaths;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER, ERROR_SUCCESS, FILETIME,
    GetLastError, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, GetProcessTimes, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
    WaitForSingleObject,
};

const IMPLEMENTATION: &str = "rust-windows-process-user";
const RUNTIME_TOKEN_SERVICE: &str = "herdr-mcp-local-runtime";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const PROCESS_RECORD_SCHEMA: u32 = 1;
const PROCESS_RECORD_MAX_BYTES: u64 = 16 * 1024;
const STARTUP_LOG_TAIL_BYTES: u64 = 8 * 1024;
const HEALTH_BUDGET: Duration = Duration::from_secs(12);
const PROCESS_STOP_BUDGET_MS: u32 = 5_000;
const RUNTIME_KIND: &str = "runtime";
const LINK_KIND: &str = "link";

#[derive(Debug, Clone)]
struct WindowsPaths {
    config_dir: PathBuf,
    generations_dir: PathBuf,
    current_dir: PathBuf,
    current_binary: PathBuf,
    current_generation: PathBuf,
    runtime_process: PathBuf,
    link_process: PathBuf,
    runtime_log: PathBuf,
    link_log: PathBuf,
    link_enabled: PathBuf,
    startup_shortcut: PathBuf,
    port: u16,
    herdr_socket: PathBuf,
    run_value_name: String,
    link_label: String,
    instance_name: Option<String>,
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
        let app_data = env::var_os("APPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| "APPDATA is required for Windows user autostart".to_owned())?;
        let startup_shortcut = app_data
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
            .join(format!("Herdr MCP Runtime{suffix}.lnk"));
        Ok(Self {
            config_dir: runtime.config_dir.clone(),
            generations_dir: runtime_root.join("generations"),
            current_binary: current_dir.join("herdr-mcp.exe"),
            current_generation: current_dir.join("generation"),
            runtime_process: runtime_root.join("windows-runtime-process.json"),
            link_process: runtime_root.join("windows-link-process.json"),
            runtime_log: runtime_root.join("windows-runtime-startup.log"),
            link_log: runtime_root.join("windows-link-startup.log"),
            link_enabled: runtime_root.join("windows-link-enabled"),
            startup_shortcut,
            current_dir,
            port: runtime.instance.default_port(),
            herdr_socket: runtime
                .herdr_socket
                .ok_or_else(|| "Windows Herdr named-pipe path is unavailable".to_owned())?,
            run_value_name: format!("Herdr MCP Runtime{suffix}"),
            link_label: format!("Herdr MCP Link{suffix}"),
            instance_name: runtime.instance.name().map(str::to_owned),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ManagedProcessRecord {
    schema_version: u32,
    kind: String,
    pid: u32,
    creation_time_100ns: u64,
    generation: String,
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

struct OwnedRegistryKey(HKEY);

impl Drop for OwnedRegistryKey {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                RegCloseKey(self.0);
            }
        }
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
            require_autostart(&paths)?;
            start_runtime(&paths)?;
            start_enabled_link(&paths)?;
            print_json(&status_value()?)?;
        }
        ServiceCommand::Stop => {
            let paths = WindowsPaths::discover()?;
            stop_managed_process(&paths.link_process, LINK_KIND)?;
            stop_managed_process(&paths.runtime_process, RUNTIME_KIND)?;
            print_json(&status_value()?)?;
        }
        ServiceCommand::Restart => {
            let paths = WindowsPaths::discover()?;
            require_autostart(&paths)?;
            stop_managed_process(&paths.link_process, LINK_KIND)?;
            stop_managed_process(&paths.runtime_process, RUNTIME_KIND)?;
            start_runtime(&paths)?;
            start_enabled_link(&paths)?;
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
    let autostart_registered = autostart_registered(&paths)?;
    let loaded = managed_process_active(&paths.runtime_process, RUNTIME_KIND)?;
    let healthy = health_once(paths.port);
    let link_enabled = link_enabled(&paths)?;
    let link_loaded = managed_process_active(&paths.link_process, LINK_KIND)?;
    let generation = current_generation(&paths);
    let link_healthy = !link_enabled || link_loaded;
    Ok(json!({
        "ok": autostart_registered && loaded && healthy && link_healthy,
        "implementation": IMPLEMENTATION,
        "loaded": loaded,
        "healthy": healthy,
        "label": paths.run_value_name,
        "autostart_registered": autostart_registered,
        "autostart_method": "startup-folder-shortcut",
        "autostart_path": paths.startup_shortcut,
        "legacy_run_registered": legacy_run_registered(&paths)?,
        "generation": generation,
        "current_target": generation,
        "runtime_current": paths.current_binary,
        "link_enabled": link_enabled,
        "link_loaded": link_loaded,
        "link_label": paths.link_label,
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
    stop_managed_process(&paths.link_process, LINK_KIND)?;
    set_link_enabled(&paths, false)
}

pub fn print_link_status() -> Result<ExitCode, String> {
    let paths = WindowsPaths::discover()?;
    let enabled = link_enabled(&paths)?;
    let loaded = managed_process_active(&paths.link_process, LINK_KIND)?;
    print_json(&json!({
        "ok": !enabled || loaded,
        "implementation": IMPLEMENTATION,
        "loaded": loaded,
        "enabled": enabled,
        "label": paths.link_label,
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

    let was_enabled = link_enabled(&paths)?;
    set_link_enabled(&paths, true)?;
    if let Err(error) = start_link(&paths) {
        if !was_enabled {
            set_link_enabled(&paths, false).ok();
        }
        return Err(error);
    }
    Ok(())
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
    let previous_startup_shortcut = startup_shortcut_owned(&paths)?;
    let previous_run_value = read_run_value(&paths.run_value_name)?;
    let expected_run_value = autostart_command(&paths)?;
    if let Some(existing) = previous_run_value.as_deref()
        && existing != expected_run_value
    {
        return Err(format!(
            "legacy HKCU Run value '{}' exists with an unexpected command; refusing to overwrite it",
            paths.run_value_name
        ));
    }
    let previous_link_enabled = link_enabled(&paths)?;

    stop_managed_process(&paths.link_process, LINK_KIND)?;
    stop_managed_process(&paths.runtime_process, RUNTIME_KIND)?;
    activate_generation(&paths, &generation_binary, &generation_id, &sha)?;

    let activation = (|| -> Result<(), String> {
        ensure_startup_shortcut(&paths)?;
        if previous_run_value.is_some() {
            delete_run_value(&paths.run_value_name)?;
        }
        start_runtime(&paths)?;
        reconcile_link()?;
        Ok(())
    })();
    if let Err(error) = activation {
        stop_managed_process(&paths.link_process, LINK_KIND).ok();
        stop_managed_process(&paths.runtime_process, RUNTIME_KIND).ok();
        restore_generation(&paths, previous_generation.as_deref())?;
        restore_startup_shortcut(&paths, previous_startup_shortcut)?;
        restore_run_value(&paths.run_value_name, previous_run_value.as_deref())?;
        set_link_enabled(&paths, previous_link_enabled)?;
        if previous_generation.is_some() {
            start_runtime(&paths)?;
            start_enabled_link(&paths)?;
        } else {
            remove_process_record(&paths.runtime_process).ok();
            remove_process_record(&paths.link_process).ok();
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
        "autostart": paths.startup_shortcut,
        "autostart_method": "startup-folder-shortcut",
        "legacy_run_removed": previous_run_value.is_some(),
        "link_reconciled": managed_process_active(&paths.link_process, LINK_KIND)?,
        "runtime_token_printed": false,
    }))
}

fn uninstall() -> Result<(), String> {
    let paths = WindowsPaths::discover()?;
    stop_managed_process(&paths.link_process, LINK_KIND)?;
    stop_managed_process(&paths.runtime_process, RUNTIME_KIND)?;
    remove_autostart_if_owned(&paths)?;
    set_link_enabled(&paths, false)?;
    print_json(&json!({
        "ok": true,
        "action": "service_uninstall",
        "implementation": IMPLEMENTATION,
        "autostart_removed": true,
        "link_disabled": true,
        "credentials_preserved": true,
        "runtime_generations_preserved": true,
    }))
}

fn status_value() -> Result<Value, String> {
    let paths = WindowsPaths::discover()?;
    let autostart_registered = autostart_registered(&paths)?;
    let loaded = managed_process_active(&paths.runtime_process, RUNTIME_KIND)?;
    let healthy = health_once(paths.port);
    let link_enabled = link_enabled(&paths)?;
    let link_loaded = managed_process_active(&paths.link_process, LINK_KIND)?;
    Ok(json!({
        "ok": autostart_registered && loaded && healthy && (!link_enabled || link_loaded),
        "implementation": IMPLEMENTATION,
        "label": paths.run_value_name,
        "autostart_registered": autostart_registered,
        "autostart_method": "startup-folder-shortcut",
        "autostart_path": paths.startup_shortcut,
        "legacy_run_registered": legacy_run_registered(&paths)?,
        "loaded": loaded,
        "healthy": healthy,
        "generation": current_generation(&paths),
        "current_target": current_generation(&paths),
        "runtime_current": paths.current_binary,
        "link_enabled": link_enabled,
        "link_loaded": link_loaded,
        "link_label": paths.link_label,
    }))
}

fn start_runtime(paths: &WindowsPaths) -> Result<(), String> {
    let port = paths.port.to_string();
    let spawned = start_managed_process(
        paths,
        RUNTIME_KIND,
        &["candidate", "--port", &port],
        &paths.runtime_process,
        &paths.runtime_log,
    )?;
    if let Err(error) = wait_for_health(paths) {
        if spawned {
            stop_managed_process(&paths.runtime_process, RUNTIME_KIND).ok();
        }
        return Err(error);
    }
    Ok(())
}

fn start_link(paths: &WindowsPaths) -> Result<(), String> {
    if !health_once(paths.port) {
        return Err("Windows Link activation requires a healthy local runtime".to_owned());
    }
    start_managed_process(
        paths,
        LINK_KIND,
        &["link", "run"],
        &paths.link_process,
        &paths.link_log,
    )?;
    thread::sleep(Duration::from_millis(250));
    if managed_process_active(&paths.link_process, LINK_KIND)? {
        Ok(())
    } else {
        Err("Windows Link process exited during startup".to_owned())
    }
}

fn start_enabled_link(paths: &WindowsPaths) -> Result<(), String> {
    if link_enabled(paths)? {
        start_link(paths)?;
    }
    Ok(())
}

fn start_managed_process(
    paths: &WindowsPaths,
    kind: &str,
    args: &[&str],
    record_path: &Path,
    log_path: &Path,
) -> Result<bool, String> {
    if let Some(record) = read_process_record(record_path, kind)? {
        if process_record_active(&record)? {
            return Ok(false);
        }
        remove_process_record(record_path)?;
    }
    let generation = current_generation(paths)
        .ok_or_else(|| "Windows runtime/current generation marker is missing".to_owned())?;
    if !paths.current_binary.is_file() {
        return Err("Windows runtime/current binary is missing".to_owned());
    }

    let log = open_startup_log(log_path, kind)?;
    let stderr = log
        .try_clone()
        .map_err(|error| format!("cannot clone Windows {kind} startup log handle: {error}"))?;
    let mut command = Command::new(&paths.current_binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .env_remove("CLOUDFLARE_ACCOUNT_ID")
        .env_remove("HERDR_MCP_TOKEN");
    if let Some(instance) = paths.instance_name.as_deref() {
        command.env("HERDR_MCP_INSTANCE", instance);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start Windows {kind} process: {error}"))?;
    let creation_time_100ns = process_creation_time_from_handle(child.as_raw_handle() as HANDLE)
        .map_err(|error| {
            let _ = child.kill();
            format!("cannot identify Windows {kind} process: {error}")
        })?;
    let record = ManagedProcessRecord {
        schema_version: PROCESS_RECORD_SCHEMA,
        kind: kind.to_owned(),
        pid: child.id(),
        creation_time_100ns,
        generation,
    };
    if let Err(error) = write_process_record(record_path, &record) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    drop(child);
    Ok(true)
}

fn managed_process_active(path: &Path, kind: &str) -> Result<bool, String> {
    let Some(record) = read_process_record(path, kind)? else {
        return Ok(false);
    };
    process_record_active(&record)
}

fn process_record_active(record: &ManagedProcessRecord) -> Result<bool, String> {
    let Some(handle) = open_process(
        record.pid,
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
    )?
    else {
        return Ok(false);
    };
    let wait = unsafe { WaitForSingleObject(handle.0, 0) };
    if wait == WAIT_OBJECT_0 {
        return Ok(false);
    }
    if wait != WAIT_TIMEOUT {
        return Err(format!(
            "cannot inspect Windows process {} wait state: {wait}",
            record.pid
        ));
    }
    Ok(process_creation_time_from_handle(handle.0)? == record.creation_time_100ns)
}

fn stop_managed_process(path: &Path, kind: &str) -> Result<bool, String> {
    let Some(record) = read_process_record(path, kind)? else {
        return Ok(false);
    };
    let Some(handle) = open_process(
        record.pid,
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
    )?
    else {
        remove_process_record(path)?;
        return Ok(false);
    };
    let wait = unsafe { WaitForSingleObject(handle.0, 0) };
    if wait == WAIT_OBJECT_0 {
        remove_process_record(path)?;
        return Ok(false);
    }
    if wait != WAIT_TIMEOUT {
        return Err(format!(
            "cannot inspect Windows {kind} process {} wait state: {wait}",
            record.pid
        ));
    }
    let actual_creation = process_creation_time_from_handle(handle.0)?;
    if actual_creation != record.creation_time_100ns {
        remove_process_record(path)?;
        return Ok(false);
    }
    if unsafe { TerminateProcess(handle.0, 0) } == 0 {
        return Err(format!(
            "cannot stop Windows {kind} process {}: Win32 error {}",
            record.pid,
            unsafe { GetLastError() }
        ));
    }
    let wait = unsafe { WaitForSingleObject(handle.0, PROCESS_STOP_BUDGET_MS) };
    if wait != WAIT_OBJECT_0 {
        return Err(format!(
            "Windows {kind} process {} did not stop within {}ms (wait={wait})",
            record.pid, PROCESS_STOP_BUDGET_MS
        ));
    }
    remove_process_record(path)?;
    Ok(true)
}

fn open_process(pid: u32, access: u32) -> Result<Option<OwnedHandle>, String> {
    let handle = unsafe { OpenProcess(access, 0, pid) };
    if !handle.is_null() {
        return Ok(Some(OwnedHandle(handle)));
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_INVALID_PARAMETER {
        Ok(None)
    } else {
        Err(format!(
            "cannot open managed Windows process {pid}: Win32 error {error}"
        ))
    }
}

fn process_creation_time_from_handle(handle: HANDLE) -> Result<u64, String> {
    let mut creation = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exit = creation;
    let mut kernel = creation;
    let mut user = creation;
    if unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err(format!(
            "GetProcessTimes failed with Win32 error {}",
            unsafe { GetLastError() }
        ));
    }
    Ok(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
}

fn read_process_record(path: &Path, kind: &str) -> Result<Option<ManagedProcessRecord>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot inspect Windows {kind} process record {}: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > PROCESS_RECORD_MAX_BYTES
    {
        return Err(format!(
            "Windows {kind} process record is not a bounded regular file: {}",
            path.display()
        ));
    }
    let bytes = fs::read(path)
        .map_err(|error| format!("cannot read Windows {kind} process record: {error}"))?;
    let record: ManagedProcessRecord = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid Windows {kind} process record: {error}"))?;
    if record.schema_version != PROCESS_RECORD_SCHEMA
        || record.kind != kind
        || record.pid == 0
        || record.creation_time_100ns == 0
        || !valid_generation_id(&record.generation)
    {
        return Err(format!("invalid Windows {kind} process record fields"));
    }
    Ok(Some(record))
}

fn write_process_record(path: &Path, record: &ManagedProcessRecord) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Windows process record has no parent directory".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create Windows process record directory: {error}"))?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec(record)
        .map_err(|error| format!("cannot encode Windows process record: {error}"))?;
    fs::write(&temp, bytes)
        .map_err(|error| format!("cannot stage Windows process record: {error}"))?;
    if path.exists() {
        fs::remove_file(path)
            .map_err(|error| format!("cannot replace Windows process record: {error}"))?;
    }
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("cannot activate Windows process record: {error}")
    })
}

fn remove_process_record(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot remove Windows process record {}: {error}",
            path.display()
        )),
    }
}

fn autostart_arguments(paths: &WindowsPaths) -> String {
    let mut args = Vec::new();
    if let Some(instance) = paths.instance_name.as_deref() {
        args.push("--instance".to_owned());
        args.push(quote_windows_arg(instance));
    }
    args.push("service".to_owned());
    args.push("start".to_owned());
    args.join(" ")
}

fn autostart_command(paths: &WindowsPaths) -> Result<String, String> {
    let binary = paths
        .current_binary
        .to_str()
        .ok_or_else(|| "Windows runtime path is not valid UTF-8".to_owned())?;
    Ok(format!(
        "{} {}",
        quote_windows_arg(binary),
        autostart_arguments(paths)
    ))
}

fn quote_windows_arg(value: &str) -> String {
    if !value.is_empty() && !value.chars().any(|ch| ch.is_whitespace() || ch == '"') {
        return value.to_owned();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for ch in value.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

fn expected_startup_shortcut(paths: &WindowsPaths) -> lnks::Shortcut {
    lnks::ShortcutBuilder::new(paths.current_binary.clone())
        .arguments(autostart_arguments(paths))
        .working_dir(paths.current_dir.clone())
        .description(format!("Start {}", paths.run_value_name))
        .window_state(lnks::WindowState::Minimized)
        .build()
}

fn same_windows_path(left: &Path, right: &Path) -> bool {
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
}

fn startup_shortcut_matches(paths: &WindowsPaths, shortcut: &lnks::Shortcut) -> bool {
    shortcut
        .target_path
        .as_deref()
        .is_some_and(|target| same_windows_path(target, &paths.current_binary))
        && shortcut.arguments.as_deref() == Some(autostart_arguments(paths).as_str())
        && shortcut
            .working_dir
            .as_deref()
            .is_some_and(|directory| same_windows_path(directory, &paths.current_dir))
}

fn startup_shortcut_owned(paths: &WindowsPaths) -> Result<bool, String> {
    if !paths.startup_shortcut.exists() {
        return Ok(false);
    }
    let shortcut = lnks::Shortcut::load(&paths.startup_shortcut).map_err(|error| {
        format!(
            "cannot inspect Windows Startup shortcut {}: {error}",
            paths.startup_shortcut.display()
        )
    })?;
    if !startup_shortcut_matches(paths, &shortcut) {
        return Err(format!(
            "Windows Startup shortcut {} is not owned by this installation",
            paths.startup_shortcut.display()
        ));
    }
    Ok(true)
}

fn ensure_startup_shortcut(paths: &WindowsPaths) -> Result<(), String> {
    if startup_shortcut_owned(paths)? {
        return Ok(());
    }
    let parent = paths
        .startup_shortcut
        .parent()
        .ok_or_else(|| "Windows Startup shortcut has no parent directory".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create Windows Startup directory: {error}"))?;
    let temp = paths
        .startup_shortcut
        .with_extension(format!("tmp-{}.lnk", std::process::id()));
    let _ = fs::remove_file(&temp);
    let expected = expected_startup_shortcut(paths);
    expected
        .save(&temp)
        .map_err(|error| format!("cannot stage Windows Startup shortcut: {error}"))?;
    let observed = lnks::Shortcut::load(&temp)
        .map_err(|error| format!("cannot verify staged Windows Startup shortcut: {error}"))?;
    if !startup_shortcut_matches(paths, &observed) {
        let _ = fs::remove_file(&temp);
        return Err("Windows Startup shortcut failed read-after-write verification".to_owned());
    }
    fs::rename(&temp, &paths.startup_shortcut).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("cannot activate Windows Startup shortcut: {error}")
    })?;
    if !startup_shortcut_owned(paths)? {
        return Err("Windows Startup shortcut disappeared after activation".to_owned());
    }
    Ok(())
}

fn remove_startup_shortcut_if_owned(paths: &WindowsPaths) -> Result<(), String> {
    if !startup_shortcut_owned(paths)? {
        return Ok(());
    }
    fs::remove_file(&paths.startup_shortcut).map_err(|error| {
        format!(
            "cannot remove Windows Startup shortcut {}: {error}",
            paths.startup_shortcut.display()
        )
    })
}

fn restore_startup_shortcut(paths: &WindowsPaths, previously_present: bool) -> Result<(), String> {
    if previously_present {
        ensure_startup_shortcut(paths)
    } else {
        remove_startup_shortcut_if_owned(paths)
    }
}

fn require_autostart(paths: &WindowsPaths) -> Result<(), String> {
    if autostart_registered(paths)? {
        Ok(())
    } else {
        Err("Windows user Startup shortcut is not installed; run `herdr-mcp install`".to_owned())
    }
}

fn autostart_registered(paths: &WindowsPaths) -> Result<bool, String> {
    startup_shortcut_owned(paths)
}

fn legacy_run_registered(paths: &WindowsPaths) -> Result<bool, String> {
    Ok(read_run_value(&paths.run_value_name)?.as_deref()
        == Some(autostart_command(paths)?.as_str()))
}

fn remove_autostart_if_owned(paths: &WindowsPaths) -> Result<(), String> {
    let legacy_run = read_run_value(&paths.run_value_name)?;
    if let Some(existing) = legacy_run.as_deref()
        && existing != autostart_command(paths)?
    {
        return Err(format!(
            "legacy HKCU Run value '{}' is not owned by this installation; refusing to delete it",
            paths.run_value_name
        ));
    }
    remove_startup_shortcut_if_owned(paths)?;
    if legacy_run.is_some() {
        delete_run_value(&paths.run_value_name)?;
    }
    Ok(())
}

fn restore_run_value(name: &str, value: Option<&str>) -> Result<(), String> {
    match value {
        Some(value) => set_run_value(name, value),
        None => delete_run_value(name),
    }
}

fn open_run_key_read() -> Result<Option<OwnedRegistryKey>, String> {
    let subkey = wide_z(RUN_KEY);
    let mut key: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(format!(
            "cannot open HKCU Run key for read: Win32 error {status}"
        ));
    }
    Ok(Some(OwnedRegistryKey(key)))
}

fn open_run_key_write() -> Result<OwnedRegistryKey, String> {
    let subkey = wide_z(RUN_KEY);
    let mut key: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!(
            "cannot open HKCU Run key for write: Win32 error {status}"
        ));
    }
    Ok(OwnedRegistryKey(key))
}

fn read_run_value(name: &str) -> Result<Option<String>, String> {
    let Some(key) = open_run_key_read()? else {
        return Ok(None);
    };
    read_registry_string(key.0, name)
}

fn read_registry_string(key: HKEY, name: &str) -> Result<Option<String>, String> {
    let name_w = wide_z(name);
    let mut value_type = 0u32;
    let mut byte_len = 0u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            name_w.as_ptr(),
            std::ptr::null(),
            &mut value_type,
            std::ptr::null_mut(),
            &mut byte_len,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(format!(
            "cannot query HKCU Run value '{name}': Win32 error {status}"
        ));
    }
    if value_type != REG_SZ || byte_len == 0 || byte_len > 16 * 1024 || byte_len % 2 != 0 {
        return Err(format!("HKCU Run value '{name}' is not a bounded REG_SZ"));
    }
    let mut data = vec![0u16; byte_len as usize / 2];
    let mut actual_len = byte_len;
    let status = unsafe {
        RegQueryValueExW(
            key,
            name_w.as_ptr(),
            std::ptr::null(),
            &mut value_type,
            data.as_mut_ptr().cast::<u8>(),
            &mut actual_len,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!(
            "cannot read HKCU Run value '{name}': Win32 error {status}"
        ));
    }
    if actual_len % 2 != 0 || actual_len as usize > data.len() * 2 {
        return Err(format!(
            "HKCU Run value '{name}' returned an invalid length"
        ));
    }
    data.truncate(actual_len as usize / 2);
    while data.last() == Some(&0) {
        data.pop();
    }
    String::from_utf16(&data)
        .map(Some)
        .map_err(|_| format!("HKCU Run value '{name}' is not valid UTF-16"))
}

fn set_run_value(name: &str, value: &str) -> Result<(), String> {
    let key = open_run_key_write()?;
    let name_w = wide_z(name);
    let value_w = wide_z(value);
    let bytes = value_w
        .len()
        .checked_mul(2)
        .and_then(|len| u32::try_from(len).ok())
        .ok_or_else(|| "HKCU Run value is too large".to_owned())?;
    let status = unsafe {
        RegSetValueExW(
            key.0,
            name_w.as_ptr(),
            0,
            REG_SZ,
            value_w.as_ptr().cast::<u8>(),
            bytes,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!(
            "cannot set HKCU Run value '{name}': Win32 error {status}"
        ));
    }
    let observed = read_registry_string(key.0, name)?;
    if observed.as_deref() != Some(value) {
        return Err(format!(
            "HKCU Run value '{name}' failed read-after-write verification"
        ));
    }
    Ok(())
}

fn delete_run_value(name: &str) -> Result<(), String> {
    let Some(key) = open_run_key_read_write()? else {
        return Ok(());
    };
    let name_w = wide_z(name);
    let status = unsafe { RegDeleteValueW(key.0, name_w.as_ptr()) };
    if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
        return Err(format!(
            "cannot delete HKCU Run value '{name}': Win32 error {status}"
        ));
    }
    Ok(())
}

fn open_run_key_read_write() -> Result<Option<OwnedRegistryKey>, String> {
    let subkey = wide_z(RUN_KEY);
    let mut key: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            &mut key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(format!(
            "cannot open HKCU Run key for mutation: Win32 error {status}"
        ));
    }
    Ok(Some(OwnedRegistryKey(key)))
}

fn wide_z(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn link_enabled(paths: &WindowsPaths) -> Result<bool, String> {
    match fs::symlink_metadata(&paths.link_enabled) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(format!(
            "Windows Link enable marker is not a regular file: {}",
            paths.link_enabled.display()
        )),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "cannot inspect Windows Link enable marker: {error}"
        )),
    }
}

fn set_link_enabled(paths: &WindowsPaths, enabled: bool) -> Result<(), String> {
    if !enabled {
        return match fs::remove_file(&paths.link_enabled) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("cannot remove Windows Link enable marker: {error}")),
        };
    }
    let parent = paths
        .link_enabled
        .parent()
        .ok_or_else(|| "Windows Link enable marker has no parent".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create Windows Link state directory: {error}"))?;
    let temp = paths
        .link_enabled
        .with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temp, b"1\n")
        .map_err(|error| format!("cannot stage Windows Link enable marker: {error}"))?;
    if paths.link_enabled.exists() {
        fs::remove_file(&paths.link_enabled)
            .map_err(|error| format!("cannot replace Windows Link enable marker: {error}"))?;
    }
    fs::rename(&temp, &paths.link_enabled).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("cannot activate Windows Link enable marker: {error}")
    })
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

fn wait_for_health(paths: &WindowsPaths) -> Result<(), String> {
    let deadline = Instant::now() + HEALTH_BUDGET;
    while Instant::now() < deadline {
        if health_once(paths.port) {
            return Ok(());
        }
        if !managed_process_active(&paths.runtime_process, RUNTIME_KIND)? {
            return Err(format!(
                "Windows Herdr MCP runtime exited before becoming healthy on 127.0.0.1:{}{}",
                paths.port,
                startup_log_diagnostic(&paths.runtime_log)
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "Windows Herdr MCP runtime did not become healthy on 127.0.0.1:{}{}",
        paths.port,
        startup_log_diagnostic(&paths.runtime_log)
    ))
}

fn open_startup_log(path: &Path, kind: &str) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create Windows {kind} log directory: {error}"))?;
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|error| format!("cannot open Windows {kind} startup log: {error}"))
}

fn startup_log_diagnostic(path: &Path) -> String {
    match read_startup_log_tail(path) {
        Ok(text) if text.trim().is_empty() => String::new(),
        Ok(text) => format!("; startup log tail: {}", text.trim()),
        Err(error) => format!("; startup log unavailable: {error}"),
    }
}

fn read_startup_log_tail(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let len = file
        .metadata()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
        .len();
    let start = len.saturating_sub(STARTUP_LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))
        .map_err(|error| format!("cannot seek {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(STARTUP_LOG_TAIL_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_test_name(prefix: &str) -> String {
        format!("{prefix}-{}", std::process::id())
    }

    #[test]
    fn hkcu_run_value_round_trip_requires_no_machine_scope() {
        let name = unique_test_name("Herdr MCP UAT");
        let value = r#"\"C:\Windows\System32\cmd.exe\" /c exit"#;
        delete_run_value(&name).unwrap();
        set_run_value(&name, value).unwrap();
        assert_eq!(read_run_value(&name).unwrap().as_deref(), Some(value));
        delete_run_value(&name).unwrap();
        assert_eq!(read_run_value(&name).unwrap(), None);
    }

    #[test]
    fn detached_process_record_stops_only_the_exact_process() {
        let root = env::temp_dir().join(unique_test_name("herdr-mcp-process-test"));
        fs::create_dir_all(&root).unwrap();
        let record_path = root.join("process.json");
        let binary = PathBuf::from("powershell.exe");
        let mut child = Command::new(&binary)
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let record = ManagedProcessRecord {
            schema_version: PROCESS_RECORD_SCHEMA,
            kind: "test".to_owned(),
            pid: child.id(),
            creation_time_100ns: process_creation_time_from_handle(child.as_raw_handle() as HANDLE)
                .unwrap(),
            generation: "rust-test".to_owned(),
        };
        write_process_record(&record_path, &record).unwrap();
        drop(child);
        assert!(managed_process_active(&record_path, "test").unwrap());
        assert!(stop_managed_process(&record_path, "test").unwrap());
        assert!(!record_path.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn windows_argument_quoting_handles_spaces_and_quotes() {
        assert_eq!(quote_windows_arg("plain"), "plain");
        assert_eq!(quote_windows_arg("two words"), "\"two words\"");
        assert_eq!(quote_windows_arg(""), "\"\"");
        assert_eq!(quote_windows_arg("a\\\"b"), "\"a\\\\\\\"b\"");
    }
}
