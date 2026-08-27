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
const STATUS_COMPACT_AFTER: usize = 24;
const DIFF_COMPACT_AFTER_FILES: usize = 8;
const DIFF_COMPACT_AFTER_BYTES: usize = 8192;
const LOG_COMPACT_AFTER: usize = 40;

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
    if action == "status"
        && !result.truncated
        && let Some(compact) = compact_git_status(&result.stdout)
    {
        output.insert("counts".to_owned(), compact.counts);
        if compact.files > STATUS_COMPACT_AFTER {
            output.insert("output".to_owned(), json!(compact.grouped));
            output.insert("compacted".to_owned(), json!(true));
        }
    }
    if action == "diff"
        && result.exit_code == 0
        && !result.truncated
        && let Some(compact) = compact_git_diff(&result.stdout)
    {
        output.insert("counts".to_owned(), compact.counts);
        if compact.files > DIFF_COMPACT_AFTER_FILES
            || result.stdout.len() > DIFF_COMPACT_AFTER_BYTES
        {
            output.insert("output".to_owned(), json!(compact.grouped));
            output.insert("compacted".to_owned(), json!(true));
        }
    }
    if action == "log"
        && result.exit_code == 0
        && !result.truncated
        && let Some(compact) = compact_git_log(&result.stdout)
    {
        output.insert("counts".to_owned(), compact.counts);
        if compact.commits > LOG_COMPACT_AFTER {
            output.insert("output".to_owned(), json!(compact.grouped));
            output.insert("compacted".to_owned(), json!(true));
        }
    }
    if !result.stderr.is_empty() {
        output.insert("stderr".to_owned(), json!(result.stderr));
    }
    Value::Object(output)
}

struct StatusCompact {
    counts: Value,
    grouped: String,
    files: usize,
}

fn compact_git_status(stdout: &str) -> Option<StatusCompact> {
    let mut branch = String::new();
    let mut entries = Vec::<(String, String)>::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            branch = rest.to_owned();
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        let xy = line[..2].to_owned();
        let rest = line[3..].trim();
        if rest.is_empty() {
            continue;
        }
        let path = rest.rsplit(" -> ").next().unwrap_or(rest);
        entries.push((xy, path.replace('\\', "/")));
    }
    if branch.is_empty() && entries.is_empty() {
        return None;
    }

    let mut modified = 0usize;
    let mut untracked = 0usize;
    let mut deleted = 0usize;
    let mut renamed = 0usize;
    for (xy, _) in &entries {
        if xy == "??" {
            untracked += 1;
        } else if xy.contains('R') {
            renamed += 1;
        } else if xy.contains('D') {
            deleted += 1;
        } else {
            modified += 1;
        }
    }

    let mut groups = std::collections::BTreeMap::<String, Vec<(String, String)>>::new();
    for (xy, path) in &entries {
        let (dir, name) = match path.rsplit_once('/') {
            Some((dir, name)) => (dir.to_owned(), name.to_owned()),
            None => (".".to_owned(), path.clone()),
        };
        groups.entry(dir).or_default().push((xy.clone(), name));
    }

    let mut grouped = String::new();
    if !branch.is_empty() {
        grouped.push_str("## ");
        grouped.push_str(&branch);
        grouped.push('\n');
    }
    for (dir, files) in &groups {
        grouped.push_str(&format!("{dir} ({})\n", files.len()));
        for (xy, name) in files {
            grouped.push_str(&format!(" {xy} {name}\n"));
        }
    }

    Some(StatusCompact {
        counts: json!({
            "branch": branch,
            "files": entries.len(),
            "modified": modified,
            "untracked": untracked,
            "deleted": deleted,
            "renamed": renamed,
        }),
        grouped,
        files: entries.len(),
    })
}

struct DiffFile {
    path: String,
    hunks: usize,
    insertions: usize,
    deletions: usize,
    binary: bool,
}

struct DiffCompact {
    counts: Value,
    grouped: String,
    files: usize,
}

fn compact_git_diff(stdout: &str) -> Option<DiffCompact> {
    let mut files = Vec::<DiffFile>::new();
    let mut current: Option<DiffFile> = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(file) = current.take() {
                files.push(file);
            }
            current = Some(DiffFile {
                path: parse_diff_git_b_path(rest),
                hunks: 0,
                insertions: 0,
                deletions: 0,
                binary: false,
            });
            continue;
        }
        let Some(file) = current.as_mut() else {
            continue;
        };
        if line.starts_with("@@ ") {
            file.hunks += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            let path = path.split('\t').next().unwrap_or(path).trim();
            if path != "/dev/null" {
                let path = path.strip_prefix("b/").unwrap_or(path);
                file.path = path.trim_matches('"').replace('\\', "/");
            }
            continue;
        }
        if line.starts_with("--- ") {
            continue;
        }
        if line.starts_with("Binary files ") {
            file.binary = true;
            continue;
        }
        if line.starts_with('+') {
            file.insertions += 1;
        } else if line.starts_with('-') {
            file.deletions += 1;
        }
    }
    if let Some(file) = current {
        files.push(file);
    }
    if files.is_empty() {
        return None;
    }

    let insertions: usize = files.iter().map(|file| file.insertions).sum();
    let deletions: usize = files.iter().map(|file| file.deletions).sum();
    let hunks: usize = files.iter().map(|file| file.hunks).sum();

    let mut groups = std::collections::BTreeMap::<String, Vec<&DiffFile>>::new();
    for file in &files {
        let dir = match file.path.rsplit_once('/') {
            Some((dir, _)) => dir.to_owned(),
            None => ".".to_owned(),
        };
        groups.entry(dir).or_default().push(file);
    }

    let mut grouped = String::new();
    for (dir, dir_files) in &groups {
        grouped.push_str(&format!("{dir} ({})\n", dir_files.len()));
        for file in dir_files {
            let name = file
                .path
                .rsplit_once('/')
                .map(|(_, name)| name)
                .unwrap_or(file.path.as_str());
            if file.binary {
                grouped.push_str(&format!(" {name}  binary\n"));
            } else {
                grouped.push_str(&format!(
                    " {name}  +{}/-{}  {} hunks\n",
                    file.insertions, file.deletions, file.hunks
                ));
            }
        }
    }

    Some(DiffCompact {
        counts: json!({
            "files": files.len(),
            "insertions": insertions,
            "deletions": deletions,
            "hunks": hunks,
        }),
        grouped,
        files: files.len(),
    })
}

fn parse_diff_git_b_path(rest: &str) -> String {
    let raw = if let Some(idx) = rest.rfind(" b/") {
        &rest[idx + 3..]
    } else if let Some(idx) = rest.rfind(" \"b/") {
        &rest[idx + 4..]
    } else {
        rest
    };
    raw.trim().trim_matches('"').replace('\\', "/")
}

struct LogCompact {
    counts: Value,
    grouped: String,
    commits: usize,
}

fn compact_git_log(stdout: &str) -> Option<LogCompact> {
    let mut commits = Vec::<(String, String)>::new();
    for line in stdout.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let (sha, subject) = parse_oneline_log(line)?;
        commits.push((sha, subject));
    }
    if commits.is_empty() {
        return None;
    }

    let mut grouped = String::new();
    for (sha, subject) in &commits {
        grouped.push_str(sha);
        if !subject.is_empty() {
            grouped.push(' ');
            grouped.push_str(subject);
        }
        grouped.push('\n');
    }

    Some(LogCompact {
        counts: json!({ "commits": commits.len() }),
        grouped,
        commits: commits.len(),
    })
}

fn parse_oneline_log(line: &str) -> Option<(String, String)> {
    let (sha, rest) = match line.split_once(' ') {
        Some((sha, rest)) => (sha, rest.trim()),
        None => (line, ""),
    };
    if sha.len() < 4 || !sha.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let subject = if rest.starts_with('(') {
        match rest.find(')') {
            Some(end) => rest[end + 1..].trim().to_owned(),
            None => rest.to_owned(),
        }
    } else {
        rest.to_owned()
    };
    Some((sha.to_owned(), subject))
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

/// Return the first dirty path among a set using one `git status` process.
pub fn first_dirty_file(root: &Path, files: &[PathBuf]) -> Result<Option<PathBuf>, String> {
    if files.is_empty() {
        return Ok(None);
    }
    let root_real = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut relatives = Vec::<PathBuf>::new();
    for file in files {
        let file_real = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
        let relative = file_real
            .strip_prefix(&root_real)
            .map_err(|_| "file is outside git root".to_owned())?
            .to_path_buf();
        if !relatives.contains(&relative) {
            relatives.push(relative);
        }
    }
    if relatives.is_empty() {
        return Ok(None);
    }

    let mut args = vec![
        "status".to_owned(),
        "--porcelain=v1".to_owned(),
        "-z".to_owned(),
        "--no-renames".to_owned(),
        "--untracked-files=normal".to_owned(),
        "--".to_owned(),
    ];
    args.extend(
        relatives
            .iter()
            .map(|path| path.to_string_lossy().into_owned()),
    );
    let budget = 4096usize
        .saturating_add(relatives.len().saturating_mul(4096))
        .min(256 * 1024);
    let result = run_git(&root_real, &args, budget, Duration::from_secs(2))?;
    if result.exit_code != 0 {
        return Err(if result.stderr.is_empty() {
            format!("git status exited {}", result.exit_code)
        } else {
            result.stderr
        });
    }
    let Some(entry) = result.stdout.split('\0').find(|entry| !entry.is_empty()) else {
        return Ok(None);
    };
    let Some(reported) = entry.get(3..).filter(|path| !path.is_empty()) else {
        return Err("git status returned malformed porcelain output".to_owned());
    };
    let reported = PathBuf::from(reported);
    let matched = relatives
        .iter()
        .find(|relative| **relative == reported)
        .cloned()
        .unwrap_or(reported);
    Ok(Some(root_real.join(matched)))
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
    use std::fs;

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
        assert_eq!(result["counts"]["untracked"], 1);
        assert!(result.get("compacted").is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compact_git_status_groups_by_directory() {
        let porcelain = "## main\n M src/a.rs\n M src/b.rs\n?? docs/x.md\n D gone.txt\n";
        let compact = compact_git_status(porcelain).unwrap();
        assert_eq!(compact.files, 4);
        assert_eq!(compact.counts["modified"], 2);
        assert_eq!(compact.counts["untracked"], 1);
        assert_eq!(compact.counts["deleted"], 1);
        assert!(compact.grouped.contains("src (2)"));
        assert!(compact.grouped.contains("a.rs"));
    }

    #[test]
    fn status_compacts_output_when_many_files() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-git-compact-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        for index in 0..20 {
            fs::write(root.join("src").join(format!("f{index}.txt")), "x\n").unwrap();
        }
        for index in 0..10 {
            fs::write(root.join("docs").join(format!("d{index}.txt")), "x\n").unwrap();
        }
        assert!(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        let snapshot = json!({"panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}], "agents": []});
        let result = run(
            &snapshot,
            &json!({"root": root, "action": "status", "max_bytes": 65536}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(result["compacted"], true);
        assert_eq!(result["counts"]["files"], 30);
        assert_eq!(result["counts"]["modified"], 30);
        let output = result["output"].as_str().unwrap();
        assert!(output.contains("src (20)"));
        assert!(output.contains("docs (10)"));
        assert!(output.contains("f0.txt"));
        assert!(!output.contains("?? src/f0.txt"));

        let truncated = run(
            &snapshot,
            &json!({"root": root, "action": "status", "max_bytes": 64}),
        );
        assert_eq!(truncated["ok"], true);
        assert_eq!(truncated["truncated"], true);
        assert!(truncated.get("compacted").is_none());
        assert!(truncated.get("counts").is_none());
        assert!(!truncated["output"].as_str().unwrap().contains("src (20)"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn first_dirty_file_checks_multiple_paths_with_one_status_result() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-git-dirty-{}-{unique}",
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
        let first = root.join("first.txt");
        let second = root.join("second.txt");
        fs::write(&first, "first\n").unwrap();
        fs::write(&second, "second\n").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=Herdr Test",
                    "-c",
                    "user.email=herdr@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "baseline"
                ])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        fs::write(&second, "dirty\n").unwrap();

        let dirty = first_dirty_file(&root, &[first.clone(), second.clone()]).unwrap();
        assert_eq!(dirty, Some(fs::canonicalize(second).unwrap()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn first_dirty_file_handles_renamed_path_with_no_rename_detection() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-git-rename-{}-{unique}",
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
        let original = root.join("original.txt");
        let renamed = root.join("renamed.txt");
        fs::write(&original, "tracked\n").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=Herdr Test",
                    "-c",
                    "user.email=herdr@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "baseline"
                ])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        fs::rename(&original, &renamed).unwrap();

        let dirty = first_dirty_file(&root, std::slice::from_ref(&renamed)).unwrap();
        assert_eq!(dirty, Some(fs::canonicalize(renamed).unwrap()));
        fs::remove_dir_all(root).unwrap();
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("herdr-mcp-{label}-{}-{unique}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn git_init(root: &Path) {
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "user.name", "Herdr Test"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "user.email", "herdr@example.invalid"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }

    fn git_commit(root: &Path, message: &str) {
        assert!(
            Command::new("git")
                .args(["add", "."])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["commit", "-q", "--allow-empty", "-m", message])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }

    fn snapshot_for(root: &Path) -> Value {
        json!({"panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}], "agents": []})
    }

    #[test]
    fn compact_git_diff_counts_files_and_hunks() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@
 context
-removed
+added
+also
diff --git a/docs/x.md b/docs/x.md
index 3333333..4444444 100644
--- a/docs/x.md
+++ b/docs/x.md
@@ -1 +1 @@
-old
+new
";
        let compact = compact_git_diff(diff).unwrap();
        assert_eq!(compact.files, 2);
        assert_eq!(compact.counts["insertions"], 3);
        assert_eq!(compact.counts["deletions"], 2);
        assert_eq!(compact.counts["hunks"], 2);
        assert!(compact.grouped.contains("src (1)"));
        assert!(compact.grouped.contains("a.rs"));
        assert!(compact.grouped.contains("+2/-1"));
        assert!(!compact.grouped.contains("context"));
    }

    #[test]
    fn compact_git_log_strips_decorations() {
        let log = "\
abc1234 (HEAD -> main, origin/main) Fix the thing
def5678 (tag: v1.0.0) Release
aaa1111 no decorations here
";
        let compact = compact_git_log(log).unwrap();
        assert_eq!(compact.commits, 3);
        assert_eq!(compact.counts["commits"], 3);
        assert!(compact.grouped.contains("abc1234 Fix the thing"));
        assert!(compact.grouped.contains("def5678 Release"));
        assert!(compact.grouped.contains("aaa1111 no decorations here"));
        assert!(!compact.grouped.contains("HEAD"));
        assert!(!compact.grouped.contains("tag:"));
    }

    #[test]
    fn small_git_diff_keeps_raw_hunk_body() {
        let root = unique_temp_dir("git-diff-small");
        git_init(&root);
        fs::write(
            root.join("keep.txt"),
            "line one\nKEEP_HUNK_BODY\nline three\n",
        )
        .unwrap();
        git_commit(&root, "baseline");
        fs::write(
            root.join("keep.txt"),
            "line one\nKEEP_HUNK_BODY\nline three\nadded line\n",
        )
        .unwrap();
        let result = run(
            &snapshot_for(&root),
            &json!({"root": root, "action": "diff", "max_bytes": 65536}),
        );
        assert_eq!(result["ok"], true);
        assert!(result.get("compacted").is_none());
        assert_eq!(result["counts"]["files"], 1);
        let output = result["output"].as_str().unwrap();
        assert!(output.contains("KEEP_HUNK_BODY"));
        assert!(output.contains("@@"));
        assert!(output.contains("+added line"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn large_git_diff_compacts_to_grouped_listing() {
        let root = unique_temp_dir("git-diff-large");
        git_init(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        for index in 0..6 {
            fs::write(
                root.join("src").join(format!("f{index}.txt")),
                format!("base-{index}\n"),
            )
            .unwrap();
        }
        for index in 0..4 {
            fs::write(
                root.join("docs").join(format!("d{index}.txt")),
                format!("doc-{index}\n"),
            )
            .unwrap();
        }
        git_commit(&root, "baseline");
        for index in 0..6 {
            fs::write(
                root.join("src").join(format!("f{index}.txt")),
                format!("changed-{index}\nCONTEXT_SHOULD_DROP\n"),
            )
            .unwrap();
        }
        for index in 0..4 {
            fs::write(
                root.join("docs").join(format!("d{index}.txt")),
                format!("updated-{index}\n"),
            )
            .unwrap();
        }
        let result = run(
            &snapshot_for(&root),
            &json!({"root": root, "action": "diff", "max_bytes": 65536}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(result["compacted"], true);
        assert_eq!(result["counts"]["files"], 10);
        let output = result["output"].as_str().unwrap();
        assert!(output.contains("src (6)"));
        assert!(output.contains("docs (4)"));
        assert!(output.contains("f0.txt"));
        assert!(output.contains("d0.txt"));
        assert!(!output.contains("CONTEXT_SHOULD_DROP"));
        assert!(!output.contains("@@"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn large_single_file_git_diff_compacts_by_size() {
        let root = unique_temp_dir("git-diff-bytes");
        git_init(&root);
        let mut original = String::new();
        let mut changed = String::new();
        for index in 0..400 {
            original.push_str(&format!("line {index}\n"));
            changed.push_str(&format!("changed {index}\n"));
        }
        fs::write(root.join("big.txt"), original).unwrap();
        git_commit(&root, "baseline");
        fs::write(root.join("big.txt"), changed).unwrap();
        let result = run(
            &snapshot_for(&root),
            &json!({"root": root, "action": "diff", "max_bytes": 65536}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(result["compacted"], true);
        assert_eq!(result["counts"]["files"], 1);
        assert!(result["counts"]["insertions"].as_u64().unwrap() > 0);
        let output = result["output"].as_str().unwrap();
        assert!(output.contains("big.txt"));
        assert!(!output.contains("changed 0"));
        assert!(!output.contains("@@"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn truncated_git_diff_skips_compact() {
        let root = unique_temp_dir("git-diff-trunc");
        git_init(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        for index in 0..10 {
            fs::write(
                root.join("src").join(format!("f{index}.txt")),
                format!("base-{index}\n"),
            )
            .unwrap();
        }
        git_commit(&root, "baseline");
        for index in 0..10 {
            fs::write(
                root.join("src").join(format!("f{index}.txt")),
                format!("changed-{index}\n"),
            )
            .unwrap();
        }
        let result = run(
            &snapshot_for(&root),
            &json!({"root": root, "action": "diff", "max_bytes": 64}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(result["truncated"], true);
        assert!(result.get("compacted").is_none());
        assert!(result.get("counts").is_none());
        assert!(!result["output"].as_str().unwrap().contains("src ("));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_log_attaches_counts_without_compacting_default() {
        let root = unique_temp_dir("git-log-small");
        git_init(&root);
        for index in 0..25 {
            git_commit(&root, &format!("commit-{index}"));
        }
        let result = run(
            &snapshot_for(&root),
            &json!({"root": root, "action": "log", "max_bytes": 65536}),
        );
        assert_eq!(result["ok"], true);
        assert!(result.get("compacted").is_none());
        assert_eq!(result["counts"]["commits"], 20);
        let output = result["output"].as_str().unwrap();
        assert!(output.contains("commit-24"));
        assert!(!output.contains("commit-0"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_log_compacts_when_many_commits() {
        let root = unique_temp_dir("git-log-large");
        git_init(&root);
        for index in 0..50 {
            git_commit(&root, &format!("commit-{index}"));
        }
        let result = run(
            &snapshot_for(&root),
            &json!({"root": root, "action": "log", "max_count": 50, "max_bytes": 65536}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(result["compacted"], true);
        assert_eq!(result["counts"]["commits"], 50);
        let output = result["output"].as_str().unwrap();
        assert!(output.contains("commit-0"));
        assert!(output.contains("commit-49"));
        assert!(!output.contains("(HEAD"));
        let first = output.lines().next().unwrap();
        let sha = first.split_whitespace().next().unwrap();
        assert!(sha.chars().all(|ch| ch.is_ascii_hexdigit()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn truncated_git_log_skips_compact() {
        let root = unique_temp_dir("git-log-trunc");
        git_init(&root);
        for index in 0..50 {
            git_commit(&root, &format!("commit-{index}"));
        }
        let result = run(
            &snapshot_for(&root),
            &json!({"root": root, "action": "log", "max_count": 50, "max_bytes": 64}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(result["truncated"], true);
        assert!(result.get("compacted").is_none());
        assert!(result.get("counts").is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
