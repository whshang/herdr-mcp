use crate::fs_security;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use regex::{Regex, RegexBuilder};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const READ_DEFAULT_LINES: usize = 200;
const READ_DEFAULT_BYTES: usize = 16 * 1024;
const READ_MAX_BYTES: usize = 256 * 1024;
const LIST_DEFAULT_ENTRIES: usize = 200;
const LIST_MAX_ENTRIES: usize = 2000;
const GREP_DEFAULT_MATCHES: usize = 50;
const GREP_MAX_MATCHES: usize = 1000;
const GREP_DEFAULT_FILE_BYTES: u64 = 64 * 1024;
const GREP_MAX_FILE_BYTES: u64 = 1024 * 1024;
const GREP_COMPACT_AFTER: usize = 24;

pub fn read(snapshot: &Value, args: &Value) -> Value {
    let path = match required_str(args, "path") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let managed = match fs_security::validate_existing(snapshot, path) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let metadata = match fs::metadata(&managed.real) {
        Ok(value) if value.is_file() => value,
        Ok(_) => return fail("not_a_file", &managed.resolved, None),
        Err(error) => return fail("stat_failed", &managed.resolved, Some(error.to_string())),
    };
    let data = match fs::read(&managed.real) {
        Ok(value) => value,
        Err(error) => return fail("read_failed", &managed.resolved, Some(error.to_string())),
    };
    let text = String::from_utf8_lossy(&data);
    let lines = text.split('\n').collect::<Vec<_>>();
    let start = match optional_usize(args, "start_line", 1, usize::MAX) {
        Ok(value) => value.unwrap_or(1),
        Err(error) => return error,
    };
    let requested_end = match optional_usize(args, "end_line", 1, usize::MAX) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let budget = match optional_usize(args, "max_bytes", 1, READ_MAX_BYTES) {
        Ok(value) => value.unwrap_or(READ_DEFAULT_BYTES),
        Err(error) => return error,
    };

    let start_index = start.saturating_sub(1).min(lines.len());
    let default_end = start.saturating_add(READ_DEFAULT_LINES - 1);
    let end = requested_end.unwrap_or(default_end).min(lines.len());
    if requested_end.is_some_and(|requested| requested < start) {
        return json!({"ok": false, "code": "invalid_params", "message": "end_line must be >= start_line"});
    }
    let selected = if start_index < end {
        &lines[start_index..end]
    } else {
        &[]
    };
    let full_content = selected.join("\n");
    let line_truncated = end < lines.len();
    let (content, delivered, byte_truncated) = truncate_complete_lines(selected, budget);
    let truncated = line_truncated || byte_truncated;
    let truncated_by = if byte_truncated {
        Some("bytes")
    } else if line_truncated {
        Some("lines")
    } else {
        None
    };
    let next_start_line = if truncated {
        if byte_truncated {
            Some(start.saturating_add(delivered))
        } else {
            Some(end.saturating_add(1))
        }
    } else {
        None
    };
    let delivered_end = if byte_truncated {
        start.saturating_add(delivered).saturating_sub(1)
    } else {
        end
    };

    json!({
        "ok": true,
        "path": managed.resolved.to_string_lossy(),
        "root": managed.root.to_string_lossy(),
        "lines": {"start": start, "end": delivered_end, "total": lines.len()},
        "next_start_line": next_start_line,
        "truncated_by": truncated_by,
        "bytes": metadata.len(),
        "budget": budget,
        "truncated": truncated,
        "content": if byte_truncated { content } else { full_content },
    })
}

#[derive(Debug, Clone)]
pub struct ImageData {
    pub meta: Value,
    pub data: String,
    pub mime_type: &'static str,
}

pub fn image(snapshot: &Value, args: &Value) -> Result<ImageData, Value> {
    let path = required_str(args, "path")?;
    let managed = fs_security::validate_existing(snapshot, path)?;
    let max_bytes = optional_usize(args, "max_bytes", 1, 8_000_000)?.unwrap_or(2_097_152);
    let metadata = fs::metadata(&managed.real)
        .map_err(|error| fail("stat_failed", &managed.resolved, Some(error.to_string())))?;
    if !metadata.is_file() {
        return Err(fail("not_a_file", &managed.resolved, None));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(json!({
            "ok": false,
            "reason": "image_too_large",
            "path": managed.resolved.to_string_lossy(),
            "bytes": metadata.len(),
            "max_bytes": max_bytes,
        }));
    }
    let mime_type = image_mime(&managed.real).ok_or_else(|| {
        json!({
            "ok": false,
            "reason": "unsupported_image",
            "path": managed.resolved.to_string_lossy(),
            "hint": "png/jpeg/gif/webp only",
        })
    })?;
    let data = fs::read(&managed.real)
        .map_err(|error| fail("read_failed", &managed.resolved, Some(error.to_string())))?;
    if data.len() > max_bytes {
        return Err(json!({
            "ok": false,
            "reason": "image_too_large",
            "path": managed.resolved.to_string_lossy(),
            "bytes": data.len(),
            "max_bytes": max_bytes,
        }));
    }
    let meta = json!({
        "ok": true,
        "path": managed.resolved.to_string_lossy(),
        "root": managed.root.to_string_lossy(),
        "mime_type": mime_type,
        "bytes": data.len(),
    });
    Ok(ImageData {
        meta,
        data: STANDARD.encode(data),
        mime_type,
    })
}

fn image_mime(path: &Path) -> Option<&'static str> {
    match path
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

pub fn list(snapshot: &Value, args: &Value) -> Value {
    let path = match required_str(args, "path") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let managed = match fs_security::validate_existing(snapshot, path) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !managed.real.is_dir() {
        return fail("not_a_directory", &managed.resolved, None);
    }
    let recursive = match optional_bool(args, "recursive") {
        Ok(value) => value.unwrap_or(false),
        Err(error) => return error,
    };
    let max_entries = match optional_usize(args, "max_entries", 1, LIST_MAX_ENTRIES) {
        Ok(value) => value.unwrap_or(LIST_DEFAULT_ENTRIES),
        Err(error) => return error,
    };
    let glob = match optional_str(args, "glob") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let glob_re = match glob.map(glob_regex).transpose() {
        Ok(value) => value,
        Err(message) => return json!({"ok": false, "code": "invalid_params", "message": message}),
    };

    let mut entries = Vec::new();
    let mut truncated = false;
    walk_list(
        &managed.real,
        &managed.real,
        recursive,
        glob_re.as_ref(),
        max_entries,
        &mut entries,
        &mut truncated,
    );
    json!({
        "ok": true,
        "path": managed.resolved.to_string_lossy(),
        "root": managed.root.to_string_lossy(),
        "count": entries.len(),
        "truncated": truncated,
        "entries": entries,
    })
}

pub fn grep(snapshot: &Value, args: &Value) -> Value {
    let root = match required_str(args, "root") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let pattern = match required_str(args, "pattern") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let managed = match fs_security::validate_existing(snapshot, root) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !managed.real.is_dir() {
        return fail("not_a_directory", &managed.resolved, None);
    }
    let regex_mode = match optional_bool(args, "regex") {
        Ok(value) => value.unwrap_or(false),
        Err(error) => return error,
    };
    let case_insensitive = match optional_bool(args, "case_insensitive") {
        Ok(value) => value.unwrap_or(false),
        Err(error) => return error,
    };
    let max_matches = match optional_usize(args, "max_matches", 1, GREP_MAX_MATCHES) {
        Ok(value) => value.unwrap_or(GREP_DEFAULT_MATCHES),
        Err(error) => return error,
    };
    let max_bytes = match optional_u64(args, "max_bytes", 1, GREP_MAX_FILE_BYTES) {
        Ok(value) => value.unwrap_or(GREP_DEFAULT_FILE_BYTES),
        Err(error) => return error,
    };
    let glob = match optional_str(args, "glob") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let glob_matcher = match glob.map(grep_glob_matcher).transpose() {
        Ok(value) => value,
        Err(message) => return json!({"ok": false, "code": "invalid_params", "message": message}),
    };
    let matcher = if regex_mode {
        match RegexBuilder::new(pattern)
            .case_insensitive(case_insensitive)
            .build()
        {
            Ok(value) => LineMatcher::Regex(value),
            Err(error) => {
                return json!({"ok": false, "code": "invalid_params", "message": format!("invalid regex: {error}")});
            }
        }
    } else if case_insensitive {
        LineMatcher::LiteralInsensitive(pattern.to_lowercase())
    } else {
        LineMatcher::Literal(pattern.to_owned())
    };

    let mut matches = Vec::new();
    let mut truncated = false;
    let search_root = glob
        .map(|value| resolve_grep_search_root(&managed.real, value))
        .unwrap_or_else(|| managed.real.clone());
    let walk = GrepWalkOptions {
        base: &managed.real,
        glob: glob_matcher.as_ref(),
        matcher: &matcher,
        max_matches,
        max_bytes,
    };
    walk_grep(&search_root, &walk, &mut matches, &mut truncated);
    let compact = if truncated {
        None
    } else {
        compact_grep_matches(&matches)
    };
    let mut output = Map::new();
    output.insert("ok".to_owned(), json!(true));
    output.insert("root".to_owned(), json!(managed.resolved.to_string_lossy()));
    output.insert("count".to_owned(), json!(matches.len()));
    output.insert("truncated".to_owned(), json!(truncated));
    output.insert("engine".to_owned(), json!("rust"));
    match compact {
        Some(compact) if compact.match_count > GREP_COMPACT_AFTER => {
            output.insert("matches".to_owned(), json!(compact.grouped));
            output.insert("counts".to_owned(), compact.counts);
            output.insert("compacted".to_owned(), json!(true));
        }
        Some(compact) => {
            output.insert("matches".to_owned(), json!(matches));
            output.insert("counts".to_owned(), compact.counts);
        }
        None => {
            output.insert("matches".to_owned(), json!(matches));
        }
    }
    Value::Object(output)
}

struct GrepCompact {
    counts: Value,
    grouped: String,
    match_count: usize,
}

fn compact_grep_matches(matches: &[Value]) -> Option<GrepCompact> {
    let mut order = Vec::<String>::new();
    let mut groups = std::collections::HashMap::<String, Vec<(u64, String)>>::new();
    for item in matches {
        let file = item.get("file")?.as_str()?.replace('\\', "/");
        let line = item.get("line")?.as_u64()?;
        let content = item.get("content")?.as_str()?.to_owned();
        groups
            .entry(file.clone())
            .or_insert_with(|| {
                order.push(file);
                Vec::new()
            })
            .push((line, content));
    }

    let mut grouped = String::new();
    for file in &order {
        let hits = groups.get(file)?;
        grouped.push_str(&format!("{file} ({})\n", hits.len()));
        for (line, content) in hits {
            grouped.push_str(&format!(" {line}: {content}\n"));
        }
    }

    Some(GrepCompact {
        counts: json!({
            "files": order.len(),
            "matches": matches.len(),
        }),
        grouped,
        match_count: matches.len(),
    })
}

fn walk_list(
    base: &Path,
    dir: &Path,
    recursive: bool,
    glob: Option<&Regex>,
    max_entries: usize,
    output: &mut Vec<Value>,
    truncated: &mut bool,
) {
    if output.len() >= max_entries {
        *truncated = true;
        return;
    }
    let mut children = match fs::read_dir(dir) {
        Ok(value) => value.filter_map(Result::ok).collect::<Vec<_>>(),
        Err(_) => return,
    };
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        if output.len() >= max_entries {
            *truncated = true;
            return;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let full = entry.path();
        if fs_security::denied_secret_path(&full) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let kind = if file_type.is_dir() {
            "dir"
        } else if file_type.is_symlink() {
            "symlink"
        } else if file_type.is_file() {
            "file"
        } else {
            "other"
        };
        if file_type.is_dir() && recursive {
            walk_list(base, &full, true, glob, max_entries, output, truncated);
        }
        if glob.is_some_and(|pattern| !pattern.is_match(&name)) {
            continue;
        }
        let mut record = Map::new();
        record.insert("name".to_owned(), json!(name));
        record.insert("type".to_owned(), json!(kind));
        record.insert("path".to_owned(), json!(full.to_string_lossy()));
        record.insert(
            "relative_path".to_owned(),
            json!(full.strip_prefix(base).unwrap_or(&full).to_string_lossy()),
        );
        if file_type.is_file()
            && let Ok(metadata) = entry.metadata()
        {
            record.insert("size".to_owned(), json!(metadata.len()));
            if let Ok(modified) = metadata.modified() {
                let timestamp = OffsetDateTime::from(modified)
                    .format(&Rfc3339)
                    .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
                record.insert("mtime".to_owned(), json!(timestamp));
            }
        }
        output.push(Value::Object(record));
    }
}

struct GrepWalkOptions<'a> {
    base: &'a Path,
    glob: Option<&'a GlobMatcher>,
    matcher: &'a LineMatcher,
    max_matches: usize,
    max_bytes: u64,
}

fn walk_grep(
    dir: &Path,
    options: &GrepWalkOptions<'_>,
    output: &mut Vec<Value>,
    truncated: &mut bool,
) {
    if output.len() >= options.max_matches {
        *truncated = true;
        return;
    }
    let mut children = match fs::read_dir(dir) {
        Ok(value) => value.filter_map(Result::ok).collect::<Vec<_>>(),
        Err(_) => return,
    };
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        if output.len() >= options.max_matches {
            *truncated = true;
            return;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let full = entry.path();
        if fs_security::denied_secret_path(&full) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            walk_grep(&full, options, output, truncated);
            continue;
        }
        if !file_type.is_file()
            || options
                .glob
                .is_some_and(|pattern| !pattern.matches(options.base, &full, &name))
        {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > options.max_bytes {
            *truncated = true;
            continue;
        }
        let Ok(data) = fs::read(&full) else {
            continue;
        };
        let text = String::from_utf8_lossy(&data);
        for (index, line) in text.split('\n').enumerate() {
            if output.len() >= options.max_matches {
                *truncated = true;
                return;
            }
            if options.matcher.matches(line) {
                output.push(json!({
                    "file": full.to_string_lossy(),
                    "line": index + 1,
                    "content": line,
                }));
            }
        }
    }
}

enum LineMatcher {
    Regex(Regex),
    Literal(String),
    LiteralInsensitive(String),
}

impl LineMatcher {
    fn matches(&self, line: &str) -> bool {
        match self {
            Self::Regex(regex) => regex.is_match(line),
            Self::Literal(pattern) => line.contains(pattern),
            Self::LiteralInsensitive(pattern) => line.to_lowercase().contains(pattern),
        }
    }
}

fn glob_regex(glob: &str) -> Result<Regex, String> {
    let mut pattern = String::from("^");
    for ch in glob.chars() {
        match ch {
            '*' => pattern.push_str(".*"),
            '?' => pattern.push('.'),
            other => pattern.push_str(&regex::escape(&other.to_string())),
        }
    }
    pattern.push('$');
    Regex::new(&pattern).map_err(|error| format!("invalid glob: {error}"))
}

struct GlobMatcher {
    regex: Regex,
    path_aware: bool,
}

impl GlobMatcher {
    fn matches(&self, base: &Path, full: &Path, name: &str) -> bool {
        if !self.path_aware {
            return self.regex.is_match(name);
        }
        let relative = full
            .strip_prefix(base)
            .unwrap_or(full)
            .to_string_lossy()
            .replace('\\', "/");
        self.regex.is_match(&relative)
    }
}

fn grep_glob_matcher(glob: &str) -> Result<GlobMatcher, String> {
    let normalized = glob.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err("glob must be root-relative".to_owned());
    }
    let path_aware = normalized.contains('/');
    if !path_aware {
        return glob_regex(&normalized).map(|regex| GlobMatcher { regex, path_aware });
    }

    let chars = normalized.chars().collect::<Vec<_>>();
    let mut pattern = String::from("^");
    let mut index = 0usize;
    while index < chars.len() {
        match chars[index] {
            '*' if chars.get(index + 1) == Some(&'*') => {
                index += 2;
                if chars.get(index) == Some(&'/') {
                    pattern.push_str("(?:.*/)?");
                    index += 1;
                } else {
                    pattern.push_str(".*");
                }
            }
            '*' => {
                pattern.push_str("[^/]*");
                index += 1;
            }
            '?' => {
                pattern.push_str("[^/]");
                index += 1;
            }
            other => {
                pattern.push_str(&regex::escape(&other.to_string()));
                index += 1;
            }
        }
    }
    pattern.push('$');
    Regex::new(&pattern)
        .map(|regex| GlobMatcher { regex, path_aware })
        .map_err(|error| format!("invalid glob: {error}"))
}

fn grep_search_root(base: &Path, glob: &str) -> PathBuf {
    let normalized = glob.replace('\\', "/");
    if !normalized.contains('/') || normalized.starts_with('/') {
        return base.to_path_buf();
    }
    let wildcard = normalized
        .char_indices()
        .find_map(|(index, ch)| matches!(ch, '*' | '?').then_some(index))
        .unwrap_or(normalized.len());
    let literal_prefix = &normalized[..wildcard];
    if literal_prefix.split('/').any(|part| part == "..") {
        return base.to_path_buf();
    }
    let directory_prefix = if literal_prefix.ends_with('/') {
        literal_prefix.trim_end_matches('/')
    } else {
        literal_prefix
            .rsplit_once('/')
            .map(|(dir, _)| dir)
            .unwrap_or("")
    };
    if directory_prefix.is_empty() {
        base.to_path_buf()
    } else {
        base.join(directory_prefix)
    }
}

fn resolve_grep_search_root(base: &Path, glob: &str) -> PathBuf {
    let candidate = grep_search_root(base, glob);
    if candidate == base {
        return base.to_path_buf();
    }
    let Ok(metadata) = fs::symlink_metadata(&candidate) else {
        return base.to_path_buf();
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return base.to_path_buf();
    }
    let Ok(real) = fs::canonicalize(&candidate) else {
        return base.to_path_buf();
    };
    if fs_security::path_within(base, &real) {
        real
    } else {
        base.to_path_buf()
    }
}

fn truncate_complete_lines(lines: &[&str], budget: usize) -> (String, usize, bool) {
    let joined = lines.join("\n");
    if joined.len() <= budget {
        return (joined, lines.len(), false);
    }
    let mut output = String::new();
    let mut delivered = 0usize;
    for line in lines {
        let additional = line.len() + usize::from(!output.is_empty());
        if output.len().saturating_add(additional) > budget {
            break;
        }
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(line);
        delivered += 1;
    }
    (output, delivered, true)
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, Value> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| {
        json!({"ok": false, "code": "invalid_params", "message": format!("{key} must be a string")})
    })
}

fn optional_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(
            json!({"ok": false, "code": "invalid_params", "message": format!("{key} must be a string")}),
        ),
    }
}

fn optional_bool(args: &Value, key: &str) -> Result<Option<bool>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(
            json!({"ok": false, "code": "invalid_params", "message": format!("{key} must be a boolean")}),
        ),
    }
}

fn optional_usize(args: &Value, key: &str, min: usize, max: usize) -> Result<Option<usize>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let number = value.as_u64().and_then(|value| usize::try_from(value).ok());
            match number.filter(|value| *value >= min && *value <= max) {
                Some(value) => Ok(Some(value)),
                None => Err(
                    json!({"ok": false, "code": "invalid_params", "message": format!("{key} must be an integer in {min}..={max}")}),
                ),
            }
        }
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
            None => Err(
                json!({"ok": false, "code": "invalid_params", "message": format!("{key} must be an integer in {min}..={max}")}),
            ),
        },
    }
}

fn fail(reason: &str, path: &Path, message: Option<String>) -> Value {
    let mut output = Map::new();
    output.insert("ok".to_owned(), json!(false));
    output.insert("reason".to_owned(), json!(reason));
    output.insert("path".to_owned(), json!(path.to_string_lossy()));
    if let Some(message) = message {
        output.insert("message".to_owned(), json!(message));
    }
    Value::Object(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_budget_never_returns_partial_line() {
        let lines = ["1234", "5678", "9"];
        let (content, delivered, truncated) = truncate_complete_lines(&lines, 7);
        assert_eq!(content, "1234");
        assert_eq!(delivered, 1);
        assert!(truncated);
    }

    #[test]
    fn glob_matches_expected_names() {
        let regex = glob_regex("*.rs").unwrap();
        assert!(regex.is_match("main.rs"));
        assert!(!regex.is_match("main.ts"));
    }

    #[test]
    fn grep_glob_matches_root_relative_paths_and_prunes_literal_prefix() {
        let matcher = grep_glob_matcher("extension/**/*.js").unwrap();
        let base = Path::new("/repo");
        assert!(matcher.matches(
            base,
            Path::new("/repo/extension/background.js"),
            "background.js"
        ));
        assert!(matcher.matches(
            base,
            Path::new("/repo/extension/content/wake.js"),
            "wake.js"
        ));
        assert!(!matcher.matches(base, Path::new("/repo/src/server.js"), "server.js"));
        assert_eq!(
            grep_search_root(base, "extension/**/*.js"),
            Path::new("/repo/extension")
        );
        assert_eq!(grep_search_root(base, "*.js"), base);
    }

    #[test]
    fn image_mime_accepts_only_supported_types() {
        assert_eq!(image_mime(Path::new("shot.PNG")), Some("image/png"));
        assert_eq!(image_mime(Path::new("photo.jpeg")), Some("image/jpeg"));
        assert_eq!(image_mime(Path::new("anim.gif")), Some("image/gif"));
        assert_eq!(image_mime(Path::new("screen.webp")), Some("image/webp"));
        assert_eq!(image_mime(Path::new("vector.svg")), None);
    }

    #[test]
    fn read_list_and_grep_share_managed_root_and_secret_gate() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-fs-tools-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("extension/content")).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.join("src/lib.rs"), "alpha\nbeta\nalpha two\n").unwrap();
        fs::write(
            root.join("extension/content/wake.js"),
            "const autoContinue = 'SCROLL independent';\n",
        )
        .unwrap();
        fs::write(root.join(".env"), "DO_NOT_READ=1\n").unwrap();
        let snapshot = json!({"panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}], "agents": []});

        let read_result = read(
            &snapshot,
            &json!({"path": root.join("src/lib.rs"), "start_line": 2, "end_line": 3}),
        );
        assert_eq!(read_result["ok"], true);
        assert_eq!(read_result["content"], "beta\nalpha two");

        let list_result = list(
            &snapshot,
            &json!({"path": root, "recursive": true, "glob": "*.rs"}),
        );
        assert_eq!(list_result["ok"], true);
        assert_eq!(list_result["count"], 1);
        assert_eq!(list_result["entries"][0]["name"], "lib.rs");

        let grep_result = grep(
            &snapshot,
            &json!({"root": root, "pattern": "alpha", "glob": "*.rs"}),
        );
        assert_eq!(grep_result["ok"], true);
        assert_eq!(grep_result["count"], 2);
        assert_eq!(grep_result["engine"], "rust");
        assert_eq!(grep_result["counts"]["files"], 1);
        assert_eq!(grep_result["counts"]["matches"], 2);
        assert!(grep_result.get("compacted").is_none());
        assert!(grep_result["matches"].as_array().is_some());
        assert!(
            grep_result["matches"][0]["file"]
                .as_str()
                .unwrap()
                .contains("lib.rs")
        );

        let secret_grep = grep(&snapshot, &json!({"root": root, "pattern": "DO_NOT_READ"}));
        assert_eq!(secret_grep["ok"], true);
        assert_eq!(secret_grep["count"], 0);
        assert_eq!(secret_grep["counts"]["files"], 0);
        assert_eq!(secret_grep["counts"]["matches"], 0);
        assert!(secret_grep.get("compacted").is_none());
        let secret_matches = secret_grep["matches"].as_array().unwrap();
        assert!(secret_matches.is_empty());
        assert!(
            !serde_json::to_string(&secret_grep)
                .unwrap()
                .contains(".env")
        );

        let path_glob_result = grep(
            &snapshot,
            &json!({
                "root": root,
                "pattern": "scroll",
                "case_insensitive": true,
                "glob": "extension/**/*.js",
                "max_matches": 200,
            }),
        );
        assert_eq!(path_glob_result["ok"], true);
        assert_eq!(path_glob_result["count"], 1);
        assert!(
            path_glob_result["matches"][0]["file"]
                .as_str()
                .unwrap()
                .ends_with("extension/content/wake.js")
        );
        assert_eq!(resolve_grep_search_root(&root, "missing/**/*.js"), root);
        let absolute_glob = grep(
            &snapshot,
            &json!({"root": root, "pattern": "scroll", "glob": "/extension/**/*.js"}),
        );
        assert_eq!(absolute_glob["ok"], false);
        assert_eq!(absolute_glob["code"], "invalid_params");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = std::env::temp_dir().join(format!(
                "herdr-mcp-fs-tools-outside-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&outside).unwrap();
            fs::write(outside.join("escaped.js"), "scroll\n").unwrap();
            symlink(&outside, root.join("escape")).unwrap();
            assert_eq!(resolve_grep_search_root(&root, "escape/**/*.js"), root);
            let escaped = grep(
                &snapshot,
                &json!({"root": root, "pattern": "scroll", "glob": "escape/**/*.js"}),
            );
            assert_eq!(escaped["ok"], true);
            assert_eq!(escaped["count"], 0);
            fs::remove_dir_all(outside).unwrap();
        }

        let secret_result = read(&snapshot, &json!({"path": root.join(".env")}));
        assert_eq!(secret_result["reason"], "secret_path_denied");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compact_grep_matches_groups_by_file() {
        let matches = vec![
            json!({"file": "src/lib.rs", "line": 10, "content": "fn foo"}),
            json!({"file": "src/lib.rs", "line": 20, "content": "fn bar"}),
            json!({"file": "src/lib.rs", "line": 30, "content": "fn baz"}),
            json!({"file": "docs/x.md", "line": 1, "content": "note"}),
        ];
        let compact = compact_grep_matches(&matches).unwrap();
        assert_eq!(compact.match_count, 4);
        assert_eq!(compact.counts["files"], 2);
        assert_eq!(compact.counts["matches"], 4);
        assert!(compact.grouped.contains("src/lib.rs (3)"));
        assert!(compact.grouped.contains(" 10: fn foo"));
        assert!(compact.grouped.contains(" 20: fn bar"));
        assert!(compact.grouped.contains("docs/x.md (1)"));
        assert!(compact.grouped.contains(" 1: note"));
    }

    #[test]
    fn grep_compacts_matches_when_many_hits_across_dirs() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-fs-grep-compact-{}-{unique}",
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
        let mut src_lib = String::new();
        for index in 0..15 {
            src_lib.push_str(&format!("fn hit_{index}()\n"));
        }
        fs::write(root.join("src/lib.rs"), src_lib).unwrap();
        let mut src_util = String::new();
        for index in 0..10 {
            src_util.push_str(&format!("fn hit_util_{index}()\n"));
        }
        fs::write(root.join("src/util.rs"), src_util).unwrap();
        let mut docs = String::new();
        for index in 0..5 {
            docs.push_str(&format!("hit doc {index}\n"));
        }
        fs::write(root.join("docs/notes.md"), docs).unwrap();
        fs::write(root.join(".env"), "hit DO_NOT_READ=1\n").unwrap();
        fs::write(
            root.join("src/big.txt"),
            format!("{}\nhit huge\n", "x".repeat(400)),
        )
        .unwrap();

        let snapshot = json!({"panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": root}], "agents": []});
        let result = grep(
            &snapshot,
            &json!({"root": root, "pattern": "hit", "max_matches": 200}),
        );
        assert_eq!(result["ok"], true);
        assert_eq!(result["truncated"], false);
        assert_eq!(result["compacted"], true);
        assert_eq!(result["counts"]["files"], 4);
        assert_eq!(result["counts"]["matches"], 31);
        assert_eq!(result["count"], 31);
        assert_eq!(result["engine"], "rust");
        let listing = result["matches"].as_str().unwrap();
        assert!(listing.contains("lib.rs (15)"));
        assert!(listing.contains("util.rs (10)"));
        assert!(listing.contains("notes.md (5)"));
        assert!(listing.contains("big.txt (1)"));
        assert!(listing.contains(" 1: fn hit_0()"));
        assert!(listing.contains(" 15: fn hit_14()"));
        assert!(listing.contains(" 1: fn hit_util_0()"));
        assert!(listing.contains(" 1: hit doc 0"));
        assert!(listing.contains("hit huge"));
        assert!(!listing.contains(".env"));
        assert!(!listing.contains("DO_NOT_READ"));

        let truncated_count = grep(
            &snapshot,
            &json!({"root": root, "pattern": "hit", "max_matches": 10}),
        );
        assert_eq!(truncated_count["ok"], true);
        assert_eq!(truncated_count["truncated"], true);
        assert_eq!(truncated_count["count"], 10);
        assert!(truncated_count.get("compacted").is_none());
        assert!(truncated_count.get("counts").is_none());
        assert!(truncated_count["matches"].as_array().is_some());
        assert!(
            !truncated_count["matches"][0]["file"]
                .as_str()
                .unwrap()
                .contains(".env")
        );

        let truncated_bytes = grep(
            &snapshot,
            &json!({
                "root": root,
                "pattern": "hit",
                "max_matches": 200,
                "max_bytes": 200,
            }),
        );
        assert_eq!(truncated_bytes["ok"], true);
        assert_eq!(truncated_bytes["truncated"], true);
        assert!(truncated_bytes.get("compacted").is_none());
        assert!(truncated_bytes.get("counts").is_none());
        assert!(truncated_bytes["matches"].as_array().is_some());
        let listing = serde_json::to_string(&truncated_bytes["matches"]).unwrap();
        assert!(!listing.contains("lib.rs (15)"));
        assert!(!listing.contains(".env"));

        fs::remove_dir_all(root).unwrap();
    }
}
