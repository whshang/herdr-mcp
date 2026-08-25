use crate::fs_security;
use serde_json::{Map, Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_MAX_BYTES: usize = 65_536;
const MAX_BYTES: usize = 512_000;
const DEFAULT_LOG_COUNT: u64 = 20;
const MAX_LOG_COUNT: u64 = 100;
const TIMEOUT: Duration = Duration::from_secs(15);

pub fn run(snapshot: &Value, args: &Value) -> Value {
    let root_input = match required_str(args, "root") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let action = match required_str(args, "action") {
        Ok(value @ ("status" | "diff" | "log")) => value,
        Ok(_) => return invalid("action must be one of status, diff, log"),
        Err(error) => return error,
    };
    let managed = match fs_security::validate_existing(snapshot, root_input) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !managed.root.is_dir() {
        return json!({"ok": false, "reason": "not_a_git_repo", "root": managed.root.to_string_lossy()});
    }
    let budget = match optional_usize(args, "max_bytes", 1, MAX_BYTES) {
        Ok(value) => value.unwrap_or(DEFAULT_MAX_BYTES),
        Err(error) => return error,
    };
    let staged = match optional_bool(args, "staged") {
        Ok(value) => value.unwrap_or(false),
        Err(error) => return error,
    };
    let max_count = match optional_u64(args, "max_count", 1, MAX_LOG_COUNT) {
        Ok(value) => value.unwrap_or(DEFAULT_LOG_COUNT),
        Err(error) => return error,
    };
    let path = match optional_str(args, "path") {
        Ok(value) => value,
        Err(error) => return error,
    };

    let mut command_args = Vec::<String>::new();
    match action {
        "status" => command_args.extend(["status".into(), "--porcelain".into(), "-b".into()]),
        "diff" => {
            command_args.push("diff".into());
            if staged {
                command_args.push("--staged".into());
            }
            if let Some(path) = path {
                let safe_path = match safe_diff_path(&managed.root, path) {
                    Ok(value) => value,
                    Err(error) => return error,
                };
                command_args.push("--".into());
                command_args.push(safe_path.to_string_lossy().into_owned());
            }
        }
        "log" => command_args.extend([
            "log".into(),
            format!("-n{max_count}"),
            "--oneline".into(),
            "--decorate".into(),
        ]),
        _ => unreachable!(),
    }

    let result = match run_git(&managed.root, &command_args, budget, TIMEOUT) {
        Ok(value) => value,
        Err(message) => {
            return json!({
                "ok": false,
                "root": managed.root.to_string_lossy(),
                "action": action,
                "message": message,
            });
        }
    };
    let mut output = Map::new();
    output.insert("ok".to_owned(), json!(result.exit_code == 0));
    output.insert("root".to_owned(), json!(managed.root.to_string_lossy()));
    output.insert("action".to_owned(), json!(action));
    output.insert("exit_code".to_owned(), json!(result.exit_code));
    output.insert("truncated".to_owned(), json!(result.truncated));
    output.insert("output".to_owned(), json!(result.stdout));
    if !result.stderr.is_empty() {
        output.insert("stderr".to_owned(), json!(result.stderr));
    }
    Value::Object(output)
}

pub fn file_dirty(root: &Path, file: &Path) -> Result<bool, String> {
    let root_real = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let file_real = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let relative = file_real
        .strip_prefix(&root_real)
        .map_err(|_| "file is outside git root".to_owned())?;
    let args = vec![
        "status".to_owned(),
        "--porcelain".to_owned(),
        "--".to_owned(),
        relative.to_string_lossy().into_owned(),
    ];
    let result = run_git(&root_real, &args, 4096, Duration::from_secs(2))?;
    if result.exit_code != 0 {
        return Err(if result.stderr.is_empty() {
            format!("git status exited {}", result.exit_code)
        } else {
            result.stderr
        });
    }
    Ok(!result.stdout.trim().is_empty())
}

struct GitResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
    truncated: bool,
}

fn run_git(
    cwd: &Path,
    args: &[String],
    budget: usize,
    timeout: Duration,
) -> Result<GitResult, String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start git: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "git stdout unavailable".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "git stderr unavailable".to_owned())?;
    let stdout_reader = thread::spawn(move || read_capped(stdout, budget.saturating_add(1)));
    let stderr_reader = thread::spawn(move || read_capped(stderr, 2001));
    let started = Instant::now();
    let status = loop {
        match child
            .try_wait()
            .map_err(|error| format!("cannot wait for git: {error}"))?
        {
            Some(status) => break status,
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err("git command timed out after 15s".to_owned());
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let truncated = stdout.len() > budget;
    let stdout = String::from_utf8_lossy(&stdout[..stdout.len().min(budget)]).into_owned();
    let stderr = String::from_utf8_lossy(&stderr[..stderr.len().min(2000)]).into_owned();
    Ok(GitResult {
        exit_code: status.code().unwrap_or(1),
        stdout,
        stderr,
        truncated,
    })
}

fn read_capped<R: Read>(mut input: R, cap: usize) -> Vec<u8> {
    let mut output = Vec::with_capacity(cap.min(64 * 1024));
    let mut buffer = [0u8; 8192];
    loop {
        let read = match input.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        if output.len() < cap {
            let keep = read.min(cap - output.len());
            output.extend_from_slice(&buffer[..keep]);
        }
    }
    output
}

fn safe_diff_path(root: &Path, input: &str) -> Result<PathBuf, Value> {
    let candidate = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        root.join(input)
    };
    let candidate = normalize_lexical(&candidate);
    if !fs_security::path_within(root, &candidate) {
        return Err(
            json!({"ok": false, "reason": "outside_managed_roots", "path": candidate.to_string_lossy()}),
        );
    }
    if fs_security::denied_secret_path(&candidate) {
        return Err(
            json!({"ok": false, "reason": "secret_path_denied", "path": candidate.to_string_lossy()}),
        );
    }
    Ok(candidate)
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                output.pop();
            }
            other => output.push(other.as_os_str()),
        }
    }
    output
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("{key} must be a string")))
}

fn optional_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(invalid(&format!("{key} must be a string"))),
    }
}

fn optional_bool(args: &Value, key: &str) -> Result<Option<bool>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(invalid(&format!("{key} must be a boolean"))),
    }
}

fn optional_usize(args: &Value, key: &str, min: usize, max: usize) -> Result<Option<usize>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value >= min && *value <= max)
        {
            Some(value) => Ok(Some(value)),
            None => Err(invalid(&format!(
                "{key} must be an integer in {min}..={max}"
            ))),
        },
    }
}

fn optional_u64(args: &Value, key: &str, min: u64, max: u64) -> Result<Option<u64>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value
            .as_u64()
            .filter(|value| *value >= min && *value <= max)
        {
            Some(value) => Ok(Some(value)),
            None => Err(invalid(&format!(
                "{key} must be an integer in {min}..={max}"
            ))),
        },
    }
}

fn invalid(message: &str) -> Value {
    json!({"ok": false, "code": "invalid_params", "message": message})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_path_cannot_escape_root() {
        let root = PathBuf::from("/tmp/project");
        assert!(safe_diff_path(&root, "src/lib.rs").is_ok());
        assert_eq!(
            safe_diff_path(&root, "../secret.txt").unwrap_err()["reason"],
            "outside_managed_roots"
        );
    }
    #[test]
    fn status_runs_only_inside_managed_git_root() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-git-tool-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.join("new.txt"), "new\n").unwrap();
        let snapshot = json!({"panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}], "agents": []});
        let result = run(
            &snapshot,
            &json!({"root": root, "action": "status", "max_bytes": 4096}),
        );
        assert_eq!(result["ok"], true);
        assert!(result["output"].as_str().unwrap().contains("new.txt"));
        fs::remove_dir_all(root).unwrap();
    }
}
