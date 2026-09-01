//! Production Link cutover execute transaction (PREPARE / ACTIVATE / VERIFY / ROLLBACK).
//!
//! Mutates **only** `dev.herdr-mcp.link-prod`. Never touches `dev.herdr-mcp.link`
//! or `dev.herdr-mcp.link-rust-candidate`. Never uses inferred launchd
//! submission jobs. Never
//! flips `production_ready`. Credentials stay in Keychain / server plist env —
//! this module never prints or embeds secret values.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use plist::{Dictionary, Value as PlistValue};
use serde_json::{Value, json};

use super::cutover::{Precondition, prod_plist_backup_path};
use super::install::{
    LINK_RUST_CANDIDATE_LABEL, assert_safe_candidate_program, candidate_program_arguments,
    configured_edge_device_identity, configured_edge_ws_url, inherited_proxy_env,
};
use super::ownership::{
    LINK_LABEL, LINK_PROD_LABEL, LinkAgentView, LinkImplementation, classify_program_arguments,
    program_points_at_managed_runtime, program_points_at_repo_checkout,
};

#[cfg(target_os = "macos")]
const LAUNCHD_BOOTOUT_BUDGET: Duration = Duration::from_secs(8);
#[cfg(target_os = "macos")]
const LAUNCHD_ABSENT_BUDGET: Duration = Duration::from_secs(4);
#[cfg(target_os = "macos")]
const BOOTSTRAP_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(200),
    Duration::from_millis(500),
    Duration::from_millis(1000),
];

/// Launchd operations required by the cutover transaction.
pub trait LaunchdOps {
    fn bootout_prod(&self, label: &str) -> Result<(), String>;
    fn bootstrap_prod(&self, plist: &Path, label: &str) -> Result<(), String>;
    /// Restart an already-loaded production Link without changing launchd
    /// ownership. This is a bounded recovery primitive for a Link process that
    /// failed to consume a generation-control revision; callers must prove
    /// ownership before invoking it.
    fn kickstart_prod(&self, label: &str) -> Result<(), String>;
    /// Probe whether `label` is loaded. Probe failures must be `Err` (never
    /// silently treated as absent).
    fn is_loaded(&self, label: &str) -> Result<bool, String>;
}

/// Real macOS launchctl backend. Refuses any label other than link-prod.
#[derive(Debug, Default)]
pub struct RealLaunchd;

impl LaunchdOps for RealLaunchd {
    fn bootout_prod(&self, label: &str) -> Result<(), String> {
        assert_prod_only_label(label)?;
        #[cfg(target_os = "macos")]
        {
            if !launchd_probe_loaded(label)? {
                return Ok(());
            }
            run_launchctl([
                OsStr::new("bootout"),
                OsStr::new(&format!("{}/{}", launchd_domain(), label)),
            ])
            .map(|_| ())
            .or_else(|error| {
                if error.contains("No such process") || error.contains("Could not find") {
                    Ok(())
                } else {
                    Err(error)
                }
            })?;
            wait_launchd_absent(label, LAUNCHD_BOOTOUT_BUDGET)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err("link cutover --execute is macOS-only".to_owned())
        }
    }

    fn bootstrap_prod(&self, plist: &Path, label: &str) -> Result<(), String> {
        assert_prod_only_label(label)?;
        #[cfg(target_os = "macos")]
        {
            let mut last_error = "launchctl bootstrap did not run".to_owned();
            for attempt in 0..=BOOTSTRAP_RETRY_DELAYS.len() {
                let _ = wait_launchd_absent(label, LAUNCHD_ABSENT_BUDGET);
                match run_launchctl([
                    OsStr::new("bootstrap"),
                    OsStr::new(&launchd_domain()),
                    plist.as_os_str(),
                ]) {
                    Ok(_) => return Ok(()),
                    Err(error) => last_error = error,
                }
                if let Some(delay) = BOOTSTRAP_RETRY_DELAYS.get(attempt).copied() {
                    std::thread::sleep(delay);
                }
            }
            Err(format!(
                "launchctl bootstrap failed after {} attempts: {last_error}",
                BOOTSTRAP_RETRY_DELAYS.len() + 1
            ))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = plist;
            Err("link cutover --execute is macOS-only".to_owned())
        }
    }

    fn kickstart_prod(&self, label: &str) -> Result<(), String> {
        assert_prod_only_label(label)?;
        #[cfg(target_os = "macos")]
        {
            if !launchd_probe_loaded(label)? {
                return Err(format!("refusing to kickstart unloaded {label}"));
            }
            run_launchctl([
                OsStr::new("kickstart"),
                OsStr::new("-k"),
                OsStr::new(&format!("{}/{}", launchd_domain(), label)),
            ])
            .map(|_| ())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = label;
            Err("link cutover --execute is macOS-only".to_owned())
        }
    }

    fn is_loaded(&self, label: &str) -> Result<bool, String> {
        #[cfg(target_os = "macos")]
        {
            launchd_probe_loaded(label)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = label;
            Err("link cutover --execute is macOS-only".to_owned())
        }
    }
}

/// In-memory launchd for synthetic UAT / unit tests.
#[derive(Debug, Default, Clone)]
pub struct FakeLaunchd {
    inner: Arc<Mutex<FakeLaunchdState>>,
}

#[derive(Debug, Default)]
struct FakeLaunchdState {
    loaded: BTreeMap<String, PathBuf>,
    bootouts: Vec<String>,
    bootstraps: Vec<(String, PathBuf)>,
    kickstarts: Vec<String>,
    fail_bootstrap_once: bool,
}

impl FakeLaunchd {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_loaded(label: &str, plist: &Path) -> Self {
        let fake = Self::new();
        fake.inner
            .lock()
            .unwrap()
            .loaded
            .insert(label.to_owned(), plist.to_path_buf());
        fake
    }

    pub fn fail_next_bootstrap(&self) {
        self.inner.lock().unwrap().fail_bootstrap_once = true;
    }

    pub fn bootouts(&self) -> Vec<String> {
        self.inner.lock().unwrap().bootouts.clone()
    }

    pub fn bootstraps(&self) -> Vec<(String, PathBuf)> {
        self.inner.lock().unwrap().bootstraps.clone()
    }

    pub fn kickstarts(&self) -> Vec<String> {
        self.inner.lock().unwrap().kickstarts.clone()
    }
}

impl LaunchdOps for FakeLaunchd {
    fn bootout_prod(&self, label: &str) -> Result<(), String> {
        assert_prod_only_label(label)?;
        let mut state = self.inner.lock().unwrap();
        state.bootouts.push(label.to_owned());
        state.loaded.remove(label);
        Ok(())
    }

    fn bootstrap_prod(&self, plist: &Path, label: &str) -> Result<(), String> {
        assert_prod_only_label(label)?;
        let mut state = self.inner.lock().unwrap();
        if state.fail_bootstrap_once {
            state.fail_bootstrap_once = false;
            return Err("synthetic bootstrap failure".to_owned());
        }
        state
            .bootstraps
            .push((label.to_owned(), plist.to_path_buf()));
        state.loaded.insert(label.to_owned(), plist.to_path_buf());
        Ok(())
    }

    fn kickstart_prod(&self, label: &str) -> Result<(), String> {
        assert_prod_only_label(label)?;
        let mut state = self.inner.lock().unwrap();
        if !state.loaded.contains_key(label) {
            return Err(format!("refusing to kickstart unloaded {label}"));
        }
        state.kickstarts.push(label.to_owned());
        Ok(())
    }

    fn is_loaded(&self, label: &str) -> Result<bool, String> {
        Ok(self.inner.lock().unwrap().loaded.contains_key(label))
    }
}

fn assert_prod_only_label(label: &str) -> Result<(), String> {
    if label != LINK_PROD_LABEL {
        return Err(format!(
            "cutover execute may only mutate {LINK_PROD_LABEL} (refused {label})"
        ));
    }
    if label == LINK_LABEL || label == LINK_RUST_CANDIDATE_LABEL {
        return Err(format!("refusing protected/candidate label {label}"));
    }
    Ok(())
}

/// Technical preconditions that must pass before activate (excludes dual UAT seal).
pub fn technical_preconditions_ready(preconditions: &[Precondition]) -> bool {
    preconditions
        .iter()
        .filter(|item| item.id != "dual_verification_uat_recorded")
        .all(|item| item.ok)
}

/// Run the guarded cutover transaction against the provided launchd backend.
pub fn execute_transaction<L: LaunchdOps>(
    home: &Path,
    prod: &LinkAgentView,
    launchd: &L,
) -> Result<Value, String> {
    let phase = "PREPARE";
    let backup_path = prod_plist_backup_path(home);
    let prod_plist = prod.plist_path.clone();

    if prod.implementation != LinkImplementation::Node {
        return Ok(failure_report(
            phase,
            false,
            &backup_path,
            None,
            format!(
                "cutover execute requires Node link-prod source (got {})",
                prod.implementation.as_str()
            ),
            &[],
        ));
    }

    let prepared = match prepare_cutover(home, &prod_plist, &backup_path) {
        Ok(value) => value,
        Err(error) => {
            return Ok(failure_report(phase, false, &backup_path, None, error, &[]));
        }
    };

    if let Err(error) = activate_cutover(launchd, &prod_plist, &prepared.rust_plist_bytes) {
        let rollback = rollback_cutover(launchd, &prod_plist, &backup_path);
        return Ok(failure_report(
            "ROLLBACK",
            true,
            &backup_path,
            Some(rollback),
            error,
            &prepared.program,
        ));
    }

    match verify_cutover(home, &prod_plist, launchd) {
        Ok(verified) => Ok(json!({
            "ok": true,
            "mode": "execute",
            "cutover_performed": true,
            "execute_implemented": true,
            "phase": "VERIFY",
            "production_ready": false,
            "production_ready_note": "dual verification UAT + seal remain operator-gated; never auto-flipped",
            "backup_plist": backup_path.display().to_string(),
            "prod_plist": prod_plist.display().to_string(),
            "program_arguments": prepared.program,
            "preserved_env_keys": prepared.preserved_env_keys,
            "verify": verified,
            "protected_labels_untouched": [LINK_LABEL, LINK_RUST_CANDIDATE_LABEL],
            "notes": [
                "PREPARE/ACTIVATE/VERIFY completed for link-prod only.",
                "Node canary link and rust candidate LaunchAgents were not mutated.",
                "production_ready remains false until independent dual UAT + seal.",
            ],
        })),
        Err(error) => {
            let rollback = rollback_cutover(launchd, &prod_plist, &backup_path);
            Ok(failure_report(
                "ROLLBACK",
                true,
                &backup_path,
                Some(rollback),
                error,
                &prepared.program,
            ))
        }
    }
}

#[derive(Debug)]
struct PreparedCutover {
    program: Vec<String>,
    rust_plist_bytes: Vec<u8>,
    preserved_env_keys: Vec<String>,
}

fn prepare_cutover(
    home: &Path,
    prod_plist: &Path,
    backup_path: &Path,
) -> Result<PreparedCutover, String> {
    if !prod_plist.is_file() {
        return Err(format!("prod plist missing: {}", prod_plist.display()));
    }
    let original = fs::read(prod_plist)
        .map_err(|error| format!("cannot read {}: {error}", prod_plist.display()))?;
    refuse_if_not_prod_label(&original, prod_plist)?;
    require_node_program_arguments(
        &original,
        &format!("current prod plist {}", prod_plist.display()),
    )?;

    if let Some(parent) = backup_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    // Authoritative Node rollback source must never be overwritten by a re-entry
    // that already sees Rust (or corrupt) prod bytes on disk.
    if backup_path.is_file() {
        let existing = fs::read(backup_path).map_err(|error| {
            format!(
                "cannot read existing backup {}: {error}",
                backup_path.display()
            )
        })?;
        refuse_if_not_prod_label(&existing, backup_path)?;
        require_node_program_arguments(
            &existing,
            &format!("existing Node rollback backup {}", backup_path.display()),
        )?;
    } else {
        atomic_write(backup_path, &original, 0o600)?;
    }

    let program = candidate_program_arguments(home)?;
    assert_safe_candidate_program(home, &program)?;
    let (bytes, env_keys) = encode_prod_rust_plist(home, &original, &program)?;
    Ok(PreparedCutover {
        program,
        rust_plist_bytes: bytes,
        preserved_env_keys: env_keys,
    })
}

fn activate_cutover<L: LaunchdOps>(
    launchd: &L,
    prod_plist: &Path,
    rust_plist_bytes: &[u8],
) -> Result<(), String> {
    atomic_write(prod_plist, rust_plist_bytes, 0o600)?;
    launchd.bootout_prod(LINK_PROD_LABEL)?;
    launchd.bootstrap_prod(prod_plist, LINK_PROD_LABEL)?;
    Ok(())
}

fn verify_cutover<L: LaunchdOps>(
    home: &Path,
    prod_plist: &Path,
    launchd: &L,
) -> Result<Value, String> {
    let bytes = fs::read(prod_plist)
        .map_err(|error| format!("cannot re-read {}: {error}", prod_plist.display()))?;
    let program = read_program_arguments(&bytes)?;
    assert_safe_candidate_program(home, &program)?;
    if !program_points_at_managed_runtime(&program, home) {
        return Err(
            "verify failed: prod ProgramArguments do not point at runtime/current".to_owned(),
        );
    }
    if program_points_at_repo_checkout(&program) {
        return Err("verify failed: prod ProgramArguments still point at a checkout".to_owned());
    }
    if !launchd.is_loaded(LINK_PROD_LABEL)? {
        return Err(format!(
            "verify failed: {LINK_PROD_LABEL} is not loaded after bootstrap"
        ));
    }
    Ok(json!({
        "label": LINK_PROD_LABEL,
        "loaded": true,
        "implementation": LinkImplementation::Rust.as_str(),
        "program_arguments": program,
        "points_at_managed_runtime": true,
        "points_at_repo_checkout": false,
    }))
}

pub fn rollback_cutover<L: LaunchdOps>(
    launchd: &L,
    prod_plist: &Path,
    backup_path: &Path,
) -> Value {
    let mut steps = Vec::new();
    match fs::read(backup_path) {
        Ok(bytes) => match atomic_write(prod_plist, &bytes, 0o600) {
            Ok(()) => steps.push(json!({"step": "restore_plist", "ok": true})),
            Err(error) => {
                steps.push(json!({"step": "restore_plist", "ok": false, "error": error}));
                return json!({"ok": false, "steps": steps});
            }
        },
        Err(error) => {
            steps.push(json!({
                "step": "restore_plist",
                "ok": false,
                "error": format!("cannot read backup {}: {error}", backup_path.display()),
            }));
            return json!({"ok": false, "steps": steps});
        }
    }

    match launchd.bootout_prod(LINK_PROD_LABEL) {
        Ok(()) => steps.push(json!({"step": "bootout", "ok": true})),
        Err(error) => {
            steps.push(json!({"step": "bootout", "ok": false, "error": error}));
            return json!({"ok": false, "steps": steps});
        }
    }
    match launchd.bootstrap_prod(prod_plist, LINK_PROD_LABEL) {
        Ok(()) => {
            steps.push(json!({"step": "bootstrap", "ok": true}));
            json!({"ok": true, "steps": steps})
        }
        Err(error) => {
            steps.push(json!({"step": "bootstrap", "ok": false, "error": error}));
            json!({"ok": false, "steps": steps})
        }
    }
}

fn failure_report(
    phase: &str,
    cutover_attempted: bool,
    backup_path: &Path,
    rollback: Option<Value>,
    error: String,
    program: &[String],
) -> Value {
    json!({
        "ok": false,
        "mode": "execute",
        "cutover_performed": false,
        "cutover_attempted": cutover_attempted,
        "execute_implemented": true,
        "phase": phase,
        "production_ready": false,
        "error": error,
        "backup_plist": backup_path.display().to_string(),
        "program_arguments": program,
        "rollback": rollback,
        "protected_labels_untouched": [LINK_LABEL, LINK_RUST_CANDIDATE_LABEL],
    })
}

/// Rewrite Node prod plist bytes to Rust `runtime/current link run`, preserving
/// non-secret EnvironmentVariables (including Keychain *service name*).
pub fn encode_prod_rust_plist(
    home: &Path,
    original_plist_bytes: &[u8],
    program: &[String],
) -> Result<(Vec<u8>, Vec<String>), String> {
    assert_safe_candidate_program(home, program)?;
    let value = PlistValue::from_reader(std::io::Cursor::new(original_plist_bytes))
        .map_err(|error| format!("cannot parse prod plist: {error}"))?;
    let dict = value
        .as_dictionary()
        .ok_or_else(|| "prod plist root must be a dict".to_owned())?;

    let label = dict
        .get("Label")
        .and_then(PlistValue::as_string)
        .unwrap_or("");
    if label != LINK_PROD_LABEL {
        return Err(format!(
            "refusing to rewrite plist with Label={label} (expected {LINK_PROD_LABEL})"
        ));
    }

    let mut env_keys = Vec::new();
    let mut env_out = Dictionary::new();
    if let Some(PlistValue::Dictionary(env)) = dict.get("EnvironmentVariables") {
        for (key, value) in env {
            let upper = key.to_ascii_uppercase();
            // Never copy raw secret *values* that look like embedded tokens.
            // Keychain *service names* (HERDR_LINK_KEYCHAIN_SERVICE) are kept.
            if (upper.contains("TOKEN") || upper.contains("PASSWORD") || upper.ends_with("_SECRET"))
                && upper != "HERDR_LINK_KEYCHAIN_SERVICE"
            {
                continue;
            }
            env_out.insert(key.clone(), value.clone());
            env_keys.push(key.clone());
        }
    }
    if let Some(edge_url) = configured_edge_ws_url(home) {
        env_out.insert("HERDR_EDGE_URL".to_owned(), PlistValue::String(edge_url));
        if !env_keys.iter().any(|key| key == "HERDR_EDGE_URL") {
            env_keys.push("HERDR_EDGE_URL".to_owned());
        }
    }
    if let Some((device_id, keychain_service)) = configured_edge_device_identity(home) {
        env_out.insert(
            "HERDR_WORKSTATION_ID".to_owned(),
            PlistValue::String(device_id),
        );
        env_out.insert(
            "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
            PlistValue::String(keychain_service),
        );
        for key in ["HERDR_WORKSTATION_ID", "HERDR_LINK_KEYCHAIN_SERVICE"] {
            if !env_keys.iter().any(|existing| existing == key) {
                env_keys.push(key.to_owned());
            }
        }
    }
    for (key, value) in inherited_proxy_env() {
        if !env_out.contains_key(&key) {
            env_out.insert(key.clone(), PlistValue::String(value));
            env_keys.push(key);
        }
    }
    env_keys.sort();
    env_keys.dedup();

    // Point generation label at the managed rust generation when available.
    if let Ok(target) = fs::read_link(
        home.join(".config")
            .join("herdr-mcp")
            .join("runtime")
            .join("current"),
    ) && let Some(name) = target.file_name().and_then(OsStr::to_str)
        && name.starts_with("rust-")
    {
        env_out.insert(
            "HERDR_RUNTIME_GENERATION".to_owned(),
            PlistValue::String(name.to_owned()),
        );
        if !env_keys.iter().any(|key| key == "HERDR_RUNTIME_GENERATION") {
            env_keys.push("HERDR_RUNTIME_GENERATION".to_owned());
            env_keys.sort();
        }
    }

    let mut root = Dictionary::new();
    root.insert(
        "Label".to_owned(),
        PlistValue::String(LINK_PROD_LABEL.to_owned()),
    );
    root.insert(
        "ProgramArguments".to_owned(),
        PlistValue::Array(
            program
                .iter()
                .map(|value| PlistValue::String(value.clone()))
                .collect(),
        ),
    );
    root.insert(
        "WorkingDirectory".to_owned(),
        PlistValue::String(
            home.join(".config")
                .join("herdr-mcp")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    root.insert(
        "EnvironmentVariables".to_owned(),
        PlistValue::Dictionary(env_out),
    );
    root.insert("RunAtLoad".to_owned(), PlistValue::Boolean(true));
    let mut keep_alive = Dictionary::new();
    keep_alive.insert("SuccessfulExit".to_owned(), PlistValue::Boolean(false));
    root.insert("KeepAlive".to_owned(), PlistValue::Dictionary(keep_alive));
    root.insert(
        "ThrottleInterval".to_owned(),
        PlistValue::Integer(10.into()),
    );
    root.insert(
        "ProcessType".to_owned(),
        PlistValue::String("Background".to_owned()),
    );
    let config_dir = home.join(".config").join("herdr-mcp");
    root.insert(
        "StandardOutPath".to_owned(),
        PlistValue::String(
            config_dir
                .join("link-prod.launchd.out.log")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    root.insert(
        "StandardErrorPath".to_owned(),
        PlistValue::String(
            config_dir
                .join("link-prod.launchd.err.log")
                .to_string_lossy()
                .into_owned(),
        ),
    );

    let mut bytes = Vec::new();
    PlistValue::Dictionary(root)
        .to_writer_xml(&mut bytes)
        .map_err(|error| format!("cannot encode rust prod link plist: {error}"))?;

    let xml = String::from_utf8_lossy(&bytes);
    if xml.contains(&format!("<string>{LINK_LABEL}</string>"))
        || xml.contains(&format!("<string>{LINK_RUST_CANDIDATE_LABEL}</string>"))
    {
        return Err("rust prod plist unexpectedly references non-prod labels".to_owned());
    }
    if xml.contains("/Documents/herdr-mcp/dist/link/") || xml.contains("/target/") {
        return Err("rust prod plist unexpectedly references checkout/target paths".to_owned());
    }
    Ok((bytes, env_keys))
}

fn require_node_program_arguments(bytes: &[u8], context: &str) -> Result<Vec<String>, String> {
    let program = read_program_arguments(bytes)?;
    match classify_program_arguments(&program) {
        LinkImplementation::Node => Ok(program),
        other => Err(format!(
            "{context}: expected Node link-prod ProgramArguments, got {}",
            other.as_str()
        )),
    }
}

fn refuse_if_not_prod_label(bytes: &[u8], path: &Path) -> Result<(), String> {
    let value = PlistValue::from_reader(std::io::Cursor::new(bytes))
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    let label = value
        .as_dictionary()
        .and_then(|dict| dict.get("Label"))
        .and_then(PlistValue::as_string)
        .unwrap_or("");
    if label != LINK_PROD_LABEL {
        return Err(format!(
            "refusing cutover on {} with Label={label}",
            path.display()
        ));
    }
    Ok(())
}

fn read_program_arguments(bytes: &[u8]) -> Result<Vec<String>, String> {
    let value = PlistValue::from_reader(std::io::Cursor::new(bytes))
        .map_err(|error| format!("cannot parse plist: {error}"))?;
    let args = value
        .as_dictionary()
        .and_then(|dict| dict.get("ProgramArguments"))
        .and_then(PlistValue::as_array)
        .ok_or_else(|| "ProgramArguments missing".to_owned())?;
    Ok(args
        .iter()
        .filter_map(PlistValue::as_string)
        .map(str::to_owned)
        .collect())
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(format!("{} must not be a symlink", path.display()));
    }
    let temp = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("herdr-mcp-link-prod"),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    {
        let mut file = fs::File::create(&temp)
            .map_err(|error| format!("cannot create {}: {error}", temp.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("cannot write {}: {error}", temp.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync {}: {error}", temp.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))
            .map_err(|error| format!("cannot chmod {}: {error}", temp.display()))?;
    }
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!(
            "cannot replace {} with {}: {error}",
            path.display(),
            temp.display()
        )
    })?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn launchd_domain() -> String {
    format!("gui/{}", users_uid())
}

#[cfg(target_os = "macos")]
fn users_uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(target_os = "macos")]
fn launchd_probe_loaded(label: &str) -> Result<bool, String> {
    let target = format!("{}/{}", launchd_domain(), label);
    let output = std::process::Command::new("/bin/launchctl")
        .args(["print", &target])
        .output()
        .map_err(|error| format!("cannot execute launchctl print {target}: {error}"))?;
    if output.status.success() {
        return Ok(true);
    }
    let detail = format!(
        "{}
{}",
        String::from_utf8_lossy(&output.stderr).trim(),
        String::from_utf8_lossy(&output.stdout).trim()
    );
    let lower = detail.to_ascii_lowercase();
    if lower.contains("could not find")
        || lower.contains("not found")
        || lower.contains("no such process")
        || lower.contains("no such service")
    {
        return Ok(false);
    }
    Err(format!(
        "launchctl print {target} failed (refusing to treat as absent): {}",
        detail.trim()
    ))
}

#[cfg(target_os = "macos")]
fn wait_launchd_absent(label: &str, budget: Duration) -> Result<(), String> {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if !launchd_probe_loaded(label)? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if launchd_probe_loaded(label)? {
        Err(format!(
            "{label} still loaded after bootout budget {}ms",
            budget.as_millis()
        ))
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn run_launchctl<I, S>(args: I) -> Result<std::process::Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    // Hard refuse inferred submission jobs for cutover mutations.
    let collected: Vec<std::ffi::OsString> = args
        .into_iter()
        .map(|value| value.as_ref().to_owned())
        .collect();
    if collected
        .first()
        .is_some_and(|value| value == "submit" || value == "load" || value == "unload")
    {
        return Err(
            "cutover execute refuses inferred launchd submission and legacy load/unload paths"
                .to_owned(),
        );
    }
    let output = std::process::Command::new("/bin/launchctl")
        .args(&collected)
        .output()
        .map_err(|error| format!("cannot execute launchctl: {error}"))?;
    if output.status.success() {
        return Ok(output);
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(format!("launchctl failed: {detail}"))
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
            "herdr-mcp-cutover-exec-{}-{}-{}",
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
        let generation = runtime.join("generations/rust-testhashexecute01");
        fs::create_dir_all(&generation).unwrap();
        let binary = generation.join("herdr-mcp");
        fs::write(&binary, b"#!/bin/sh\necho fake\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        }
        symlink(
            "generations/rust-testhashexecute01",
            runtime.join("current"),
        )
        .unwrap();
    }

    fn node_prod_plist_xml(home: &Path) -> Vec<u8> {
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LINK_PROD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/node</string>
    <string>/Users/qingxian/Documents/herdr-mcp/dist/link/macos-daemon.js</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HERDR_EDGE_URL</key>
    <string>wss://herdr-edge-prod.example/ws</string>
    <key>HERDR_WORKSTATION_ID</key>
    <string>prod-real-runtime</string>
    <key>HERDR_LINK_KEYCHAIN_SERVICE</key>
    <string>herdr-edge-prod-link-secret</string>
    <key>HERDR_MCP_TOKEN</key>
    <string>should-be-stripped</string>
    <key>HERDR_RUNTIME_CONTROL_PATH</key>
    <string>{}/runtime-control-prod.json</string>
  </dict>
</dict>
</plist>
"#,
            home.join(".config/herdr-mcp").display()
        );
        xml.into_bytes()
    }

    #[test]
    fn encode_preserves_keychain_service_and_strips_embedded_token() {
        let home = test_home();
        setup_managed_runtime(&home);
        let config_dir = home.join(".config/herdr-mcp");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.toml"),
            "[edge]\npublic_origin = \"https://herdr-mcp.agentforme.cc.cd\"\n",
        )
        .unwrap();
        let program = candidate_program_arguments(&home).unwrap();
        let (bytes, keys) =
            encode_prod_rust_plist(&home, &node_prod_plist_xml(&home), &program).unwrap();
        let xml = String::from_utf8_lossy(&bytes);
        assert!(xml.contains("runtime/current/herdr-mcp"));
        assert!(xml.contains("herdr-edge-prod-link-secret"));
        assert!(xml.contains("wss://herdr-mcp.agentforme.cc.cd/ws"));
        assert!(!xml.contains("wss://herdr-edge-prod.example/ws"));
        assert!(!xml.contains("should-be-stripped"));
        assert!(keys.contains(&"HERDR_LINK_KEYCHAIN_SERVICE".to_owned()));
        assert!(!keys.iter().any(|key| key == "HERDR_MCP_TOKEN"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn synthetic_uat_activate_verify_succeeds_with_fake_launchd() {
        let home = test_home();
        setup_managed_runtime(&home);
        let agents = home.join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        let prod_plist = agents.join(format!("{LINK_PROD_LABEL}.plist"));
        fs::write(&prod_plist, node_prod_plist_xml(&home)).unwrap();

        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, &prod_plist);
        let prod = LinkAgentView {
            label: LINK_PROD_LABEL.to_owned(),
            plist_path: prod_plist.clone(),
            present: true,
            loaded: true,
            implementation: LinkImplementation::Node,
            program_arguments: vec![
                "/usr/local/bin/node".to_owned(),
                "/Users/qingxian/Documents/herdr-mcp/dist/link/macos-daemon.js".to_owned(),
            ],
            edge_url: Some("wss://herdr-edge-prod.example/ws".to_owned()),
            workstation_id: Some("prod-real-runtime".to_owned()),
            runtime_generation: Some("stable-0.3.32".to_owned()),
            control_path: None,
            status_path: None,
        };

        let report = execute_transaction(&home, &prod, &launchd).unwrap();
        assert_eq!(report["ok"], true);
        assert_eq!(report["cutover_performed"], true);
        assert_eq!(report["production_ready"], false);
        assert_eq!(report["phase"], "VERIFY");
        assert_eq!(launchd.bootouts(), vec![LINK_PROD_LABEL.to_owned()]);
        assert_eq!(launchd.bootstraps().len(), 1);
        assert_eq!(launchd.bootstraps()[0].0, LINK_PROD_LABEL);

        let rewritten = fs::read_to_string(&prod_plist).unwrap();
        assert!(rewritten.contains("runtime/current/herdr-mcp"));
        assert!(!rewritten.contains("dist/link/macos-daemon.js"));
        assert!(prod_plist_backup_path(&home).is_file());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn synthetic_uat_rolls_back_when_bootstrap_fails() {
        let home = test_home();
        setup_managed_runtime(&home);
        let agents = home.join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        let prod_plist = agents.join(format!("{LINK_PROD_LABEL}.plist"));
        let original = node_prod_plist_xml(&home);
        fs::write(&prod_plist, &original).unwrap();

        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, &prod_plist);
        launchd.fail_next_bootstrap();
        let prod = LinkAgentView {
            label: LINK_PROD_LABEL.to_owned(),
            plist_path: prod_plist.clone(),
            present: true,
            loaded: true,
            implementation: LinkImplementation::Node,
            program_arguments: vec![
                "/usr/local/bin/node".to_owned(),
                "/Users/qingxian/Documents/herdr-mcp/dist/link/macos-daemon.js".to_owned(),
            ],
            edge_url: None,
            workstation_id: None,
            runtime_generation: None,
            control_path: None,
            status_path: None,
        };

        let report = execute_transaction(&home, &prod, &launchd).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(report["cutover_performed"], false);
        assert_eq!(report["phase"], "ROLLBACK");
        assert_eq!(report["rollback"]["ok"], true);
        let restored = fs::read(&prod_plist).unwrap();
        assert!(
            String::from_utf8_lossy(&restored).contains("dist/link/macos-daemon.js"),
            "rollback must restore Node prod plist bytes"
        );
        // bootout for activate + bootout for rollback; bootstrap only for rollback success path
        assert!(launchd.bootouts().len() >= 2);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn refuses_non_prod_launchd_labels() {
        let fake = FakeLaunchd::new();
        assert!(fake.bootout_prod(LINK_LABEL).is_err());
        assert!(fake.bootout_prod(LINK_RUST_CANDIDATE_LABEL).is_err());
    }

    #[test]
    fn technical_ready_ignores_dual_uat_seal_blocker() {
        let items = vec![
            Precondition {
                id: "runtime_current_managed".to_owned(),
                ok: true,
                detail: "ok".to_owned(),
            },
            Precondition {
                id: "dual_verification_uat_recorded".to_owned(),
                ok: false,
                detail: "seal later".to_owned(),
            },
        ];
        assert!(technical_preconditions_ready(&items));
        let blocked = vec![Precondition {
            id: "candidate_healthy".to_owned(),
            ok: false,
            detail: "missing".to_owned(),
        }];
        assert!(!technical_preconditions_ready(&blocked));
    }

    #[test]
    fn prepare_refuses_rust_prod_and_keeps_existing_node_backup() {
        let home = test_home();
        setup_managed_runtime(&home);
        let agents = home.join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        let prod_plist = agents.join(format!("{LINK_PROD_LABEL}.plist"));
        let node_bytes = node_prod_plist_xml(&home);
        let backup = prod_plist_backup_path(&home);
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::write(&backup, &node_bytes).unwrap();

        let rust_program = candidate_program_arguments(&home).unwrap();
        let (rust_bytes, _) = encode_prod_rust_plist(&home, &node_bytes, &rust_program).unwrap();
        fs::write(&prod_plist, &rust_bytes).unwrap();

        let err = prepare_cutover(&home, &prod_plist, &backup).unwrap_err();
        assert!(
            err.contains("expected Node"),
            "prepare must refuse Rust prod: {err}"
        );
        let preserved = fs::read(&backup).unwrap();
        assert_eq!(
            preserved, node_bytes,
            "existing Node backup must not be overwritten"
        );

        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, &prod_plist);
        let prod = LinkAgentView {
            label: LINK_PROD_LABEL.to_owned(),
            plist_path: prod_plist,
            present: true,
            loaded: true,
            implementation: LinkImplementation::Rust,
            program_arguments: rust_program,
            edge_url: None,
            workstation_id: None,
            runtime_generation: None,
            control_path: None,
            status_path: None,
        };
        let report = execute_transaction(&home, &prod, &launchd).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(report["cutover_performed"], false);
        assert_eq!(report["phase"], "PREPARE");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn prepare_reuses_existing_node_backup_without_overwrite() {
        let home = test_home();
        setup_managed_runtime(&home);
        let agents = home.join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        let prod_plist = agents.join(format!("{LINK_PROD_LABEL}.plist"));
        let original = node_prod_plist_xml(&home);
        fs::write(&prod_plist, &original).unwrap();
        let backup = prod_plist_backup_path(&home);
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        let marker_backup = {
            // Distinct but still Node: tweak Keychain service name.
            let xml = String::from_utf8(original.clone()).unwrap().replace(
                "herdr-edge-prod-link-secret",
                "herdr-edge-prod-link-secret-preserved",
            );
            xml.into_bytes()
        };
        fs::write(&backup, &marker_backup).unwrap();

        let prepared = prepare_cutover(&home, &prod_plist, &backup).unwrap();
        assert!(!prepared.program.is_empty());
        let preserved = fs::read(&backup).unwrap();
        assert_eq!(
            preserved, marker_backup,
            "prepare must reuse existing Node backup bytes"
        );
        let _ = fs::remove_dir_all(&home);
    }
}
