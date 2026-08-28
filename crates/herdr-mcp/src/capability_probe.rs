use crate::capability_inventory::Evidence;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub const PROBE_ADAPTER_VERSION: u32 = 2;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_PROBE_OUTPUT_BYTES: usize = 32 * 1024;
const MAX_HERDR_START_KINDS: usize = 256;
const SAFE_ENV_KEYS: &[&str] = &["PATH", "LANG", "LC_ALL", "TMPDIR"];
static PROBE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const SAFE_SELF_DESCRIPTION_AGENTS: &[&str] = &[
    "agy", "claude", "codex", "droid", "grok", "kilo", "opencode", "pi",
];

struct ProbeHomeGuard {
    path: PathBuf,
}

impl Drop for ProbeHomeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug)]
struct CommandOutput {
    success: bool,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

pub fn version_probe(agent: &str, path: &Path, observed_at_ms: i64) -> Option<Evidence<String>> {
    if !safe_self_description_agent(agent) {
        return None;
    }
    let output = run_bounded(path, &[OsStr::new("--version")], COMMAND_TIMEOUT).ok()?;
    version_evidence(output, observed_at_ms)
}

fn version_evidence(output: CommandOutput, observed_at_ms: i64) -> Option<Evidence<String>> {
    if output.timed_out {
        return None;
    }
    let value =
        first_nonempty_line(&output.stdout).or_else(|| first_nonempty_line(&output.stderr))?;
    Some(Evidence {
        value,
        source: "cli_version_probe".to_owned(),
        authority: if output.success {
            "reported".to_owned()
        } else {
            "reported_nonzero_exit".to_owned()
        },
        observed_at_ms,
        detail: None,
    })
}

#[derive(Debug, Default)]
pub struct DeepProbeResult {
    pub supports_code_edit: Option<Evidence<bool>>,
    pub supports_shell: Option<Evidence<bool>>,
    pub can_run_headless: Option<Evidence<bool>>,
}

pub fn deep_probe(agent: &str, path: &Path, observed_at_ms: i64) -> DeepProbeResult {
    if !safe_self_description_agent(agent) {
        return DeepProbeResult::default();
    }
    let Ok(output) = run_bounded(path, &[OsStr::new("--help")], COMMAND_TIMEOUT) else {
        return DeepProbeResult::default();
    };
    if output.timed_out {
        return DeepProbeResult::default();
    }
    let text = format!("{}\n{}", output.stdout, output.stderr).to_lowercase();
    let headless = [
        "non-interactive",
        "noninteractive",
        "headless",
        "run non-interactively",
    ]
    .into_iter()
    .find(|needle| text.contains(needle))
    .map(|needle| {
        reported_bool(
            "cli_help_probe",
            observed_at_ms,
            format!("help advertises '{needle}'"),
        )
    });

    let (supports_code_edit, supports_shell) = match agent {
        "pi" => {
            let code_edit = text.contains("edit       - edit files with find/replace")
                && text.contains("write      - write files (creates/overwrites)");
            let shell = text.contains("bash       - execute bash commands");
            (
                code_edit.then(|| {
                    reported_bool(
                        "pi_cli_help_probe",
                        observed_at_ms,
                        "Pi help lists built-in edit and write file tools".to_owned(),
                    )
                }),
                shell.then(|| {
                    reported_bool(
                        "pi_cli_help_probe",
                        observed_at_ms,
                        "Pi help lists built-in bash command tool".to_owned(),
                    )
                }),
            )
        }
        _ => (None, None),
    };

    DeepProbeResult {
        supports_code_edit,
        supports_shell,
        can_run_headless: headless,
    }
}

fn reported_bool(source: &str, observed_at_ms: i64, detail: String) -> Evidence<bool> {
    Evidence {
        value: true,
        source: source.to_owned(),
        authority: "reported".to_owned(),
        observed_at_ms,
        detail: Some(detail),
    }
}

fn safe_self_description_agent(agent: &str) -> bool {
    SAFE_SELF_DESCRIPTION_AGENTS.contains(&agent)
}

pub fn find_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    find_executable_in_path(name, &path)
}

pub fn herdr_declared_start_kinds() -> Result<Vec<String>, String> {
    let herdr = find_executable("herdr")
        .ok_or_else(|| "Herdr CLI is not available on PATH for start-kind discovery".to_owned())?;
    let output = run_bounded(
        &herdr,
        &[
            OsStr::new("agent"),
            OsStr::new("start"),
            OsStr::new("--help"),
        ],
        COMMAND_TIMEOUT,
    )?;
    if output.timed_out {
        return Err("Herdr start-kind discovery timed out".to_owned());
    }
    if !output.success {
        return Err("Herdr start-kind discovery exited unsuccessfully".to_owned());
    }
    parse_herdr_start_kinds(&format!("{}\n{}", output.stdout, output.stderr))
}

fn parse_herdr_start_kinds(text: &str) -> Result<Vec<String>, String> {
    let marker = "[possible values:";
    let start = text
        .find(marker)
        .ok_or_else(|| "Herdr agent start help did not expose possible values".to_owned())?;
    let rest = &text[start + marker.len()..];
    let end = rest.find(']').ok_or_else(|| {
        "Herdr agent start help has an unterminated possible-values list".to_owned()
    })?;
    let mut kinds = rest[..end]
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    kinds.sort();
    kinds.dedup();
    if kinds.is_empty() || kinds.len() > MAX_HERDR_START_KINDS {
        return Err(format!(
            "Herdr agent start help exposed {} possible values; expected 1..={MAX_HERDR_START_KINDS}",
            kinds.len()
        ));
    }
    if kinds.iter().any(|kind| {
        kind.len() > 64
            || !kind.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
            })
    }) {
        return Err("Herdr agent start help exposed an invalid agent kind".to_owned());
    }
    Ok(kinds)
}

fn find_executable_in_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    if name.contains(std::path::MAIN_SEPARATOR) {
        return None;
    }
    for dir in std::env::split_paths(path) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub fn fingerprint(
    agent: &str,
    manifest: &Value,
    binary: Option<&Path>,
    herdr_startable: Option<bool>,
) -> Result<String, String> {
    let mut digest = Sha256::new();
    digest.update(b"herdr-capability-fingerprint-v1\0");
    digest.update(agent.as_bytes());
    digest.update(b"\0");
    for key in ["active_version", "source", "source_kind"] {
        if let Some(value) = manifest.get(key).and_then(Value::as_str) {
            digest.update(key.as_bytes());
            digest.update(b"=");
            digest.update(value.as_bytes());
            digest.update(b"\0");
        }
    }
    digest.update(b"herdr_startable=");
    digest.update(match herdr_startable {
        Some(true) => b"true".as_slice(),
        Some(false) => b"false".as_slice(),
        None => b"unknown".as_slice(),
    });
    digest.update(b"\0");
    digest.update(PROBE_ADAPTER_VERSION.to_le_bytes());
    if let Some(binary) = binary {
        digest.update(binary.as_os_str().as_encoded_bytes());
        let metadata = std::fs::metadata(binary).map_err(|error| {
            format!(
                "cannot inspect capability binary {}: {error}",
                binary.display()
            )
        })?;
        digest.update(metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(UNIX_EPOCH)
        {
            digest.update(duration.as_nanos().to_le_bytes());
        }
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn run_bounded(path: &Path, args: &[&OsStr], timeout: Duration) -> Result<CommandOutput, String> {
    let sequence = PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let probe_home = std::env::temp_dir().join(format!(
        "herdr-mcp-capability-probe-home-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir_all(&probe_home).map_err(|error| {
        format!(
            "cannot create isolated capability probe home {}: {error}",
            probe_home.display()
        )
    })?;
    let _probe_home_guard = ProbeHomeGuard {
        path: probe_home.clone(),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&probe_home, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                format!(
                    "cannot secure isolated capability probe home {}: {error}",
                    probe_home.display()
                )
            },
        )?;
    }

    let mut command = std::process::Command::new(path);
    command
        .args(args.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .envs(filtered_probe_environment(std::env::vars_os()))
        .env("HOME", &probe_home)
        .env("XDG_CONFIG_HOME", probe_home.join("config"))
        .env("XDG_STATE_HOME", probe_home.join("state"))
        .env("XDG_DATA_HOME", probe_home.join("data"))
        .env("XDG_CACHE_HOME", probe_home.join("cache"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("CI", "1")
        .env("NO_UPDATE_NOTIFIER", "1")
        .env("DO_NOT_TRACK", "1")
        .env("HERDR_MCP_CAPABILITY_PROBE", "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start capability probe {}: {error}", path.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "capability probe stdout unavailable".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "capability probe stderr unavailable".to_owned())?;
    let stdout_reader = std::thread::spawn(move || read_bounded_and_drain(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded_and_drain(stderr));

    let deadline = Instant::now() + timeout;
    let (success, timed_out) = loop {
        match child
            .try_wait()
            .map_err(|error| format!("cannot observe capability probe: {error}"))?
        {
            Some(status) => break (status.success(), false),
            None if Instant::now() >= deadline => {
                terminate_probe_process_group(child.id());
                let _ = child.kill();
                let _ = child.wait();
                break (false, true);
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    if !timed_out {
        for _ in 0..10 {
            if stdout_reader.is_finished() && stderr_reader.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !stdout_reader.is_finished() || !stderr_reader.is_finished() {
            terminate_probe_process_group(child.id());
        }
    }
    let stdout = stdout_reader
        .join()
        .map_err(|_| "capability probe stdout reader panicked".to_owned())?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "capability probe stderr reader panicked".to_owned())?;
    // The leader may have exited cleanly while a background descendant detached from stdio.
    // Reap anything that remains in the dedicated process group only after output is drained.
    terminate_probe_process_group(child.id());
    Ok(CommandOutput {
        success,
        timed_out,
        stdout,
        stderr,
    })
}

#[cfg(unix)]
fn terminate_probe_process_group(pid: u32) {
    let pgid = -(pid as i32);
    unsafe {
        libc::kill(pgid, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_millis(10));
    unsafe {
        libc::kill(pgid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn terminate_probe_process_group(_pid: u32) {}

fn filtered_probe_environment<I>(vars: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    vars.into_iter()
        .filter(|(key, _)| key.to_str().is_some_and(|key| SAFE_ENV_KEYS.contains(&key)))
        .collect()
}

fn read_bounded_and_drain(mut reader: impl Read) -> String {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let remaining = MAX_PROBE_OUTPUT_BYTES.saturating_sub(captured.len());
                if remaining > 0 {
                    captured.extend_from_slice(&buffer[..read.min(remaining)]);
                }
            }
        }
    }
    String::from_utf8_lossy(&captured).into_owned()
}

fn first_nonempty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(240).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "herdr-capability-probe-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn environment_filter_drops_secret_like_values() {
        let filtered = filtered_probe_environment(vec![
            (OsString::from("PATH"), OsString::from("/bin")),
            (OsString::from("HOME"), OsString::from("/tmp/home")),
            (
                OsString::from("OPENAI_API_KEY"),
                OsString::from("must-not-leak"),
            ),
            (
                OsString::from("HERDR_EDGE_TOKEN"),
                OsString::from("must-not-leak"),
            ),
        ]);
        assert_eq!(filtered.len(), 1);
        assert!(filtered.iter().any(|(key, _)| key == "PATH"));
        assert!(!filtered.iter().any(|(key, _)| key == "HOME"));
        assert!(!filtered.iter().any(|(key, _)| key == "OPENAI_API_KEY"));
        assert!(!filtered.iter().any(|(key, _)| key == "HERDR_EDGE_TOKEN"));
    }

    #[test]
    fn self_description_probe_requires_explicit_safe_adapter() {
        assert!(safe_self_description_agent("pi"));
        assert!(!safe_self_description_agent("cline"));
        assert!(!safe_self_description_agent("future-agent"));
    }

    #[cfg(unix)]
    #[test]
    fn disallowed_agent_probe_does_not_execute_binary() {
        let dir = temp_dir("disallowed");
        let marker = dir.join("invoked");
        let binary = script(
            &dir,
            "cline",
            &format!("touch '{}'\necho 9.9.9", marker.display()),
        );
        assert!(version_probe("cline", &binary, 1).is_none());
        assert!(!marker.exists());
        let result = deep_probe("cline", &binary, 1);
        assert!(result.can_run_headless.is_none());
        assert!(!marker.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn version_evidence_reads_reported_version_without_process_timing() {
        let evidence = version_evidence(
            CommandOutput {
                success: true,
                timed_out: false,
                stdout: "demo 1.2.3\n".to_owned(),
                stderr: String::new(),
            },
            42,
        )
        .unwrap();
        assert_eq!(evidence.value, "demo 1.2.3");
        assert_eq!(evidence.authority, "reported");
        assert_eq!(evidence.observed_at_ms, 42);
    }

    #[cfg(unix)]
    #[test]
    fn probe_timeout_kills_child_and_returns_no_version() {
        let dir = temp_dir("timeout");
        let binary = script(&dir, "slow", "sleep 2; echo late");
        let output = run_bounded(&binary, &[], Duration::from_millis(50)).unwrap();
        assert!(output.timed_out);
        assert!(!output.success);
        assert!(version_evidence(output, 1).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn probe_capture_is_bounded_while_pipe_is_fully_drained() {
        let dir = temp_dir("bounded");
        let binary = script(
            &dir,
            "large",
            "i=0; while [ $i -lt 5000 ]; do printf '0123456789'; i=$((i+1)); done",
        );
        let output = run_bounded(&binary, &[], Duration::from_secs(5)).unwrap();
        assert!(output.success, "large-output probe unexpectedly timed out");
        assert!(output.stdout.len() <= MAX_PROBE_OUTPUT_BYTES);
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn successful_probe_reaps_background_descendants_before_home_cleanup() {
        let dir = temp_dir("background");
        let binary = script(
            &dir,
            "background",
            r#"printf '%s\n' "$HOME"; (sleep 0.2; mkdir -p "$HOME/recreated") >/dev/null 2>&1 &"#,
        );
        let output = run_bounded(&binary, &[], Duration::from_secs(5)).unwrap();
        assert!(
            output.success,
            "background-cleanup probe unexpectedly timed out"
        );
        let probe_home = PathBuf::from(output.stdout.trim());
        std::thread::sleep(Duration::from_millis(350));
        assert!(!probe_home.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn executable_discovery_uses_supplied_path_and_rejects_missing() {
        let dir = temp_dir("path");
        let binary = script(&dir, "demo-agent", "echo ok");
        assert_eq!(
            find_executable_in_path("demo-agent", dir.as_os_str()),
            Some(binary)
        );
        assert!(find_executable_in_path("missing-agent", dir.as_os_str()).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn herdr_start_kind_parser_uses_the_installed_cli_declaration() {
        let kinds = parse_herdr_start_kinds(
            "Usage: herdr agent start --kind <KIND>\n\n  --kind <KIND> [possible values: pi, claude, omp, qwen, mastracode]\n",
        )
        .unwrap();
        assert_eq!(kinds, vec!["claude", "mastracode", "omp", "pi", "qwen"]);
    }

    #[test]
    fn herdr_start_kind_parser_fails_closed_on_missing_or_invalid_declaration() {
        assert!(parse_herdr_start_kinds("Usage: herdr agent start").is_err());
        assert!(parse_herdr_start_kinds("[possible values: pi, ../foreign]").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn print_flag_alone_does_not_verify_headless_execution() {
        let dir = temp_dir("print-only");
        let binary = script(
            &dir,
            "claude",
            "printf '%s\\n' '  --print  Print diagnostics'",
        );
        let result = deep_probe("claude", &binary, 9);
        assert!(result.can_run_headless.is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn pi_deep_probe_requires_explicit_built_in_tool_evidence() {
        let dir = temp_dir("pi-deep");
        let binary = script(
            &dir,
            "pi",
            "cat <<'HELP'\nRun non-interactively\nBuilt-in Tool Names:\n  read       - Read file contents\n  bash       - Execute bash commands\n  edit       - Edit files with find/replace\n  write      - Write files (creates/overwrites)\nHELP",
        );
        let result = deep_probe("pi", &binary, 9);
        assert_eq!(
            result.supports_code_edit.as_ref().map(|e| e.value),
            Some(true)
        );
        assert_eq!(result.supports_shell.as_ref().map(|e| e.value), Some(true));
        assert_eq!(
            result.can_run_headless.as_ref().map(|e| e.value),
            Some(true)
        );

        let generic = deep_probe("unknown", &binary, 9);
        assert!(generic.supports_code_edit.is_none());
        assert!(generic.supports_shell.is_none());
        assert!(generic.can_run_headless.is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn fingerprint_changes_with_manifest_or_binary_identity() {
        let dir = temp_dir("fingerprint");
        let binary = script(&dir, "demo", "echo one");
        let first_manifest = json!({
            "active_version": "1",
            "source": "bundled",
            "source_kind": "bundled"
        });
        let second_manifest = json!({
            "active_version": "2",
            "source": "bundled",
            "source_kind": "bundled"
        });
        let first = fingerprint("demo", &first_manifest, Some(&binary), Some(true)).unwrap();
        let manifest_changed =
            fingerprint("demo", &second_manifest, Some(&binary), Some(true)).unwrap();
        assert_ne!(first, manifest_changed);
        let startability_changed =
            fingerprint("demo", &first_manifest, Some(&binary), Some(false)).unwrap();
        assert_ne!(first, startability_changed);
        fs::write(&binary, "#!/bin/sh\necho a-much-longer-version\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let binary_changed =
            fingerprint("demo", &first_manifest, Some(&binary), Some(true)).unwrap();
        assert_ne!(first, binary_changed);
        let _ = fs::remove_dir_all(dir);
    }
}
