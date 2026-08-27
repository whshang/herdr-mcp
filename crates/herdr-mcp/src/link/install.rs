//! Candidate LaunchAgent install/uninstall for Rust Link soak.
//!
//! Writes and bootstraps **only** `dev.herdr-mcp.link-rust-candidate`, with
//! ProgramArguments pointing at `~/.config/herdr-mcp/runtime/current/herdr-mcp
//! link run`. Never mutates `dev.herdr-mcp.link` or `dev.herdr-mcp.link-prod`,
//! never schedules inferred launchd submission jobs, and never points launchd at a checkout or
//! `target/` binary.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
#[cfg(target_os = "macos")]
use std::process::{Command, Output};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use plist::{Dictionary, Value as PlistValue};
#[cfg(target_os = "macos")]
use serde_json::{Value, json};

use super::ownership::{LINK_LABEL, LINK_PROD_LABEL};
use super::run::{MACOS_DEFAULT_EDGE_URL, MACOS_LINK_KEYCHAIN_SERVICE};

/// Candidate-only LaunchAgent label. Distinct from live Node `link` / `link-prod`.
pub const LINK_RUST_CANDIDATE_LABEL: &str = "dev.herdr-mcp.link-rust-candidate";

/// Default canary workstation id for the Rust candidate (avoids colliding with
/// live Node `dev-real-runtime` and prod `prod-real-runtime`).
pub const CANDIDATE_WORKSTATION_ID: &str = "dev-rust-link-candidate";

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

/// Labels this module must never write, bootout, or replace.
pub fn protected_live_link_labels() -> &'static [&'static str] {
    &[LINK_LABEL, LINK_PROD_LABEL]
}

/// CLI entry: install the Rust Link candidate LaunchAgent.
pub fn install() -> Result<ExitCode, String> {
    #[cfg(not(target_os = "macos"))]
    {
        Err("herdr-mcp link install is macOS-only (LaunchAgent candidate soak)".to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        let home = home_dir().ok_or_else(|| "HOME is required for link install".to_owned())?;
        let report = install_candidate(&home)?;
        println!("{report}");
        Ok(ExitCode::SUCCESS)
    }
}

/// CLI entry: uninstall the Rust Link candidate LaunchAgent only.
pub fn uninstall() -> Result<ExitCode, String> {
    #[cfg(not(target_os = "macos"))]
    {
        Err("herdr-mcp link uninstall is macOS-only (LaunchAgent candidate soak)".to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        let home = home_dir().ok_or_else(|| "HOME is required for link uninstall".to_owned())?;
        let report = uninstall_candidate(&home)?;
        println!("{report}");
        Ok(ExitCode::SUCCESS)
    }
}

/// Resolve and validate the managed `runtime/current/herdr-mcp` binary path.
pub fn resolve_managed_runtime_binary(home: &Path) -> Result<PathBuf, String> {
    let current_link = runtime_current_link(home);
    let binary = managed_runtime_binary(home);

    match fs::symlink_metadata(&current_link) {
        Ok(meta) if meta.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "runtime/current must be a managed symlink: {}",
                current_link.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "runtime/current is missing; install an active generation first: {}",
                current_link.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "cannot inspect runtime/current {}: {error}",
                current_link.display()
            ));
        }
    }

    let target = fs::read_link(&current_link).map_err(|error| {
        format!(
            "cannot read runtime/current symlink {}: {error}",
            current_link.display()
        )
    })?;
    if !is_owned_generation_target(&target) {
        return Err(format!(
            "runtime/current must point at generations/rust-*: got {}",
            target.display()
        ));
    }

    let resolved = if target.is_absolute() {
        target.join("herdr-mcp")
    } else {
        current_link
            .parent()
            .ok_or_else(|| "runtime/current has no parent".to_owned())?
            .join(&target)
            .join("herdr-mcp")
    };
    refuse_checkout_or_target_path(&resolved)?;
    refuse_checkout_or_target_path(&binary)?;

    if !binary.is_file() && !resolved.is_file() {
        return Err(format!(
            "managed runtime binary missing at {} (resolved {})",
            binary.display(),
            resolved.display()
        ));
    }

    Ok(binary)
}

/// Build ProgramArguments for the candidate LaunchAgent.
pub fn candidate_program_arguments(home: &Path) -> Result<Vec<String>, String> {
    let binary = resolve_managed_runtime_binary(home)?;
    let args = vec![
        binary.to_string_lossy().into_owned(),
        "link".to_owned(),
        "run".to_owned(),
    ];
    assert_safe_candidate_program(home, &args)?;
    Ok(args)
}

/// Fail closed unless argv is managed-runtime `herdr-mcp link run`.
pub fn assert_safe_candidate_program(home: &Path, args: &[String]) -> Result<(), String> {
    if args.len() != 3 {
        return Err(format!(
            "candidate ProgramArguments must be exactly 3 entries (got {})",
            args.len()
        ));
    }
    let expected = managed_runtime_binary(home);
    if Path::new(&args[0]) != expected.as_path() {
        return Err(format!(
            "candidate ProgramArguments[0] must be {} (got {})",
            expected.display(),
            args[0]
        ));
    }
    refuse_checkout_or_target_path(Path::new(&args[0]))?;
    if args[1] != "link" || args[2] != "run" {
        return Err(format!(
            "candidate ProgramArguments must end with 'link run' (got {} {})",
            args[1], args[2]
        ));
    }
    Ok(())
}

/// Encode the candidate LaunchAgent plist XML bytes.
pub fn encode_candidate_plist(
    home: &Path,
    program: &[String],
    env: &BTreeMap<String, String>,
) -> Result<Vec<u8>, String> {
    assert_safe_candidate_program(home, program)?;
    for label in protected_live_link_labels() {
        if *label == LINK_RUST_CANDIDATE_LABEL {
            return Err("internal error: candidate label listed as protected".to_owned());
        }
    }

    let paths = candidate_paths(home);
    let mut root = Dictionary::new();
    root.insert(
        "Label".to_owned(),
        PlistValue::String(LINK_RUST_CANDIDATE_LABEL.to_owned()),
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
        PlistValue::String(paths.config_dir.to_string_lossy().into_owned()),
    );
    let mut env_dict = Dictionary::new();
    for (key, value) in env {
        if key.contains("TOKEN") || key.contains("SECRET") || key.contains("PASSWORD") {
            return Err(format!(
                "refusing to embed credential-like key '{key}' in candidate plist"
            ));
        }
        env_dict.insert(key.clone(), PlistValue::String(value.clone()));
    }
    root.insert(
        "EnvironmentVariables".to_owned(),
        PlistValue::Dictionary(env_dict),
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
    root.insert(
        "StandardOutPath".to_owned(),
        PlistValue::String(paths.stdout_log.to_string_lossy().into_owned()),
    );
    root.insert(
        "StandardErrorPath".to_owned(),
        PlistValue::String(paths.stderr_log.to_string_lossy().into_owned()),
    );

    let mut bytes = Vec::new();
    PlistValue::Dictionary(root)
        .to_writer_xml(&mut bytes)
        .map_err(|error| format!("cannot encode candidate link plist: {error}"))?;

    // Defense in depth: encoded Label must be the candidate only.
    let xml = String::from_utf8_lossy(&bytes);
    let label_needle = format!("<string>{LINK_RUST_CANDIDATE_LABEL}</string>");
    if !xml.contains(&label_needle) {
        return Err("candidate plist missing candidate label".to_owned());
    }
    for label in protected_live_link_labels() {
        let protected_needle = format!("<string>{label}</string>");
        if xml.contains(&protected_needle) {
            return Err(format!(
                "candidate plist unexpectedly references protected label {label}"
            ));
        }
    }
    Ok(bytes)
}

/// Default non-secret env for the candidate LaunchAgent.
pub fn default_candidate_env() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "HERDR_EDGE_URL".to_owned(),
            MACOS_DEFAULT_EDGE_URL.to_owned(),
        ),
        (
            "HERDR_WORKSTATION_ID".to_owned(),
            CANDIDATE_WORKSTATION_ID.to_owned(),
        ),
        (
            "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
            MACOS_LINK_KEYCHAIN_SERVICE.to_owned(),
        ),
        (
            "PATH".to_owned(),
            "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
        ),
    ])
}

pub fn candidate_plist_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{LINK_RUST_CANDIDATE_LABEL}.plist"))
}

pub fn managed_runtime_binary(home: &Path) -> PathBuf {
    home.join(".config")
        .join("herdr-mcp")
        .join("runtime")
        .join("current")
        .join("herdr-mcp")
}

fn runtime_current_link(home: &Path) -> PathBuf {
    home.join(".config")
        .join("herdr-mcp")
        .join("runtime")
        .join("current")
}

fn is_owned_generation_target(target: &Path) -> bool {
    let mut components = target.components();
    matches!(
        components.next(),
        Some(Component::Normal(value)) if value == OsStr::new("generations")
    ) && matches!(
        components.next(),
        Some(Component::Normal(value))
            if value.to_string_lossy().starts_with("rust-")
    ) && components.next().is_none()
}

fn refuse_checkout_or_target_path(path: &Path) -> Result<(), String> {
    let lower = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    if lower.contains("/target/debug/")
        || lower.contains("/target/release/")
        || lower.ends_with("/target/debug/herdr-mcp")
        || lower.ends_with("/target/release/herdr-mcp")
    {
        return Err(format!(
            "refusing Cargo target binary for Link launchd: {}",
            path.display()
        ));
    }
    if (lower.contains("/documents/") || lower.contains("/.herdr/worktrees/"))
        && (lower.contains("/herdr-mcp/") || lower.ends_with("/herdr-mcp"))
        && !lower.contains("/.config/herdr-mcp/runtime/")
    {
        return Err(format!(
            "refusing checkout/worktree path for Link launchd: {}",
            path.display()
        ));
    }
    Ok(())
}

struct CandidatePaths {
    config_dir: PathBuf,
    plist: PathBuf,
    stdout_log: PathBuf,
    stderr_log: PathBuf,
}

fn candidate_paths(home: &Path) -> CandidatePaths {
    let config_dir = home.join(".config").join("herdr-mcp");
    CandidatePaths {
        plist: candidate_plist_path(home),
        stdout_log: config_dir.join("link-rust-candidate.launchd.out.log"),
        stderr_log: config_dir.join("link-rust-candidate.launchd.err.log"),
        config_dir,
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn install_candidate(home: &Path) -> Result<Value, String> {
    assert_not_protected_mutation(LINK_RUST_CANDIDATE_LABEL)?;
    let program = candidate_program_arguments(home)?;
    let env = default_candidate_env();
    let edge_url = env
        .get("HERDR_EDGE_URL")
        .cloned()
        .unwrap_or_else(|| MACOS_DEFAULT_EDGE_URL.to_owned());
    let edge_contract = super::edge_contract::probe_edge_contract_for_rust_link(&edge_url)
        .map_err(|error| format!("link install refused: {error}"))?;
    let bytes = encode_candidate_plist(home, &program, &env)?;
    let paths = candidate_paths(home);
    fs::create_dir_all(&paths.config_dir)
        .map_err(|error| format!("cannot create {}: {error}", paths.config_dir.display()))?;
    atomic_write(&paths.plist, &bytes, 0o600)?;

    // Replace only the candidate job: bootout candidate label if loaded, then
    // bootstrap the new plist. Never touch protected live Node labels.
    bootout_label(LINK_RUST_CANDIDATE_LABEL)?;
    bootstrap_with_retry(&paths.plist, LINK_RUST_CANDIDATE_LABEL)?;

    Ok(json!({
        "ok": true,
        "action": "link_install_candidate",
        "label": LINK_RUST_CANDIDATE_LABEL,
        "plist": paths.plist.display().to_string(),
        "program_arguments": program,
        "edge_url": edge_url,
        "edge_contract_epoch": edge_contract.contract_epoch,
        "edge_contract_hash": edge_contract.contract_hash,
        "edge_service": edge_contract.service,
        "protected_labels_untouched": protected_live_link_labels(),
        "cutover_performed": false,
        "notes": [
            "Candidate LaunchAgent only. Does not unload or replace live Node link/link-prod.",
            "Credentials still load via link run (Keychain + server plist); not embedded here.",
            "Edge /health must publish public contract epoch 2 before install bootstraps.",
        ],
    }))
}

#[cfg(target_os = "macos")]
fn uninstall_candidate(home: &Path) -> Result<Value, String> {
    assert_not_protected_mutation(LINK_RUST_CANDIDATE_LABEL)?;
    let paths = candidate_paths(home);
    bootout_label(LINK_RUST_CANDIDATE_LABEL)?;
    let removed = if paths.plist.is_file() {
        refuse_if_plist_is_protected_label(&paths.plist)?;
        fs::remove_file(&paths.plist)
            .map_err(|error| format!("cannot remove {}: {error}", paths.plist.display()))?;
        true
    } else {
        false
    };
    Ok(json!({
        "ok": true,
        "action": "link_uninstall_candidate",
        "label": LINK_RUST_CANDIDATE_LABEL,
        "plist": paths.plist.display().to_string(),
        "removed_plist": removed,
        "protected_labels_untouched": protected_live_link_labels(),
        "cutover_performed": false,
    }))
}

fn assert_not_protected_mutation(label: &str) -> Result<(), String> {
    if protected_live_link_labels().contains(&label) {
        return Err(format!(
            "refusing to mutate protected live Link label {label}"
        ));
    }
    if label != LINK_RUST_CANDIDATE_LABEL {
        return Err(format!(
            "link install/uninstall only manages {LINK_RUST_CANDIDATE_LABEL} (got {label})"
        ));
    }
    Ok(())
}

fn refuse_if_plist_is_protected_label(path: &Path) -> Result<(), String> {
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let value = PlistValue::from_reader(std::io::Cursor::new(bytes))
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    let label = value
        .as_dictionary()
        .and_then(|dict| dict.get("Label"))
        .and_then(PlistValue::as_string)
        .unwrap_or("");
    if protected_live_link_labels().contains(&label) {
        return Err(format!(
            "refusing to delete protected live Link plist {} (label {label})",
            path.display()
        ));
    }
    if label != LINK_RUST_CANDIDATE_LABEL {
        return Err(format!(
            "refusing to delete unexpected LaunchAgent {} (label {label})",
            path.display()
        ));
    }
    Ok(())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
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
            .unwrap_or("herdr-mcp-link"),
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
fn bootstrap_with_retry(plist: &Path, label: &str) -> Result<(), String> {
    let mut last_error = "launchctl bootstrap did not run".to_owned();
    for attempt in 0..=BOOTSTRAP_RETRY_DELAYS.len() {
        let _ = wait_launchd_absent(label, LAUNCHD_ABSENT_BUDGET);
        match run_launchctl([
            OsStr::new("bootstrap"),
            OsStr::new(&domain()),
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

#[cfg(target_os = "macos")]
fn bootout_label(label: &str) -> Result<(), String> {
    assert_not_protected_mutation(label)?;
    if !is_loaded(label) {
        return Ok(());
    }
    run_launchctl([
        OsStr::new("bootout"),
        OsStr::new(&format!("{}/{}", domain(), label)),
    ])
    .map(|_| ())
    .or_else(|error| {
        // Already absent is fine.
        if error.contains("No such process") || error.contains("Could not find") {
            Ok(())
        } else {
            Err(error)
        }
    })?;
    wait_launchd_absent(label, LAUNCHD_BOOTOUT_BUDGET)
}

#[cfg(target_os = "macos")]
fn is_loaded(label: &str) -> bool {
    Command::new("/bin/launchctl")
        .args(["print", &format!("{}/{}", domain(), label)])
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(target_os = "macos")]
fn wait_launchd_absent(label: &str, budget: Duration) -> Result<(), String> {
    let deadline = Instant::now() + budget;
    loop {
        if !is_loaded(label) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "launchd label {label} still present after {}",
                budget.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(target_os = "macos")]
fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
fn run_launchctl<I, S>(args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let collected = args
        .into_iter()
        .map(|value| value.as_ref().to_os_string())
        .collect::<Vec<_>>();
    if collected
        .first()
        .is_some_and(|value| value == OsStr::new("submit"))
    {
        return Err(
            "inferred launchd submission is forbidden for Link lifecycle mutations".to_owned(),
        );
    }
    let output = Command::new("/bin/launchctl")
        .args(&collected)
        .output()
        .map_err(|error| format!("cannot execute launchctl: {error}"))?;
    if output.status.success() {
        return Ok(output);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim().chars().take(400).collect::<String>();
    Err(format!("launchctl failed: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
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
        let root = env::temp_dir().join(format!(
            "herdr-mcp-link-install-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn setup_managed_runtime(home: &Path) -> PathBuf {
        let runtime = home.join(".config/herdr-mcp/runtime");
        let generation = runtime.join("generations/rust-testhash00000000");
        fs::create_dir_all(&generation).unwrap();
        let binary = generation.join("herdr-mcp");
        fs::write(&binary, b"#!/bin/sh\necho fake\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        }
        symlink("generations/rust-testhash00000000", runtime.join("current")).unwrap();
        managed_runtime_binary(home)
    }

    #[test]
    fn candidate_label_is_distinct_from_live_node_jobs() {
        assert_ne!(LINK_RUST_CANDIDATE_LABEL, LINK_LABEL);
        assert_ne!(LINK_RUST_CANDIDATE_LABEL, LINK_PROD_LABEL);
        assert!(protected_live_link_labels().contains(&LINK_LABEL));
        assert!(protected_live_link_labels().contains(&LINK_PROD_LABEL));
        assert!(!protected_live_link_labels().contains(&LINK_RUST_CANDIDATE_LABEL));
    }

    #[test]
    fn program_arguments_require_managed_runtime_link_run() {
        let home = test_home();
        setup_managed_runtime(&home);
        let args = candidate_program_arguments(&home).expect("args");
        assert_eq!(
            args,
            vec![
                managed_runtime_binary(&home).to_string_lossy().into_owned(),
                "link".to_owned(),
                "run".to_owned(),
            ]
        );
        assert_safe_candidate_program(&home, &args).unwrap();
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn refuses_checkout_and_target_program_paths() {
        let home = test_home();
        setup_managed_runtime(&home);

        let checkout = home
            .join("Documents/herdr-mcp/target/release/herdr-mcp")
            .to_string_lossy()
            .into_owned();
        let err =
            assert_safe_candidate_program(&home, &[checkout, "link".to_owned(), "run".to_owned()])
                .expect_err("checkout");
        assert!(err.contains("must be") || err.contains("refusing"));

        let target = "/tmp/project/target/debug/herdr-mcp".to_owned();
        let err = refuse_checkout_or_target_path(Path::new(&target)).expect_err("target");
        assert!(err.contains("target"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_rejects_non_generation_runtime_current() {
        let home = test_home();
        let runtime = home.join(".config/herdr-mcp/runtime");
        fs::create_dir_all(runtime.join("current-dir")).unwrap();
        fs::write(runtime.join("current-dir/herdr-mcp"), b"x").unwrap();
        symlink("current-dir", runtime.join("current")).unwrap();
        let err = resolve_managed_runtime_binary(&home).expect_err("bad current");
        assert!(err.contains("generations/rust-"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn encoded_plist_owns_candidate_label_and_runtime_argv() {
        let home = test_home();
        setup_managed_runtime(&home);
        let program = candidate_program_arguments(&home).unwrap();
        let bytes = encode_candidate_plist(&home, &program, &default_candidate_env()).unwrap();
        let xml = String::from_utf8(bytes).unwrap();
        assert!(xml.contains(LINK_RUST_CANDIDATE_LABEL));
        assert!(xml.contains(&format!("<string>{LINK_RUST_CANDIDATE_LABEL}</string>")));
        assert!(xml.contains("link</string>"));
        assert!(xml.contains("run</string>"));
        assert!(xml.contains("runtime/current/herdr-mcp"));
        assert!(!xml.contains(&format!("<string>{LINK_LABEL}</string>")));
        assert!(!xml.contains(&format!("<string>{LINK_PROD_LABEL}</string>")));
        assert!(!xml.contains("TOKEN"));
        assert!(!xml.contains("dist/link"));
        assert!(!xml.contains("/target/"));
        assert!(xml.contains(CANDIDATE_WORKSTATION_ID));
        assert!(xml.contains("herdr-edge-prod"));
        assert!(xml.contains("herdr-edge-prod-link-secret"));
        assert!(!xml.contains("herdr-edge-dev.whshang"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn encode_rejects_credential_env_keys() {
        let home = test_home();
        setup_managed_runtime(&home);
        let program = candidate_program_arguments(&home).unwrap();
        let mut env = default_candidate_env();
        env.insert("HERDR_MCP_TOKEN".to_owned(), "secret".to_owned());
        let err = encode_candidate_plist(&home, &program, &env).expect_err("token");
        assert!(err.contains("credential-like"));
        assert!(!err.contains("secret"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn protected_mutation_guard_blocks_live_labels() {
        let err = assert_not_protected_mutation(LINK_PROD_LABEL).expect_err("prod");
        assert!(err.contains("protected"));
        let err = assert_not_protected_mutation(LINK_LABEL).expect_err("link");
        assert!(err.contains("protected"));
        assert_not_protected_mutation(LINK_RUST_CANDIDATE_LABEL).unwrap();
    }

    #[test]
    fn refuse_delete_protects_live_plist_labels() {
        let home = test_home();
        let path = home.join("dev.herdr-mcp.link-prod.plist");
        let mut root = Dictionary::new();
        root.insert(
            "Label".to_owned(),
            PlistValue::String(LINK_PROD_LABEL.to_owned()),
        );
        let mut bytes = Vec::new();
        PlistValue::Dictionary(root)
            .to_writer_xml(&mut bytes)
            .unwrap();
        fs::write(&path, bytes).unwrap();
        let err = refuse_if_plist_is_protected_label(&path).expect_err("prod");
        assert!(err.contains("protected"));
        let _ = fs::remove_dir_all(&home);
    }
}
