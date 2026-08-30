//! v0.4.2 local skill registry: deterministic, bounded, canonical-path-confined
//! discovery of local `SKILL.md` / `skill.md` skills from `.agents/skills`
//! scopes (project and user). Discovery returns metadata only; load returns the
//! body on demand. Never a vector for arbitrary home scanning.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const MAX_LOCAL_SKILL_BYTES: usize = 512 * 1024;
pub const MAX_LOCAL_SKILLS_PER_SCOPE: usize = 512;

/// One discovered local skill file with identity facts. `path` is canonical
/// (symlink-resolved) and guaranteed to stay within its skills base.
#[derive(Debug, Clone)]
pub struct LocalSkillFile {
    pub id: String,
    /// Canonical absolute path to the `SKILL.md` / `skill.md`.
    pub path: PathBuf,
    /// `project:<root>` or `user:<home>` source identity.
    pub scope_identity: String,
}

#[derive(Debug, Clone, Default)]
pub struct LocalDiscovery {
    pub project: Vec<LocalSkillFile>,
    pub user: Vec<LocalSkillFile>,
}

/// Parsed common SKILL.md frontmatter surface. Unknown keys are ignored.
#[derive(Debug, Clone, Default)]
pub struct SkillFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub summary: Option<String>,
    pub version: Option<String>,
}

pub struct LocalSkillRegistry;

impl LocalSkillRegistry {
    /// Discover local skills under an optional project root first, then the user
    /// home. Precedence deduplication (builtin > project > user) happens in
    /// [`crate::progressive_skills::ProgressiveSkillService`].
    pub fn discover(project_root: Option<&Path>, home: &Path) -> LocalDiscovery {
        let mut discovery = LocalDiscovery::default();
        if let Some(root) = project_root
            && let Ok(canon) = fs::canonicalize(root)
        {
            let identity = format!("project:{}", canon.display());
            collect_base(
                &canon.join(".agents/skills"),
                &identity,
                &mut discovery.project,
            );
        }
        if let Ok(home_canon) = fs::canonicalize(home) {
            let identity = format!("user:{}", home_canon.display());
            collect_base(
                &home_canon.join(".agents/skills"),
                &identity,
                &mut discovery.user,
            );
        }
        discovery
    }
}

/// Enumerate `<base>/*/SKILL.md` (or `skill.md`), enforcing canonical-path /
/// symlink confinement (rejects symlinks that resolve outside `base`), a
/// per-scope count cap, and a per-file size bound.
fn collect_base(base: &Path, scope_identity: &str, out: &mut Vec<LocalSkillFile>) {
    let Ok(canon_base) = fs::canonicalize(base) else {
        return;
    };
    let Ok(entries) = fs::read_dir(&canon_base) else {
        return;
    };
    let mut found = 0usize;
    for entry in entries.flatten() {
        if found >= MAX_LOCAL_SKILLS_PER_SCOPE {
            break;
        }
        let Ok(metadata) = fs::metadata(entry.path()) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        // Resolve the child dir; if it is a symlink resolving outside base, reject.
        let Ok(canon_child) = fs::canonicalize(entry.path()) else {
            continue;
        };
        if !canon_child.starts_with(&canon_base) {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.is_empty() {
            continue;
        }
        // Prefer SKILL.md, fall back to skill.md.
        let skill_path = ["SKILL.md", "skill.md"]
            .iter()
            .map(|name| canon_child.join(name))
            .find(|path| path.is_file());
        let Some(skill_path) = skill_path else {
            continue;
        };
        let Ok(skill_canon) = fs::canonicalize(&skill_path) else {
            continue;
        };
        if !skill_canon.starts_with(&canon_base) {
            continue;
        }
        let Ok(file_meta) = fs::metadata(&skill_canon) else {
            continue;
        };
        if file_meta.len() > MAX_LOCAL_SKILL_BYTES as u64 {
            continue;
        }
        out.push(LocalSkillFile {
            id,
            path: skill_canon,
            scope_identity: scope_identity.to_owned(),
        });
        found += 1;
    }
}

/// Read a local skill file with the same bounded ceiling used at discovery.
pub fn read_file_bounded(path: &Path) -> Option<Arc<str>> {
    if fs::metadata(path).ok()?.len() > MAX_LOCAL_SKILL_BYTES as u64 {
        return None;
    }
    let raw = fs::read(path).ok()?;
    if raw.len() > MAX_LOCAL_SKILL_BYTES {
        return None;
    }
    Some(Arc::from(String::from_utf8_lossy(&raw)))
}

/// Resolve the user home dir without canonicalizing (canonicalization happens
/// during discovery for a stable identity).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Parse common SKILL.md YAML frontmatter for `name`, `description`, `summary`,
/// and `version` (the latter also honored under a `metadata:` block). Handles
/// quoted scalars, `>` / `|` block scalars, and ignores unknown keys. Sufficient
/// for real `~/.agents/skills` skills including `ego-browser` and `opencli-usage`.
pub fn parse_frontmatter(content: &str) -> SkillFrontmatter {
    let mut fm = SkillFrontmatter::default();
    let Some(body) = frontmatter_text(content) else {
        return fm;
    };
    let lines: Vec<&str> = body.split('\n').collect();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
            i += 1;
            continue;
        }
        let (key, raw_value) = match line.split_once(':') {
            Some(pair) => pair,
            None => {
                i += 1;
                continue;
            }
        };
        let key = key.trim().to_ascii_lowercase();
        let value = raw_value.trim();
        // Nested keys (indented) are consumed by their parent collector.
        if line.len() - trimmed.len() > 0 {
            i += 1;
            continue;
        }
        if matches!(value, ">" | "|" | ">-" | "|-" | ">+" | "|+") {
            // `collect_block` advances `i` to the first unconsumed line.
            let block = collect_block(&lines, &mut i);
            match key.as_str() {
                "name" => fm.name = some_cleaned(&block),
                "description" => fm.description = some_cleaned(&block),
                "summary" => fm.summary = some_cleaned(&block),
                _ => {}
            }
            continue;
        }
        match key.as_str() {
            "name" => {
                fm.name = some_cleaned(value);
                i += 1;
            }
            "description" => {
                fm.description = some_cleaned(value);
                i += 1;
            }
            "summary" => {
                fm.summary = some_cleaned(value);
                i += 1;
            }
            "version" => {
                fm.version = some_cleaned(value);
                i += 1;
            }
            "metadata" => {
                if value.is_empty() {
                    // `collect_metadata_block` advances `i` to the first
                    // unconsumed line.
                    if let Some(block) = collect_metadata_block(&lines, &mut i)
                        && fm.version.is_none()
                    {
                        for line in block.split('\n') {
                            if let Some(rest) = line.trim_start().strip_prefix("version:") {
                                fm.version = some_cleaned(rest.trim());
                                break;
                            }
                        }
                    }
                } else if fm.version.is_none() {
                    // Inline metadata JSON value; best-effort version extraction.
                    fm.version = inline_json_version(value);
                    i += 1;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    fm
}

fn frontmatter_text(content: &str) -> Option<&str> {
    let body = content.strip_prefix("---")?;
    let end = body.find("\n---")?;
    Some(&body[..end])
}

fn is_indented(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

/// Consume the `key: >` marker line plus indented continuation lines; join the
/// scalar with single spaces. Advances `i` to the first unconsumed line.
fn collect_block(lines: &[&str], i: &mut usize) -> String {
    let mut out = String::new();
    *i += 1;
    while *i < lines.len() && is_indented(lines[*i]) {
        let token = lines[*i].trim();
        if !token.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(token);
        }
        *i += 1;
    }
    out
}

/// Consume indented lines following a top-level key (used for `metadata:`), or
/// return `None` if none follow. Advances `i` to the first unconsumed line.
fn collect_metadata_block(lines: &[&str], i: &mut usize) -> Option<String> {
    let mut out = String::new();
    *i += 1;
    while *i < lines.len() && is_indented(lines[*i]) {
        out.push_str(lines[*i].trim_start());
        out.push('\n');
        *i += 1;
    }
    if out.is_empty() { None } else { Some(out) }
}

fn some_cleaned(value: &str) -> Option<String> {
    let cleaned = clean(value);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn clean(value: &str) -> String {
    let value = value.trim();
    let stripped = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
        .unwrap_or(value);
    stripped.to_owned()
}

/// Best-effort extraction of an inline `"version": "x.y.z"` token from inline
/// metadata JSON.
fn inline_json_version(value: &str) -> Option<String> {
    let start = value.find("version")?;
    let rest = &value[start..];
    let (_, remainder) = rest.split_once(':')?;
    let candidate = clean(remainder.trim().trim_start_matches('"'));
    some_cleaned(candidate.trim().trim_matches('"'))
}
