//! Daily production auto-update scheduler.
//!
//! The scheduler owns only a low-frequency launchd trigger. Release discovery,
//! provenance verification, staging, activation, rollback, and durable update
//! state remain owned by `updater`.

#[cfg(target_os = "macos")]
use crate::paths::RuntimePaths;
#[cfg(target_os = "macos")]
use plist::{Dictionary, Value as PlistValue};
use serde_json::{Value, json};
#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) const AUTO_UPDATE_LABEL: &str = "dev.herdr-mcp.auto-update";
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
const AUTO_UPDATE_INTERVAL_SECONDS: u64 = 24 * 60 * 60;
#[cfg(target_os = "macos")]
const SERVICE_UNINSTALL_FENCE: &str = "service-uninstall.fence";
#[cfg(target_os = "macos")]
const SERVICE_UNINSTALL_FENCE_BYTES: &[u8] = b"herdr-mcp-service-uninstall-fence-v1\n";

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchdState {
    Loaded,
    Absent,
}

#[cfg(target_os = "macos")]
pub(crate) fn reconcile_after_service_install() -> Value {
    match reconcile_after_service_install_inner() {
        Ok(value) => value,
        Err(error) => json!({
            "ok": false,
            "installed": false,
            "label": AUTO_UPDATE_LABEL,
            "detail": error,
        }),
    }
}

/// Strict removal used by service/product lifecycle: uncertainty must block
/// service removal rather than leave a loaded updater pointing at a deleted runtime.
#[cfg(target_os = "macos")]
pub(crate) fn remove_before_service_uninstall_checked() -> Result<Value, String> {
    remove_before_service_uninstall_inner()
}

/// Mutation-grade ownership check for product uninstall. A missing scheduler
/// is fine, an owned scheduler is removable, and any loaded-without-plist or
/// foreign plist state fails closed before the product journal starts removal.
#[cfg(target_os = "macos")]
pub(crate) fn product_uninstall_preflight() -> Result<bool, String> {
    let paths = RuntimePaths::discover()?;
    if paths.instance.is_named() {
        return Ok(false);
    }
    let home = home_dir()?;
    let plist_path = launch_agent_path(&home);
    let state = launchd_state(AUTO_UPDATE_LABEL)?;
    match read_optional_regular(&plist_path)? {
        Some(bytes) => {
            verify_owned_plist(&bytes, &paths)?;
            Ok(true)
        }
        None if state == LaunchdState::Loaded => Err(
            "auto-update LaunchAgent is loaded without an inspectable owned plist; refusing product uninstall"
                .to_owned(),
        ),
        None => Ok(false),
    }
}

pub(crate) fn status_line() -> String {
    match status_snapshot() {
        Ok(value) => {
            if value.get("skipped").and_then(Value::as_bool) == Some(true) {
                return "not scheduled for named instance".to_owned();
            }
            let present = value.get("present").and_then(Value::as_bool) == Some(true);
            let loaded = value.get("loaded").and_then(Value::as_bool) == Some(true);
            let owned = value.get("owned").and_then(Value::as_bool) == Some(true);
            if present && loaded && owned {
                "daily, prod-runtime + stable-release only, background".to_owned()
            } else if present && owned {
                "installed but not loaded".to_owned()
            } else if present {
                "foreign/unrecognized scheduler preserved".to_owned()
            } else {
                "not installed".to_owned()
            }
        }
        Err(error) => format!("unknown ({error})"),
    }
}

/// Refuse update discovery/activation while a service uninstall fence is armed.
#[cfg(target_os = "macos")]
pub(crate) fn ensure_updates_allowed() -> Result<(), String> {
    let paths = RuntimePaths::discover()?;
    match read_service_uninstall_fence(&paths)? {
        FenceState::Absent => Ok(()),
        FenceState::Owned => Err(
            "updates are suspended because herdr-mcp service was explicitly uninstalled; run herdr-mcp install or reinstall to re-enable updates"
                .to_owned(),
        ),
    }
}

/// Validate any durable fence before a manual service install mutates state.
#[cfg(target_os = "macos")]
pub(crate) fn preflight_service_install_fence() -> Result<(), String> {
    let paths = RuntimePaths::discover()?;
    let _ = read_service_uninstall_fence(&paths)?;
    Ok(())
}

/// Arm a durable fence before the scheduler is booted out or the service is removed.
#[cfg(target_os = "macos")]
pub(crate) fn arm_service_uninstall_fence() -> Result<(), String> {
    let paths = RuntimePaths::discover()?;
    if paths.instance.is_named() {
        return Ok(());
    }
    if read_service_uninstall_fence(&paths)? == FenceState::Owned {
        return Ok(());
    }
    let fence_path = service_uninstall_fence_path(&paths)?;
    let update_dir = fence_path
        .parent()
        .ok_or_else(|| "service-uninstall fence has no parent directory".to_owned())?;
    ensure_real_dir(update_dir)?;
    atomic_write(&fence_path, SERVICE_UNINSTALL_FENCE_BYTES, 0o600)
}

/// A successful explicit install/reinstall is the only operation that clears the fence.
#[cfg(target_os = "macos")]
pub(crate) fn clear_service_uninstall_fence() -> Result<(), String> {
    let paths = RuntimePaths::discover()?;
    if paths.instance.is_named() {
        return Ok(());
    }
    let path = service_uninstall_fence_path(&paths)?;
    match read_service_uninstall_fence(&paths)? {
        FenceState::Absent => Ok(()),
        FenceState::Owned => fs::remove_file(&path)
            .map_err(|error| format!("cannot clear update service-uninstall fence: {error}")),
    }
}

/// Exact prior state of the auto-update scheduler and the service-uninstall
/// fence, captured before a service install so a failed post-commit step can
/// restore them. Fails closed on any foreign/uninspectable scheduler state.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
pub(crate) struct InstallIntegrationSnapshot {
    /// Prior owned scheduler plist bytes, or `None` when absent.
    scheduler_plist: Option<Vec<u8>>,
    /// Prior launchd load state of the scheduler.
    scheduler_loaded: bool,
    /// Prior service-uninstall fence state.
    fence_owned: bool,
}

#[cfg(target_os = "macos")]
impl InstallIntegrationSnapshot {
    pub(crate) fn capture() -> Result<Self, String> {
        let paths = RuntimePaths::discover()?;
        if paths.instance.is_named() {
            return Ok(Self {
                scheduler_plist: None,
                scheduler_loaded: false,
                fence_owned: false,
            });
        }
        let home = home_dir()?;
        let plist_path = launch_agent_path(&home);
        let scheduler_plist = read_optional_regular(&plist_path)?;
        let scheduler_loaded = launchd_state(AUTO_UPDATE_LABEL)? == LaunchdState::Loaded;
        // A loaded scheduler without an inspectable owned plist is foreign and
        // must fail closed before the service commit, never silently replaced.
        if scheduler_loaded && scheduler_plist.is_none() {
            return Err(
                "auto-update LaunchAgent is loaded without an inspectable owned plist; refusing service install"
                    .to_owned(),
            );
        }
        if let Some(bytes) = scheduler_plist.as_deref() {
            verify_owned_plist(bytes, &paths)?;
        }
        let fence_owned = read_service_uninstall_fence(&paths)? == FenceState::Owned;
        Ok(Self {
            scheduler_plist,
            scheduler_loaded,
            fence_owned,
        })
    }

    /// Restore identity, scheduler, and fence to their exact prior state in a
    /// fail-safe order. The protective service-uninstall fence is established
    /// FIRST so a scheduler left loaded by a later failure can never resurrect
    /// a service that compensation just uninstalled; identity and scheduler
    /// plist+load state are restored next; and only after the scheduler is
    /// confirmed at its prior exact state is the fence cleared (per the prior
    /// snapshot). Any restore error keeps the protective fence armed and is
    /// reported as degraded-but-fenced.
    pub(crate) fn restore(
        &self,
        restore_identity: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        self.restore_impl(restore_identity, launchd_state, bootstrap, bootout)
    }

    /// Testable core of [`Self::restore`] with injectable launchd operations so
    /// scheduler bootstrap/bootout/state failures can be fault-injected without
    /// a real launchd.
    fn restore_impl<State, Bootstrap, Bootout>(
        &self,
        restore_identity: impl FnOnce() -> Result<(), String>,
        state: State,
        bootstrap: Bootstrap,
        bootout: Bootout,
    ) -> Result<(), String>
    where
        State: Fn(&str) -> Result<LaunchdState, String>,
        Bootstrap: Fn(&Path) -> Result<(), String>,
        Bootout: Fn(&str) -> Result<(), String>,
    {
        let paths = RuntimePaths::discover()?;
        if paths.instance.is_named() {
            return Ok(());
        }
        let home = home_dir()?;
        let plist_path = launch_agent_path(&home);
        let fence_path = service_uninstall_fence_path(&paths)?;

        // Phase 1: establish the protective service-uninstall fence FIRST.
        // Updates are refused while the fence is owned, so a scheduler left
        // loaded by a later restore failure cannot resurrect a service.
        let update_dir = fence_path
            .parent()
            .ok_or_else(|| "service-uninstall fence has no parent directory".to_owned())?;
        ensure_real_dir(update_dir)?;
        if read_service_uninstall_fence(&paths)? != FenceState::Owned {
            atomic_write(&fence_path, SERVICE_UNINSTALL_FENCE_BYTES, 0o600)?;
        }

        // Phase 2: restore identity, then scheduler plist + load state.
        let identity_result = restore_identity();
        let scheduler_result =
            self.restore_scheduler(&paths, &plist_path, &state, &bootstrap, &bootout);

        // Phase 3: only clear the protective fence when every restore reached
        // the prior exact state AND the prior snapshot had no fence.
        match (identity_result, scheduler_result) {
            (Ok(()), Ok(())) => {
                if !self.fence_owned {
                    fs::remove_file(&fence_path).map_err(|error| {
                        format!("cannot clear restored service-uninstall fence: {error}")
                    })?;
                }
                Ok(())
            }
            (identity_result, scheduler_result) => {
                let mut failures = Vec::new();
                if let Err(error) = identity_result {
                    failures.push(format!("identity restore failed: {error}"));
                }
                if let Err(error) = scheduler_result {
                    failures.push(format!("scheduler restore failed: {error}"));
                }
                Err(format!(
                    "service install integration restore degraded-but-fenced: {}; protective service-uninstall fence remains armed",
                    failures.join("; ")
                ))
            }
        }
    }

    /// Restore the scheduler plist bytes and load state to their prior exact
    /// state. Load state is an independent fact: even when the plist bytes
    /// already match the prior snapshot, the loaded/unloaded state is still
    /// restored.
    fn restore_scheduler<State, Bootstrap, Bootout>(
        &self,
        paths: &RuntimePaths,
        plist_path: &Path,
        state: &State,
        bootstrap: &Bootstrap,
        bootout: &Bootout,
    ) -> Result<(), String>
    where
        State: Fn(&str) -> Result<LaunchdState, String>,
        Bootstrap: Fn(&Path) -> Result<(), String>,
        Bootout: Fn(&str) -> Result<(), String>,
    {
        let current_loaded = state(AUTO_UPDATE_LABEL)? == LaunchdState::Loaded;
        let current_plist = read_optional_regular(plist_path)?;
        if let Some(bytes) = current_plist.as_deref() {
            verify_owned_plist(bytes, paths)?;
        }

        // Restore the plist bytes if they differ from prior.
        let plist_differs = match (&self.scheduler_plist, current_plist) {
            (Some(prior), Some(current)) => prior != &current,
            (Some(_), None) => true,
            (None, Some(_)) => true,
            (None, None) => false,
        };
        if plist_differs {
            if current_loaded {
                bootout(AUTO_UPDATE_LABEL)?;
            }
            match &self.scheduler_plist {
                Some(prior) => atomic_write(plist_path, prior, 0o600)?,
                None => fs::remove_file(plist_path)
                    .map_err(|error| format!("cannot remove auto-update LaunchAgent: {error}"))?,
            }
        }

        // Load state is an independent fact: restore it to the prior exact
        // state even when the plist bytes already match.
        let now_loaded = state(AUTO_UPDATE_LABEL)? == LaunchdState::Loaded;
        match (self.scheduler_loaded, now_loaded) {
            (true, false) => bootstrap(plist_path)?,
            (false, true) => bootout(AUTO_UPDATE_LABEL)?,
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(target_os = "macos")]
enum FenceState {
    Absent,
    Owned,
}

#[cfg(target_os = "macos")]
fn service_uninstall_fence_path(_paths: &RuntimePaths) -> Result<PathBuf, String> {
    Ok(home_dir()?
        .join("Library/Caches/herdr-mcp/update")
        .join(SERVICE_UNINSTALL_FENCE))
}

#[cfg(target_os = "macos")]
fn read_service_uninstall_fence(paths: &RuntimePaths) -> Result<FenceState, String> {
    if paths.instance.is_named() {
        return Ok(FenceState::Absent);
    }
    let path = service_uninstall_fence_path(paths)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FenceState::Absent);
        }
        Err(error) => {
            return Err(format!(
                "cannot inspect update service-uninstall fence {}: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 128 {
        return Err(format!(
            "update service-uninstall fence is not an owned regular file: {}",
            path.display()
        ));
    }
    let bytes = fs::read(&path)
        .map_err(|error| format!("cannot read update service-uninstall fence: {error}"))?;
    if bytes != SERVICE_UNINSTALL_FENCE_BYTES {
        return Err("update service-uninstall fence has foreign/unrecognized contents".to_owned());
    }
    Ok(FenceState::Owned)
}

#[cfg(target_os = "macos")]
fn ensure_real_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "update scheduler directory must not be a symlink: {}",
            path.display()
        )),
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(format!(
            "update scheduler path is not a directory: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                    .map_err(|error| format!("cannot secure {}: {error}", path.display()))?;
            }
            Ok(())
        }
        Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
    }
}

#[cfg(not(target_os = "macos"))]
fn status_snapshot() -> Result<Value, String> {
    Ok(json!({
        "ok": true,
        "present": false,
        "loaded": false,
        "owned": false,
        "skipped": true,
        "reason": "unsupported_platform",
    }))
}

#[cfg(target_os = "macos")]
fn status_snapshot() -> Result<Value, String> {
    let paths = RuntimePaths::discover()?;
    if paths.instance.is_named() {
        return Ok(json!({
            "ok": true,
            "present": false,
            "loaded": false,
            "owned": false,
            "skipped": true,
            "reason": "named_instance",
        }));
    }
    let home = home_dir()?;
    let plist_path = launch_agent_path(&home);
    let state = launchd_state(AUTO_UPDATE_LABEL)?;
    let existing = read_optional_regular(&plist_path)?;
    let (present, owned, detail) = match existing {
        Some(bytes) => match verify_owned_plist(&bytes, &paths) {
            Ok(()) => (true, true, None),
            Err(error) => (true, false, Some(error)),
        },
        None => (false, false, None),
    };
    Ok(json!({
        "ok": owned && state == LaunchdState::Loaded,
        "present": present,
        "loaded": state == LaunchdState::Loaded,
        "owned": owned,
        "label": AUTO_UPDATE_LABEL,
        "interval_seconds": AUTO_UPDATE_INTERVAL_SECONDS,
        "stable_only": true,
        "detail": detail,
    }))
}

#[cfg(target_os = "macos")]
fn reconcile_after_service_install_inner() -> Result<Value, String> {
    let paths = RuntimePaths::discover()?;
    if paths.instance.is_named() {
        return Ok(json!({
            "ok": true,
            "installed": false,
            "skipped": true,
            "reason": "named_instance",
        }));
    }
    let home = home_dir()?;
    let plist_path = launch_agent_path(&home);
    let expected = encode_plist(&paths)?;
    let state = launchd_state(AUTO_UPDATE_LABEL)?;

    if let Some(existing) = read_optional_regular(&plist_path)? {
        verify_owned_plist(&existing, &paths)?;
        if existing == expected {
            if state == LaunchdState::Absent {
                bootstrap(&plist_path)?;
            }
            return Ok(json!({
                "ok": true,
                "installed": true,
                "changed": false,
                "label": AUTO_UPDATE_LABEL,
                "interval_seconds": AUTO_UPDATE_INTERVAL_SECONDS,
                "stable_only": true,
            }));
        }
        if state == LaunchdState::Loaded {
            bootout(AUTO_UPDATE_LABEL)?;
        }
    } else if state == LaunchdState::Loaded {
        return Err(
            "auto-update LaunchAgent is loaded without an inspectable owned plist; refusing replacement"
                .to_owned(),
        );
    }

    atomic_write(&plist_path, &expected, 0o600)?;
    bootstrap(&plist_path)?;
    Ok(json!({
        "ok": true,
        "installed": true,
        "changed": true,
        "label": AUTO_UPDATE_LABEL,
        "interval_seconds": AUTO_UPDATE_INTERVAL_SECONDS,
        "stable_only": true,
    }))
}

#[cfg(target_os = "macos")]
fn remove_before_service_uninstall_inner() -> Result<Value, String> {
    let paths = RuntimePaths::discover()?;
    if paths.instance.is_named() {
        return Ok(json!({
            "ok": true,
            "removed": false,
            "skipped": true,
            "reason": "named_instance",
        }));
    }
    let home = home_dir()?;
    let plist_path = launch_agent_path(&home);
    let state = launchd_state(AUTO_UPDATE_LABEL)?;
    let existing = read_optional_regular(&plist_path)?;
    match existing {
        Some(bytes) => verify_owned_plist(&bytes, &paths)?,
        None if state == LaunchdState::Loaded => {
            return Err(
                "auto-update LaunchAgent is loaded without an inspectable owned plist; refusing blind removal"
                    .to_owned(),
            );
        }
        None => {
            return Ok(json!({
                "ok": true,
                "removed": false,
                "label": AUTO_UPDATE_LABEL,
            }));
        }
    }
    if state == LaunchdState::Loaded {
        bootout(AUTO_UPDATE_LABEL)?;
    }
    fs::remove_file(&plist_path)
        .map_err(|error| format!("cannot remove {}: {error}", plist_path.display()))?;
    Ok(json!({
        "ok": true,
        "removed": true,
        "label": AUTO_UPDATE_LABEL,
    }))
}

#[cfg(target_os = "macos")]
fn encode_plist(paths: &RuntimePaths) -> Result<Vec<u8>, String> {
    let current_binary = paths.config_dir.join("runtime/current/herdr-mcp");
    let mut root = Dictionary::new();
    root.insert(
        "Label".to_owned(),
        PlistValue::String(AUTO_UPDATE_LABEL.to_owned()),
    );
    root.insert(
        "ProgramArguments".to_owned(),
        PlistValue::Array(
            [
                current_binary.to_string_lossy().into_owned(),
                "update".to_owned(),
                "auto".to_owned(),
            ]
            .into_iter()
            .map(PlistValue::String)
            .collect(),
        ),
    );
    let mut env = Dictionary::new();
    env.insert(
        "HERDR_MCP_CONFIG_DIR".to_owned(),
        PlistValue::String(paths.config_dir.to_string_lossy().into_owned()),
    );
    root.insert(
        "EnvironmentVariables".to_owned(),
        PlistValue::Dictionary(env),
    );
    root.insert("RunAtLoad".to_owned(), PlistValue::Boolean(true));
    root.insert(
        "StartInterval".to_owned(),
        PlistValue::Integer(AUTO_UPDATE_INTERVAL_SECONDS.into()),
    );
    root.insert(
        "ProcessType".to_owned(),
        PlistValue::String("Background".to_owned()),
    );
    root.insert("LowPriorityIO".to_owned(), PlistValue::Boolean(true));
    root.insert(
        "ThrottleInterval".to_owned(),
        PlistValue::Integer(300_u64.into()),
    );
    let mut bytes = Vec::new();
    PlistValue::Dictionary(root)
        .to_writer_xml(&mut bytes)
        .map_err(|error| format!("cannot encode auto-update LaunchAgent: {error}"))?;
    Ok(bytes)
}

#[cfg(target_os = "macos")]
fn verify_owned_plist(bytes: &[u8], paths: &RuntimePaths) -> Result<(), String> {
    let value = PlistValue::from_reader(std::io::Cursor::new(bytes))
        .map_err(|error| format!("cannot parse auto-update LaunchAgent: {error}"))?;
    let dict = value
        .as_dictionary()
        .ok_or_else(|| "auto-update LaunchAgent root must be a dictionary".to_owned())?;
    if dict.get("Label").and_then(PlistValue::as_string) != Some(AUTO_UPDATE_LABEL) {
        return Err("auto-update LaunchAgent Label is not owned by herdr-mcp".to_owned());
    }
    let args = dict
        .get("ProgramArguments")
        .and_then(PlistValue::as_array)
        .ok_or_else(|| "auto-update LaunchAgent ProgramArguments are missing".to_owned())?;
    let args = args
        .iter()
        .map(|value| value.as_string().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "auto-update LaunchAgent ProgramArguments must be strings".to_owned())?;
    let expected_binary = paths.config_dir.join("runtime/current/herdr-mcp");
    if args
        != vec![
            expected_binary.to_string_lossy().into_owned(),
            "update".to_owned(),
            "auto".to_owned(),
        ]
    {
        return Err(
            "auto-update LaunchAgent ProgramArguments are not owned by herdr-mcp".to_owned(),
        );
    }
    let env = dict
        .get("EnvironmentVariables")
        .and_then(PlistValue::as_dictionary)
        .ok_or_else(|| "auto-update LaunchAgent EnvironmentVariables are missing".to_owned())?;
    if env
        .get("HERDR_MCP_CONFIG_DIR")
        .and_then(PlistValue::as_string)
        != Some(paths.config_dir.to_string_lossy().as_ref())
    {
        return Err("auto-update LaunchAgent config root does not match this instance".to_owned());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_agent_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{AUTO_UPDATE_LABEL}.plist"))
}

#[cfg(target_os = "macos")]
fn read_optional_regular(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "auto-update LaunchAgent must be a regular file: {}",
            path.display()
        ));
    }
    fs::read(path)
        .map(Some)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

#[cfg(target_os = "macos")]
fn launchd_state(label: &str) -> Result<LaunchdState, String> {
    let target = format!("gui/{}/{label}", unsafe { libc::geteuid() });
    let output = Command::new("launchctl")
        .args(["print", &target])
        .output()
        .map_err(|error| format!("cannot query launchd state for {label}: {error}"))?;
    if output.status.success() {
        return Ok(LaunchdState::Loaded);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    if stderr.contains("could not find service")
        || stderr.contains("service not found")
        || stderr.contains("no such process")
    {
        return Ok(LaunchdState::Absent);
    }
    Err(format!(
        "cannot determine launchd state for {label}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(target_os = "macos")]
fn bootstrap(plist_path: &Path) -> Result<(), String> {
    let domain = format!("gui/{}", unsafe { libc::geteuid() });
    let output = Command::new("launchctl")
        .args(["bootstrap", &domain, plist_path.to_string_lossy().as_ref()])
        .output()
        .map_err(|error| format!("cannot bootstrap auto-update LaunchAgent: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "cannot bootstrap auto-update LaunchAgent: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(target_os = "macos")]
fn bootout(label: &str) -> Result<(), String> {
    let target = format!("gui/{}/{label}", unsafe { libc::geteuid() });
    let output = Command::new("launchctl")
        .args(["bootout", &target])
        .output()
        .map_err(|error| format!("cannot bootout auto-update LaunchAgent: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "cannot bootout auto-update LaunchAgent: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(target_os = "macos")]
fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(format!("refusing to replace symlink {}", path.display()));
    }
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("auto-update.plist"),
        std::process::id()
    ));
    fs::write(&tmp, bytes).map_err(|error| format!("cannot write {}: {error}", tmp.display()))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot chmod {}: {error}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|error| format!("cannot activate {}: {error}", path.display()))
}

#[cfg(target_os = "macos")]
fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is required for auto-update scheduler".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn interval_is_daily_and_label_is_stable() {
        assert_eq!(AUTO_UPDATE_INTERVAL_SECONDS, 86_400);
        assert_eq!(AUTO_UPDATE_LABEL, "dev.herdr-mcp.auto-update");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plist_is_stable_daily_background_trigger_for_default_instance() {
        let _guard = crate::test_env::lock();
        let old_home = std::env::var_os("HOME");
        let old_instance = std::env::var_os("HERDR_MCP_INSTANCE");
        let old_config = std::env::var_os("HERDR_MCP_CONFIG_DIR");
        let root = std::env::temp_dir().join(format!(
            "herdr-auto-update-plist-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let home = root.join("home");
        let config = home.join(".config/herdr-mcp");
        fs::create_dir_all(&config).unwrap();
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::remove_var("HERDR_MCP_INSTANCE");
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &config);
        }
        let paths = RuntimePaths::discover().unwrap();
        let bytes = encode_plist(&paths).unwrap();
        verify_owned_plist(&bytes, &paths).unwrap();
        let value = PlistValue::from_reader(std::io::Cursor::new(bytes)).unwrap();
        let dict = value.as_dictionary().unwrap();
        assert_eq!(
            dict.get("StartInterval")
                .and_then(PlistValue::as_unsigned_integer),
            Some(86_400)
        );
        assert_eq!(
            dict.get("RunAtLoad").and_then(PlistValue::as_boolean),
            Some(true)
        );
        unsafe {
            match old_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match old_instance {
                Some(value) => std::env::set_var("HERDR_MCP_INSTANCE", value),
                None => std::env::remove_var("HERDR_MCP_INSTANCE"),
            }
            match old_config {
                Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn service_uninstall_fence_blocks_updates_until_explicit_install_clears_it() {
        let _guard = crate::test_env::lock();
        let old_home = std::env::var_os("HOME");
        let old_instance = std::env::var_os("HERDR_MCP_INSTANCE");
        let old_config = std::env::var_os("HERDR_MCP_CONFIG_DIR");
        let root = std::env::temp_dir().join(format!(
            "herdr-auto-update-fence-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = root.join("home");
        let config = home.join(".config/herdr-mcp");
        fs::create_dir_all(&config).unwrap();
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::remove_var("HERDR_MCP_INSTANCE");
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &config);
        }

        assert!(ensure_updates_allowed().is_ok());
        arm_service_uninstall_fence().unwrap();
        arm_service_uninstall_fence().unwrap();
        assert!(ensure_updates_allowed().is_err());
        let paths = RuntimePaths::discover().unwrap();
        let fence = service_uninstall_fence_path(&paths).unwrap();
        assert!(
            !fence.starts_with(&config),
            "the service-uninstall fence must survive product config deletion"
        );
        fs::remove_dir_all(&config).unwrap();
        assert!(
            ensure_updates_allowed().is_err(),
            "deleting product config must not clear the uninstall intent"
        );
        preflight_service_install_fence().unwrap();
        clear_service_uninstall_fence().unwrap();
        assert!(ensure_updates_allowed().is_ok());

        fs::create_dir_all(fence.parent().unwrap()).unwrap();
        fs::write(&fence, b"foreign\n").unwrap();
        assert!(preflight_service_install_fence().is_err());
        assert!(arm_service_uninstall_fence().is_err());
        assert!(clear_service_uninstall_fence().is_err());

        unsafe {
            match old_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match old_instance {
                Some(value) => std::env::set_var("HERDR_MCP_INSTANCE", value),
                None => std::env::remove_var("HERDR_MCP_INSTANCE"),
            }
            match old_config {
                Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    /// Set up a temp HOME + config and return the env guard, root, home, and
    /// discovered paths. The guard must be held for the whole test so env
    /// mutations are serialized against other tests.
    #[cfg(target_os = "macos")]
    fn snapshot_fixture(
        name: &str,
    ) -> (
        std::sync::MutexGuard<'static, ()>,
        PathBuf,
        PathBuf,
        RuntimePaths,
    ) {
        let guard = crate::test_env::lock();
        let root = std::env::temp_dir().join(format!(
            "herdr-install-snapshot-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = root.join("home");
        let config = home.join(".config/herdr-mcp");
        fs::create_dir_all(&config).unwrap();
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::remove_var("HERDR_MCP_INSTANCE");
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &config);
        }
        let paths = RuntimePaths::discover().unwrap();
        (guard, root, home, paths)
    }

    #[cfg(target_os = "macos")]
    fn snapshot(
        scheduler_plist: Option<Vec<u8>>,
        scheduler_loaded: bool,
        fence_owned: bool,
    ) -> InstallIntegrationSnapshot {
        InstallIntegrationSnapshot {
            scheduler_plist,
            scheduler_loaded,
            fence_owned,
        }
    }

    /// A fake launchd whose loaded state is tracked in a shared cell, so
    /// bootstrap/bootout mutate the state that `state` reports.
    #[cfg(target_os = "macos")]
    struct FakeLaunchd {
        loaded: std::cell::Cell<bool>,
        bootout_fail: bool,
        bootstrap_fail: bool,
    }

    #[cfg(target_os = "macos")]
    impl FakeLaunchd {
        fn new(loaded: bool) -> Self {
            Self {
                loaded: std::cell::Cell::new(loaded),
                bootout_fail: false,
                bootstrap_fail: false,
            }
        }
        fn state(&self, _label: &str) -> Result<LaunchdState, String> {
            Ok(if self.loaded.get() {
                LaunchdState::Loaded
            } else {
                LaunchdState::Absent
            })
        }
        fn bootstrap(&self, _path: &Path) -> Result<(), String> {
            if self.bootstrap_fail {
                Err("bootstrap boom".to_owned())
            } else {
                self.loaded.set(true);
                Ok(())
            }
        }
        fn bootout(&self, _label: &str) -> Result<(), String> {
            if self.bootout_fail {
                Err("bootout boom".to_owned())
            } else {
                self.loaded.set(false);
                Ok(())
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn fence_owned(paths: &RuntimePaths) -> bool {
        read_service_uninstall_fence(paths).unwrap() == FenceState::Owned
    }

    #[cfg(target_os = "macos")]
    fn scheduler_plist_path(home: &Path) -> PathBuf {
        launch_agent_path(home)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fresh_install_restore_removes_scheduler_and_clears_fence_on_success() {
        let (_guard, root, home, paths) = snapshot_fixture("fresh-ok");
        let prior = snapshot(None, false, false);
        // Simulate a committed fresh install: scheduler plist present + loaded,
        // fence cleared.
        let plist = scheduler_plist_path(&home);
        fs::create_dir_all(plist.parent().unwrap()).unwrap();
        fs::write(&plist, encode_plist(&paths).unwrap()).unwrap();
        let fake = FakeLaunchd::new(true);

        prior
            .restore_impl(
                || Ok(()),
                |label| fake.state(label),
                |path| fake.bootstrap(path),
                |label| fake.bootout(label),
            )
            .unwrap();

        assert!(
            !plist.exists(),
            "fresh install restore must remove the scheduler"
        );
        assert!(
            !fake.loaded.get(),
            "fresh install restore must unload the scheduler"
        );
        assert!(
            !fence_owned(&paths),
            "prior fence was absent, so restore must leave it absent"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn upgrade_restore_restores_prior_scheduler_fence_and_identity() {
        let (_guard, root, home, paths) = snapshot_fixture("upgrade-ok");
        let prior_bytes = encode_plist(&paths).unwrap();
        let prior = snapshot(Some(prior_bytes.clone()), true, true);
        // Simulate a committed upgrade: scheduler plist rewritten + loaded,
        // fence cleared.
        let plist = scheduler_plist_path(&home);
        fs::create_dir_all(plist.parent().unwrap()).unwrap();
        let mut different = prior_bytes.clone();
        different.push(b' ');
        fs::write(&plist, &different).unwrap();
        let fake = FakeLaunchd::new(true);
        let mut identity_restored = false;

        prior
            .restore_impl(
                || {
                    identity_restored = true;
                    Ok(())
                },
                |label| fake.state(label),
                |path| fake.bootstrap(path),
                |label| fake.bootout(label),
            )
            .unwrap();

        assert!(identity_restored);
        assert_eq!(
            fs::read(&plist).unwrap(),
            prior_bytes,
            "plist must be restored"
        );
        assert!(fake.loaded.get(), "prior scheduler was loaded");
        assert!(
            fence_owned(&paths),
            "prior fence was owned, so restore must re-arm it"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn same_plist_bytes_still_restores_load_state() {
        let (_guard, root, home, paths) = snapshot_fixture("same-bytes");
        let bytes = encode_plist(&paths).unwrap();
        // Prior: plist present + loaded. Current: same plist bytes but unloaded.
        let prior = snapshot(Some(bytes.clone()), true, false);
        let plist = scheduler_plist_path(&home);
        fs::create_dir_all(plist.parent().unwrap()).unwrap();
        fs::write(&plist, &bytes).unwrap();
        let fake = FakeLaunchd::new(false);

        prior
            .restore_impl(
                || Ok(()),
                |label| fake.state(label),
                |path| fake.bootstrap(path),
                |label| fake.bootout(label),
            )
            .unwrap();

        assert!(
            fake.loaded.get(),
            "load state is an independent fact: same plist bytes must still be bootstrapped to prior loaded state"
        );
        assert_eq!(fs::read(&plist).unwrap(), bytes);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scheduler_bootout_failure_keeps_protective_fence_armed() {
        let (_guard, root, home, paths) = snapshot_fixture("bootout-fail");
        let prior = snapshot(None, false, false);
        // Committed fresh install: scheduler loaded, fence cleared.
        let plist = scheduler_plist_path(&home);
        fs::create_dir_all(plist.parent().unwrap()).unwrap();
        fs::write(&plist, encode_plist(&paths).unwrap()).unwrap();
        let mut fake = FakeLaunchd::new(true);
        fake.bootout_fail = true;

        let err = prior
            .restore_impl(
                || Ok(()),
                |label| fake.state(label),
                |path| fake.bootstrap(path),
                |label| fake.bootout(label),
            )
            .unwrap_err();

        assert!(
            err.contains("degraded-but-fenced"),
            "restore error must be reported as degraded-but-fenced: {err}"
        );
        assert!(
            fence_owned(&paths),
            "a restore failure must leave the protective fence armed, never absent"
        );
        assert!(
            fake.loaded.get(),
            "scheduler may remain loaded, but it must be fenced"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn identity_restore_failure_keeps_protective_fence_armed() {
        let (_guard, root, home, paths) = snapshot_fixture("identity-fail");
        let prior = snapshot(None, false, false);
        let plist = scheduler_plist_path(&home);
        fs::create_dir_all(plist.parent().unwrap()).unwrap();
        fs::write(&plist, encode_plist(&paths).unwrap()).unwrap();
        let fake = FakeLaunchd::new(true);

        let err = prior
            .restore_impl(
                || Err("identity restore boom".to_owned()),
                |label| fake.state(label),
                |path| fake.bootstrap(path),
                |label| fake.bootout(label),
            )
            .unwrap_err();

        assert!(err.contains("degraded-but-fenced"));
        assert!(err.contains("identity restore boom"));
        assert!(
            fence_owned(&paths),
            "identity restore failure must leave the protective fence armed"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn noop_restore_preserves_prior_integration_state() {
        let (_guard, root, home, paths) = snapshot_fixture("noop");
        let bytes = encode_plist(&paths).unwrap();
        // Prior and current are identical: plist present + loaded, fence owned.
        let prior = snapshot(Some(bytes.clone()), true, true);
        let plist = scheduler_plist_path(&home);
        fs::create_dir_all(plist.parent().unwrap()).unwrap();
        fs::write(&plist, &bytes).unwrap();
        let fake = FakeLaunchd::new(true);
        let mut identity_restored = false;

        prior
            .restore_impl(
                || {
                    identity_restored = true;
                    Ok(())
                },
                |label| fake.state(label),
                |path| fake.bootstrap(path),
                |label| fake.bootout(label),
            )
            .unwrap();

        assert!(identity_restored);
        assert_eq!(fs::read(&plist).unwrap(), bytes);
        assert!(fake.loaded.get());
        assert!(fence_owned(&paths));
        let _ = fs::remove_dir_all(root);
    }
}
