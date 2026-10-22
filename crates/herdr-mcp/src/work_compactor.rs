use crate::semantic::{SemanticChatMessage, SemanticChatOptions, SemanticService};
use crate::state_cache::EventCache;
use crate::state_store::StateStore;
use crate::state_store::work_journal::{WorkEvent, WorkState, clip_chars};
use serde::{Deserialize, Serialize};

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Rendered-document budget (~4k tokens), including the embedded structured
/// memory block.
const HANDOFF_MAX_CHARS: usize = 12_000;
/// Rolling recent-log bound, newest last.
const RECENT_MAX_LINES: usize = 15;
const CURRENT_STATE_MAX_CHARS: usize = 1_000;
const EVENT_RECENT_MAX_CHARS: usize = 200;
/// Tasks untouched for longer than this are closed as stale (a task list that
/// never ages is a task list nobody trusts).
const STALE_TASK_DAYS: i64 = 30;
const DAY_MS: i64 = 86_400_000;
/// Trailing structured-memory block: everything after it is machine state,
/// so the markdown above stays free to render for humans/agents.
const MEMORY_OPEN: &str = "<!-- relay-memory: ";
const MEMORY_CLOSE: &str = "-->";
/// New events folded into the handoff per compaction batch.
const EVENT_BATCH_SIZE: usize = 100;
/// Bounded batches per scope per tick; larger backlogs resume on later ticks.
const EVENT_BATCHES_PER_TICK: usize = 4;
const COMPACTION_INTERVAL: Duration = Duration::from_secs(10 * 60);
const SUMMARY_MAX_TOKENS: u32 = 4_000;
/// The semantic chat message limit is 65_536 BYTES; stay well under it so the
/// summarizer request is never rejected as invalid.
const SEMANTIC_PROMPT_MAX_BYTES: usize = 60_000;

pub(crate) trait WorkSummarizer: Send + Sync {
    fn summarize(&self, prompt: &str) -> Result<String, String>;
}

/// The summarizer request shape: one user message plus JSON-mode options. Both
/// the live call and the prompt-size test go through this, so a test proves the
/// real request is accepted instead of a lookalike.
fn summarizer_message(prompt: &str) -> SemanticChatMessage {
    SemanticChatMessage {
        role: "user".to_owned(),
        content: prompt.to_owned(),
    }
}

fn summarizer_options() -> SemanticChatOptions {
    SemanticChatOptions {
        max_tokens: Some(SUMMARY_MAX_TOKENS),
        json_response: true,
        ..SemanticChatOptions::default()
    }
}

impl WorkSummarizer for SemanticService {
    fn summarize(&self, prompt: &str) -> Result<String, String> {
        self.chat_background(&[summarizer_message(prompt)], summarizer_options())
            .map(|response| response.content)
            .map_err(|error| error.code().to_owned())
    }
}

/// The structured relay memory: one shared per-project state that any agent
/// entry point (tool capture, native-log tailer, MCP pickup) reads/writes.
/// Long-lived sections hold id-keyed items so LLM edits apply as incremental
/// merge ops and a rewrite can never silently wipe them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RelayMemory {
    /// ## 目标与计划
    #[serde(default)]
    pub goals: Vec<RelayItem>,
    /// ## 偏好与规范
    #[serde(default)]
    pub preferences: Vec<RelayItem>,
    /// ## 错题集 (what went wrong + the correct approach)
    #[serde(default)]
    pub mistakes: Vec<RelayItem>,
    /// ## 剩余任务
    #[serde(default)]
    pub tasks: Vec<RelayTask>,
    /// ## 当前状态
    #[serde(default)]
    pub current_state: String,
    /// Internal rolling log (not rendered).
    #[serde(default)]
    pub recent: Vec<String>,
    /// ## 历史会话路径: "provider time path" of source session logs, newest last.
    #[serde(default)]
    pub sources: Vec<String>,
    /// Git context of the worktree at the last compaction (no LLM), rendered
    /// under each open task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<GitContext>,
}

/// Cheap, bounded git facts about a worktree: branch, changed files (vs the
/// merge-base with the default branch, plus uncommitted/untracked) and a
/// derived next step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitContext {
    pub branch: String,
    pub worktree: String,
    #[serde(default)]
    pub changed_files: Vec<String>,
    /// Total changed files (the list is capped at CHANGED_FILES_MAX).
    #[serde(default)]
    pub changed_total: usize,
    #[serde(default)]
    pub next_step: String,
}

const CHANGED_FILES_MAX: usize = 8;

fn git_output(worktree: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned()
    })
}

/// Derive the git context of `worktree`; `None` when it is not a git work
/// tree (or git is unavailable).
pub(crate) fn git_context(worktree: &Path) -> Option<GitContext> {
    if git_output(worktree, &["rev-parse", "--is-inside-work-tree"])?.trim() != "true" {
        return None;
    }
    let branch = git_output(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "(no commits)".to_owned());
    let status = git_output(
        worktree,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )
    .unwrap_or_default();
    let mut files: Vec<String> = Vec::new();
    let push = |files: &mut Vec<String>, path: &str| {
        let path = path.trim().trim_matches('"');
        if !path.is_empty() && !files.iter().any(|f| f == path) {
            files.push(path.to_owned());
        }
    };
    let dirty = status.lines().filter(|line| line.len() > 3).count();
    for line in status.lines().filter(|line| line.len() > 3) {
        let path = &line[3..];
        push(&mut files, path.rsplit(" -> ").next().unwrap_or(path));
    }
    // Committed changes on this branch vs the default branch.
    let base = git_output(
        worktree,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .or_else(|| {
        ["main", "master"].into_iter().find_map(|name| {
            git_output(worktree, &["rev-parse", "--verify", "--quiet", name])
                .map(|_| name.to_owned())
        })
    });
    let merge_base = base
        .as_deref()
        .and_then(|base| git_output(worktree, &["merge-base", "HEAD", base]));
    let mut ahead = 0;
    if let Some(merge_base) = merge_base.as_deref() {
        for path in git_output(worktree, &["diff", "--name-only", merge_base, "HEAD"])
            .unwrap_or_default()
            .lines()
        {
            push(&mut files, path);
        }
        ahead = git_output(
            worktree,
            &["rev-list", "--count", &format!("{merge_base}..HEAD")],
        )
        .and_then(|count| count.trim().parse::<usize>().ok())
        .unwrap_or(0);
    }
    let next_step = if dirty > 0 {
        format!("verify and commit {dirty} uncommitted change(s) on {branch}")
    } else if ahead > 0 {
        format!("push {branch} and open/merge its PR ({ahead} commit(s) ahead)")
    } else {
        String::new()
    };
    let changed_total = files.len();
    files.truncate(CHANGED_FILES_MAX);
    Some(GitContext {
        branch,
        worktree: worktree.to_string_lossy().into_owned(),
        changed_files: files,
        changed_total,
        next_step,
    })
}

/// One-line rendering of a git context (shown under each open task).
pub(crate) fn render_git_context(context: &GitContext) -> String {
    let mut line = format!("branch `{}` @ `{}`", context.branch, context.worktree);
    if context.changed_total > 0 {
        line.push_str(&format!(
            " · files({}): {}",
            context.changed_total,
            context.changed_files.join(", ")
        ));
        if context.changed_total > context.changed_files.len() {
            line.push_str(", …");
        }
    }
    if !context.next_step.is_empty() {
        line.push_str(&format!(" · next: {}", context.next_step));
    }
    single_line(&line)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RelayItem {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RelayTask {
    pub id: String,
    pub text: String,
    #[serde(default = "task_status_todo")]
    pub status: String,
    /// Last time an op or a file edit touched this task; drives staleness.
    /// `None` on legacy rows, which count from their first load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_ms: Option<i64>,
}

fn task_status_todo() -> String {
    "todo".to_owned()
}

const TASK_STATUSES: [&str; 4] = ["todo", "doing", "done", "blocked"];

/// Incremental ops the summarizer returns. Only these apply; the previous
/// memory is the merge base, so an LLM rewrite cannot wipe sections.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct HandoffOps {
    goals: Vec<ItemOp>,
    preferences: Vec<ItemOp>,
    mistakes: Vec<ItemOp>,
    tasks: Vec<TaskOp>,
    current_state: Option<String>,
    recent: Vec<String>,
}

impl HandoffOps {
    /// Valid-but-unrecognized JSON (`{}`, renamed keys) deserializes to an
    /// empty op set; treat it as a failure so the rule fallback records the
    /// events instead of silently advancing the cursor past them.
    fn is_empty(&self) -> bool {
        self.goals.is_empty()
            && self.preferences.is_empty()
            && self.mistakes.is_empty()
            && self.tasks.is_empty()
            && self.recent.is_empty()
            && self
                .current_state
                .as_deref()
                .is_none_or(|state| state.trim().is_empty())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ItemOp {
    op: String,
    id: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TaskOp {
    op: String,
    id: Option<String>,
    text: Option<String>,
    status: Option<String>,
}

/// The structured memory from a stored handoff, or `None` for a legacy
/// free-form document (no decodable relay-memory block).
pub(crate) fn parsed_memory(handoff_md: &str) -> Option<RelayMemory> {
    let body = handoff_md.trim_end().strip_suffix(MEMORY_CLOSE)?;
    let start = body.rfind(MEMORY_OPEN)?;
    serde_json::from_str::<RelayMemory>(&body[start + MEMORY_OPEN.len()..]).ok()
}

/// Split a stored handoff into its structured memory and legacy context.
/// When the trailing relay-memory block decodes, the rendered markdown above
/// it is derived state, not context, so legacy is empty. Without a block the
/// whole document is legacy free-form context awaiting migration.
fn split_memory(handoff_md: &str) -> (RelayMemory, String) {
    match parsed_memory(handoff_md) {
        Some(memory) => (memory, String::new()),
        None => (RelayMemory::default(), handoff_md.trim().to_owned()),
    }
}

/// Fold agent edits of HANDOFF.md back into the memory: the rules and task
/// list in the file are authoritative (rendered newest-first, so reversed
/// back). Lines without an `[id]` become new items and duplicate lines
/// collapse to the oldest id. Returns true if folded.
pub(crate) fn fold_file_edits(memory: &mut RelayMemory, file_md: &str, now_ms: i64) -> bool {
    #[derive(PartialEq)]
    enum Section {
        None,
        Rules,
        Mistakes,
        Tasks,
        Other,
    }
    let mut section = Section::None;
    let (mut rules, mut mistakes, mut tasks) = (Vec::new(), Vec::new(), Vec::new());
    let mut saw_any = false;
    for line in file_md.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("<!--") {
            break;
        }
        if let Some(heading) = trimmed.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            section = if heading == RULES_HEADING {
                saw_any = true;
                Section::Rules
            } else if heading == MISTAKES_HEADING {
                saw_any = true;
                Section::Mistakes
            } else if heading == TASKS_HEADING {
                saw_any = true;
                Section::Tasks
            } else if heading == "规则" {
                Section::None
            } else {
                Section::Other
            };
            continue;
        }
        let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        else {
            continue;
        };
        let (id, rest) = match item.strip_prefix('[').and_then(|r| r.split_once(']')) {
            Some((id, rest)) => (Some(id.trim().to_owned()), rest.trim()),
            None => (None, item.trim()),
        };
        match section {
            Section::Rules | Section::Mistakes if !rest.is_empty() => {
                let target = if section == Section::Rules {
                    &mut rules
                } else {
                    &mut mistakes
                };
                target.push((id, rest.to_owned()));
            }
            Section::Tasks if !rest.is_empty() => {
                let (status, text) = match rest.strip_prefix('(').and_then(|r| r.split_once(')')) {
                    Some((status, text)) if TASK_STATUSES.contains(&status.trim()) => {
                        (status.trim().to_owned(), text.trim().to_owned())
                    }
                    _ => ("todo".to_owned(), rest.to_owned()),
                };
                if !text.is_empty() {
                    tasks.push((id, text, status));
                }
            }
            _ => {}
        }
    }
    if !saw_any {
        return false;
    }
    // Duplicate lines are resolved by the dedupe pass below: the rendered
    // file is newest-first, so reversing gives the oldest line (and id) first
    // and the fuller text wins over an older clipped copy.
    let to_items = |parsed: Vec<(Option<String>, String)>, prefix: &str| {
        let mut items: Vec<RelayItem> = Vec::new();
        for (id, text) in parsed.into_iter().rev() {
            let id = id
                .filter(|id| !id.is_empty() && !items.iter().any(|item| &item.id == id))
                .unwrap_or_else(|| next_id(&items, prefix));
            items.push(RelayItem { id, text });
        }
        dedupe_items(&mut items);
        items
    };
    memory.preferences = to_items(rules, "p");
    memory.mistakes = to_items(mistakes, "m");
    let previous = std::mem::take(&mut memory.tasks);
    let mut folded: Vec<RelayTask> = Vec::new();
    for (id, text, status) in tasks.into_iter().rev() {
        let id = id
            .filter(|id| !id.is_empty() && !folded.iter().any(|task| &task.id == id))
            .unwrap_or_else(|| {
                let ids: Vec<RelayItem> = folded
                    .iter()
                    .map(|task| RelayItem {
                        id: task.id.clone(),
                        text: String::new(),
                    })
                    .collect();
                next_id(&ids, "t")
            });
        // An unchanged line keeps its age; an edited one counts as touched.
        let updated_ms = previous
            .iter()
            .find(|task| task.id == id && task.text == text && task.status == status)
            .and_then(|task| task.updated_ms)
            .or(Some(now_ms));
        folded.push(RelayTask {
            id,
            text,
            status,
            updated_ms,
        });
    }
    dedupe_tasks(&mut folded);
    memory.tasks = folded;
    true
}

/// Record the native session log files behind these events (newest last,
/// deduplicated by path, bounded).
fn record_sources(memory: &mut RelayMemory, events: &[WorkEvent]) {
    for line in &mut memory.sources {
        *line = normalize_legacy_times(line);
    }
    for event in events {
        let Some(path) = event.source_ref.strip_prefix("native-log:") else {
            continue;
        };
        let when = relay_time(event.ts);
        memory
            .sources
            .retain(|line| !line.ends_with(&format!(" {path}")));
        memory
            .sources
            .push(format!("{} {when} {path}", single_line(&event.provider)));
    }
    let excess = memory.sources.len().saturating_sub(SOURCES_MAX);
    memory.sources.drain(..excess);
}

/// Fixed relay display offset: Asia/Shanghai (UTC+8, no DST).
const RELAY_TZ_OFFSET_SECS: i64 = 8 * 3600;
const RELAY_TZ_LABEL: &str = "+08:00";

/// The single relay time format: `YYYY-MM-DD HH:MM +08:00` (Asia/Shanghai)
/// from epoch milliseconds. Every relay-rendered timestamp goes through here.
pub(crate) fn relay_time(ts_ms: i64) -> String {
    let secs = ts_ms.div_euclid(1000) + RELAY_TZ_OFFSET_SECS;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} {RELAY_TZ_LABEL}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// Civil-from-days (Howard Hinnant).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Rewrite legacy `YYYY-MM-DD HH:MM UTC` stamps (stored before the relay
/// switched to +08:00) into the relay format, so output stays consistent.
fn normalize_legacy_times(text: &str) -> String {
    static LEGACY: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let legacy = LEGACY.get_or_init(|| {
        regex::Regex::new(r"\b(\d{4})-(\d{2})-(\d{2}) (\d{2}):(\d{2}) UTC\b").expect("regex")
    });
    legacy
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let n = |i: usize| caps[i].parse::<i64>().unwrap_or(0);
            let days = days_from_civil(n(1), n(2), n(3));
            relay_time((days * 86_400 + n(4) * 3600 + n(5) * 60) * 1000)
        })
        .into_owned()
}

/// Whitespace-collapse a value onto one line for list rendering.
fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Comparison key for a rule/mistake/task text: trim, collapse whitespace,
/// drop a trailing "…"/"...", drop trailing 。.；; punctuation and case-fold
/// ASCII. The summarizer re-adds what it cannot see — often a clipped copy
/// ending in "…" — so writes compare on this key instead of exact text.
fn normalize_text(text: &str) -> String {
    let mut key = single_line(text).to_ascii_lowercase();
    loop {
        let trimmed = key
            .trim_end()
            .trim_end_matches(['。', '.', '；', ';', ','])
            .trim_end();
        let stripped = trimmed.strip_suffix('…').unwrap_or(trimmed).trim_end();
        if stripped.len() == key.len() {
            return stripped.to_owned();
        }
        key.truncate(stripped.len());
    }
}

/// Same rule/task? Equal after normalization, or the shorter one is a
/// clipped copy: a prefix match merges only when the SHORTER ORIGINAL text
/// ends with an explicit truncation marker, so `test` never swallows
/// `test deployment` while a clipped duplicate still folds into its source.
fn same_text(a: &str, b: &str) -> bool {
    let (key_a, key_b) = (normalize_text(a), normalize_text(b));
    if key_a.is_empty() || key_b.is_empty() {
        return false;
    }
    if key_a == key_b {
        return true;
    }
    let (shorter, shorter_key, longer_key) = if key_a.len() <= key_b.len() {
        (a, key_a.as_str(), key_b.as_str())
    } else {
        (b, key_b.as_str(), key_a.as_str())
    };
    is_clipped(shorter) && longer_key.starts_with(shorter_key)
}

/// A text ending in `…` or `...` was clipped by some producer, so its
/// normalized key is only a prefix of the full text.
fn is_clipped(text: &str) -> bool {
    let trimmed = text.trim_end();
    trimmed.ends_with('…') || trimmed.ends_with("...")
}

/// Should `candidate` replace `current` when both are copies of the same
/// text? A text without a truncation marker always beats a clipped copy of
/// itself (the marker is lost content, not extra content), and among equally
/// clipped/full text the longer one wins — judged on the normalized key, so a
/// duplicate that only differs in spacing or punctuation cannot win.
fn prefers_replacement(current: &str, candidate: &str) -> bool {
    if is_clipped(current) != is_clipped(candidate) {
        return is_clipped(current);
    }
    normalize_text(candidate).len() > normalize_text(current).len()
}

/// Final dedupe pass over one section: ops (and a file fold) can leave the
/// same text under two ids. Keep the earliest id and the fuller (never a
/// clipped) text, so one rule is never listed twice.
fn dedupe_items(items: &mut Vec<RelayItem>) {
    let mut deduped: Vec<RelayItem> = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        match deduped
            .iter_mut()
            .find(|kept| same_text(&kept.text, &item.text))
        {
            Some(kept) => {
                if prefers_replacement(&kept.text, &item.text) {
                    kept.text = item.text;
                }
            }
            None => deduped.push(item),
        }
    }
    *items = deduped;
}

/// Same as [`dedupe_items`] for tasks: the merged task keeps the earliest id,
/// the fuller text, the freshest touch stamp and the status that stamp
/// belongs to (a duplicate marked done must stay done and get closed, so a
/// tie goes to the duplicate seen later).
fn dedupe_tasks(tasks: &mut Vec<RelayTask>) {
    let mut deduped: Vec<RelayTask> = Vec::with_capacity(tasks.len());
    for task in tasks.drain(..) {
        match deduped
            .iter_mut()
            .find(|kept| same_text(&kept.text, &task.text))
        {
            Some(kept) => {
                if prefers_replacement(&kept.text, &task.text) {
                    kept.text = task.text;
                }
                if task.updated_ms.unwrap_or(i64::MIN) >= kept.updated_ms.unwrap_or(i64::MIN) {
                    kept.status = task.status;
                }
                kept.updated_ms = kept.updated_ms.max(task.updated_ms);
            }
            None => deduped.push(task),
        }
    }
    *tasks = deduped;
}

/// Render the fixed-section handoff document plus the trailing structured
/// block. Section order is contractual (entry-agnostic readers rely on it).
pub(crate) fn render_handoff(memory: &RelayMemory) -> String {
    // Only rules, the task list and source session paths; newest first.
    // Agents may edit items in place (`- [id] text`, tasks
    // `- [id] (status) text`); edits are folded back on the next tick.
    let mut out =
        format!("# HANDOFF\n\n## 规则\n\n> {RULES_SCOPE_NOTE}\n>\n> {RULES_EXECUTABLE_NOTE}\n");
    render_items(&mut out, RULES_HEADING, &memory.preferences);
    render_items(&mut out, MISTAKES_HEADING, &memory.mistakes);
    out.push_str(&format!("\n\n## {TASKS_HEADING}\n"));
    // Done tasks are closed work: dropped from the memory by `refresh_tasks`
    // and never rendered, so the list only shows what is still open.
    for task in memory
        .tasks
        .iter()
        .rev()
        .filter(|task| task.status != "done")
    {
        out.push_str(&format!(
            "\n- [{}] ({}) {}",
            task.id,
            task.status,
            single_line(&task.text)
        ));
        // Indented, non-list line: ignored when agent edits are folded back.
        if let Some(context) = &memory.workspace {
            out.push_str(&format!("\n  ↳ {}", render_git_context(context)));
        }
    }
    out.push_str("\n\n## 历史会话路径\n");
    for line in memory.sources.iter().rev() {
        out.push_str(&format!(
            "\n- {}",
            normalize_legacy_times(&single_line(line))
        ));
    }
    // `<!--` inside item text must never terminate the block early or shadow
    // the outer open marker; \u0021 decodes back to `!` in valid JSON.
    let json = serde_json::to_string(memory)
        .unwrap_or_else(|_| "{}".to_owned())
        .replace("<!--", "<\\u0021--");
    out.push_str(&format!("\n\n{MEMORY_OPEN}{json}{MEMORY_CLOSE}\n"));
    out
}

/// Rules recorded here are project-scoped; agents must keep them out of
/// global instruction files.
pub(crate) const RULES_SCOPE_NOTE: &str = "以下规则仅适用于本项目（this project only）。不要修改全局 agent 指令文件（~/.agents/AGENTS.md、~/.claude/CLAUDE.md、~/.codex/AGENTS.md 等）；偏好请写在本文件。";
/// Prefer executable rules: enforceable mistakes become tests/lints/scripts.
pub(crate) const RULES_EXECUTABLE_NOTE: &str = "错题集中能用测试、lint 或脚本强制执行的条目，应改写成对应的测试/lint/脚本，本文件只保留原因（mistakes that a test, lint or script can enforce should become one; keep only the reason here).";
const RULES_HEADING: &str = "公共原则/偏好规范";
const MISTAKES_HEADING: &str = "错题集";
const TASKS_HEADING: &str = "任务清单";
const SOURCES_MAX: usize = 30;

fn render_items(out: &mut String, heading: &str, items: &[RelayItem]) {
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\n### {heading}\n"));
    for item in items.iter().rev() {
        out.push_str(&format!("\n- [{}] {}", item.id, single_line(&item.text)));
    }
}

fn next_id(items: &[RelayItem], prefix: &str) -> String {
    let max = items
        .iter()
        .filter_map(|item| {
            item.id
                .strip_prefix(prefix)
                .and_then(|rest| rest.parse::<u64>().ok())
        })
        .max()
        .unwrap_or(0);
    format!("{prefix}{}", max.saturating_add(1))
}

fn apply_item_ops(items: &mut Vec<RelayItem>, ops: Vec<ItemOp>, prefix: &str) {
    for op in ops {
        match op.op.as_str() {
            "remove" => {
                if let Some(id) = op.id.as_deref().map(str::trim) {
                    items.retain(|item| item.id != id);
                }
            }
            "add" | "update" => {
                let Some(text) = op
                    .text
                    .map(|text| text.trim().to_owned())
                    .filter(|text| !text.is_empty())
                else {
                    continue;
                };
                let id = op
                    .id
                    .map(|id| id.trim().to_owned())
                    .filter(|id| !id.is_empty());
                if let Some(existing) = id
                    .as_deref()
                    .and_then(|id| items.iter_mut().find(|item| item.id == id))
                {
                    existing.text = text;
                    continue;
                }
                // Dedupe on write: an add whose normalized text equals — or is
                // a truncated/expanded form of — an existing item updates that
                // item (keeping the longer text) instead of pushing a new id.
                // The summarizer cannot see items it was not shown, so it
                // re-adds them under fresh ids; without this the file doubles.
                if let Some(existing) = items.iter_mut().find(|item| same_text(&item.text, &text)) {
                    if prefers_replacement(&existing.text, &text) {
                        existing.text = text;
                    }
                    continue;
                }
                let id = id.unwrap_or_else(|| next_id(items, prefix));
                items.push(RelayItem { id, text });
            }
            _ => {}
        }
    }
}

fn apply_task_ops(tasks: &mut Vec<RelayTask>, ops: Vec<TaskOp>, now_ms: i64) {
    for op in ops {
        let id = op
            .id
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty());
        match op.op.as_str() {
            "remove" => {
                if let Some(id) = id {
                    tasks.retain(|task| task.id != id);
                }
            }
            "add" | "update" => {
                let text = op
                    .text
                    .map(|text| text.trim().to_owned())
                    .filter(|text| !text.is_empty());
                let status = op
                    .status
                    .map(|status| status.trim().to_owned())
                    .filter(|status| TASK_STATUSES.contains(&status.as_str()));
                // Same-id update, then a text duplicate (a re-added or clipped
                // copy of a task already listed), then a genuinely new task.
                let explicit = id
                    .as_deref()
                    .and_then(|id| tasks.iter().position(|task| task.id == id));
                let target = explicit.or_else(|| {
                    text.as_deref()
                        .and_then(|text| tasks.iter().position(|task| same_text(&task.text, text)))
                });
                if let Some(index) = target {
                    let existing = &mut tasks[index];
                    // A duplicate only wins when it is the fuller text (a
                    // non-clipped, longer copy); an explicit update is taken
                    // as given.
                    if let Some(text) = text
                        && (explicit.is_some() || prefers_replacement(&existing.text, &text))
                    {
                        existing.text = text;
                    }
                    if let Some(status) = status {
                        existing.status = status;
                    }
                    existing.updated_ms = Some(now_ms);
                    continue;
                }
                let Some(text) = text else { continue };
                let id = id.unwrap_or_else(|| {
                    next_id(
                        &tasks
                            .iter()
                            .map(|task| RelayItem {
                                id: task.id.clone(),
                                text: String::new(),
                            })
                            .collect::<Vec<_>>(),
                        "t",
                    )
                });
                tasks.push(RelayTask {
                    id,
                    text,
                    status: status.unwrap_or_else(task_status_todo),
                    updated_ms: Some(now_ms),
                });
            }
            _ => {}
        }
    }
}

/// Merge a valid semantic response into the memory. Sections the response
/// does not mention keep their previous items by construction.
fn apply_memory_ops(memory: &mut RelayMemory, ops: HandoffOps, now_ms: i64) {
    apply_item_ops(&mut memory.goals, ops.goals, "g");
    apply_item_ops(&mut memory.preferences, ops.preferences, "p");
    apply_item_ops(&mut memory.mistakes, ops.mistakes, "m");
    apply_task_ops(&mut memory.tasks, ops.tasks, now_ms);
    if let Some(state) = ops
        .current_state
        .map(|state| state.trim().to_owned())
        .filter(|state| !state.is_empty())
    {
        memory.current_state = clip_chars(&state, CURRENT_STATE_MAX_CHARS);
    }
    for line in ops.recent {
        let line = clip_chars(&single_line(&line), EVENT_RECENT_MAX_CHARS);
        if !line.is_empty() {
            memory.recent.push(line);
        }
    }
    memory.recent = memory
        .recent
        .split_off(memory.recent.len().saturating_sub(RECENT_MAX_LINES));
    // One final dedupe pass: an explicit-id update can re-state another item
    // (`update p2 -> "alpha."` when p1 already says `Alpha`), which must not
    // leave the section with two copies of the same text.
    dedupe_items(&mut memory.goals);
    dedupe_items(&mut memory.preferences);
    dedupe_items(&mut memory.mistakes);
    dedupe_tasks(&mut memory.tasks);
}

/// One bounded, provider-prefixed rolling-log line for the fallback path.
fn recent_line(event: &WorkEvent) -> String {
    clip_chars(
        &format!(
            "{}: [{}] {}",
            single_line(&event.provider),
            event.kind,
            single_line(&event.summary)
        ),
        EVENT_RECENT_MAX_CHARS,
    )
}

/// Rule fallback (LLM failure/invalid JSON): long-lived sections and tasks
/// stay byte-identical; only the rolling log and 当前状态 advance. Legacy
/// free-form handoffs migrate into 当前状态 here.
fn apply_fallback(memory: &mut RelayMemory, legacy: &str, events: &[WorkEvent]) {
    for event in events {
        memory.recent.push(recent_line(event));
    }
    memory.recent = memory
        .recent
        .split_off(memory.recent.len().saturating_sub(RECENT_MAX_LINES));
    if !legacy.trim().is_empty() {
        memory.current_state = clip_chars(legacy.trim(), CURRENT_STATE_MAX_CHARS);
    } else if let Some(last) = events.last() {
        let line = recent_line(last);
        let previous = memory.current_state.trim();
        memory.current_state = clip_chars(
            &if previous.is_empty() {
                line
            } else {
                format!("{line}\n{previous}")
            },
            CURRENT_STATE_MAX_CHARS,
        );
    }
}

/// Enforce the rendered budget without ever cutting item text: a clipped rule
/// is a lost rule. Only derived, droppable things go — the rolling log, extra
/// source paths, closed tasks, the git context. If the document still does not
/// fit, it is written over budget with complete items.
fn enforce_budget(memory: &mut RelayMemory) {
    for _ in 0..200 {
        if render_handoff(memory).chars().count() <= HANDOFF_MAX_CHARS {
            return;
        }
        if !memory.recent.is_empty() {
            memory.recent.remove(0);
            continue;
        }
        if memory.sources.len() > 5 {
            memory.sources.remove(0);
            continue;
        }
        if memory.tasks.iter().any(|task| task.status == "done") {
            memory.tasks.retain(|task| task.status != "done");
            continue;
        }
        // Derived git context is re-derivable; drop it before anything else.
        if memory.workspace.take().is_some() {
            continue;
        }
        return;
    }
}

/// PR numbers and remote branch names already merged into `origin/main`, used
/// to close tasks that finished work still lists as open.
#[derive(Debug, Default)]
struct MergedRefs {
    prs: Vec<u64>,
    branches: Vec<String>,
}

impl MergedRefs {
    fn matched_by(&self, text: &str) -> bool {
        // A number only closes a task when the text uses PR semantics, so a
        // bare `#N` or `issue #N` reference never closes it. No word-boundary
        // anchor before `PR`: CJK text can sit directly against the marker
        // (`完成PR#456合并`), while a following letter (`PRs`, `PR-456`) still
        // fails because only whitespace or `#` may separate it from the number.
        static PR: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let pr = PR.get_or_init(|| {
            regex::Regex::new(r"(?i)PR(?:\s*#\s*|\s+|#)\s*(\d+)|/pull/(\d+)").expect("pr regex")
        });
        pr.captures_iter(text).any(|caps| {
            caps.iter()
                .skip(1)
                .flatten()
                .find_map(|number| number.as_str().parse::<u64>().ok())
                .is_some_and(|number| self.prs.contains(&number))
        }) || self
            .branches
            .iter()
            .any(|branch| names_branch(text, branch))
    }
}

/// Token-bounded branch match: the task text must contain the branch name as
/// one token, split on whitespace and punctuation except `/`, `-`, `.`, `_`
/// (branch-name characters), so `fix/foo` never matches `fix/foobar`.
fn names_branch(text: &str, branch: &str) -> bool {
    !branch.is_empty()
        && text
            .split(is_branch_token_separator)
            .any(|token| token == branch)
}

fn is_branch_token_separator(ch: char) -> bool {
    !(ch.is_alphanumeric() || matches!(ch, '/' | '-' | '.' | '_'))
}

/// Read merged PR numbers / branch names from the worktree's `origin/main`.
/// Returns empty refs when git or the remote ref is unavailable, so a
/// non-git or offline worktree closes nothing.
fn merged_refs(worktree: &Path) -> MergedRefs {
    let mut refs = MergedRefs::default();
    let log = git_output(
        worktree,
        &["log", "origin/main", "--oneline", "--max-count=2000"],
    )
    .unwrap_or_default();
    if let Some(branches) = git_output(
        worktree,
        &[
            "branch",
            "-r",
            "--merged",
            "origin/main",
            "--format=%(refname:short)",
        ],
    ) {
        for name in branches.lines().map(str::trim) {
            let name = name.strip_prefix("origin/").unwrap_or(name);
            if !name.is_empty() && name != "main" && name != "HEAD" && !name.contains("->") {
                refs.branches.push(name.to_owned());
            }
        }
    }
    static PR_TRAILING: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let trailing = PR_TRAILING
        .get_or_init(|| regex::Regex::new(r"\(#(\d+)\)\s*$").expect("trailing pr regex"));
    static PR_MERGE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let merge = PR_MERGE.get_or_init(|| {
        regex::Regex::new(r"^Merge pull request #(\d+)\b").expect("merge pr regex")
    });
    for line in log.lines() {
        // `--oneline` prints "<sha> <subject>"; only merge evidence in the
        // subject counts, so a body/commit message mentioning "#123" ("fix:
        // workaround for #123 (#456)") never closes the wrong task.
        let subject = line.split_once(' ').map_or(line, |(_, subject)| subject);
        let capture = merge
            .captures(subject)
            .or_else(|| trailing.captures(subject));
        if let Some(caps) = capture
            && let Ok(number) = caps[1].parse::<u64>()
            && !refs.prs.contains(&number)
        {
            refs.prs.push(number);
        }
    }
    refs
}

/// Close finished tasks: `done` tasks leave the memory entirely, and an open
/// task is closed when its PR/branch is already merged into `origin/main` or
/// when nothing touched it for STALE_TASK_DAYS. `merged` is `None` when the
/// caller already knows there is no open task, so git is not consulted at all.
/// Legacy tasks without an update stamp start their clock now instead of being
/// dropped on first load.
fn refresh_tasks(memory: &mut RelayMemory, merged: Option<&MergedRefs>, now_ms: i64) {
    for task in &mut memory.tasks {
        task.updated_ms.get_or_insert(now_ms);
    }
    memory.tasks.retain(|task| {
        task.status != "done"
            && now_ms.saturating_sub(task.updated_ms.unwrap_or(now_ms)) <= STALE_TASK_DAYS * DAY_MS
            && !merged.is_some_and(|merged| merged.matched_by(&task.text))
    });
}

/// Merged PR/branch evidence per worktree: computed at most once per tick, and
/// only when that worktree still holds an open task.
#[derive(Default)]
struct MergedRefCache {
    entries: std::collections::HashMap<String, MergedRefs>,
}

impl MergedRefCache {
    fn for_memory(&mut self, memory: &RelayMemory, worktree: &Path) -> Option<&MergedRefs> {
        // Nothing to close: skip git entirely.
        if !memory.tasks.iter().any(|task| task.status != "done") {
            return None;
        }
        let key = worktree.to_string_lossy().into_owned();
        let root = worktree.to_path_buf();
        Some(
            self.entries
                .entry(key)
                .or_insert_with(|| merged_refs(&root)),
        )
    }
}

pub(crate) fn compact_tick(
    store: &StateStore,
    summarizer: &dyn WorkSummarizer,
    ingest: &dyn Fn(&StateStore) -> Result<usize, String>,
    now_ms: i64,
) -> Result<usize, String> {
    let mut first_error = ingest(store).err();
    let scopes = store.work_scopes_with_new_events()?;
    let mut compacted = 0;
    // Merged-ref evidence is per worktree per tick, shared by compaction and
    // the task auto-close pass below.
    let mut merged = MergedRefCache::default();

    for (project_ref, worktree) in scopes {
        if let Err(error) = compact_scope(
            store,
            summarizer,
            &project_ref,
            &worktree,
            now_ms,
            &mut merged,
        ) {
            first_error.get_or_insert(error);
        } else {
            compacted += 1;
        }
    }

    // The file export can fail after the cursor advanced (or the worktree was
    // reset afterwards); re-derive HANDOFF.md whenever it drifts from state.
    // This pass also closes finished tasks on every tick, including ticks with
    // no new events at all.
    if let Err(error) = reconcile_handoff_files(store, &mut merged, now_ms) {
        first_error.get_or_insert(error);
    }

    if let Err(error) = store.delete_expired_work_events(now_ms) {
        first_error.get_or_insert(error);
    }
    first_error.map_or(Ok(compacted), Err)
}

fn compact_scope(
    store: &StateStore,
    summarizer: &dyn WorkSummarizer,
    project_ref: &str,
    worktree: &str,
    now_ms: i64,
    merged: &mut MergedRefCache,
) -> Result<(), String> {
    // Process new events oldest-first in bounded batches. The cursor only
    // advances to the last event actually folded into the handoff, so a
    // backlog larger than one tick simply continues on the next tick (or the
    // next batch) instead of being silently skipped.
    let mut previous = store
        .work_state(project_ref, worktree)?
        .unwrap_or_else(|| WorkState {
            project_ref: project_ref.to_owned(),
            worktree: worktree.to_owned(),
            log_cursors_json: "{}".to_owned(),
            ..WorkState::default()
        });
    let worktree_path = Path::new(worktree);

    for _ in 0..EVENT_BATCHES_PER_TICK {
        let events = store.work_events_after(
            project_ref,
            worktree,
            previous.through_event_id,
            EVENT_BATCH_SIZE,
        )?;
        let Some(through_event_id) = events.last().map(|event| event.event_id) else {
            return Ok(());
        };
        let done = events.len() < EVENT_BATCH_SIZE;

        // Merge base: structured memory from the relay-memory block, with a
        // legacy free-form document carried as context for migration.
        let (base, legacy) = split_memory(&previous.handoff_md);
        let workspace = git_context(Path::new(worktree));
        // Rebuild from the merge base + whatever HANDOFF.md currently holds,
        // so a retry after a concurrent agent edit folds that edit in too.
        let mut build = |file_md: Option<&str>, ops: Option<&HandoffOps>| {
            let mut memory = base.clone();
            if let Some(file_md) = file_md
                && file_md != previous.handoff_md
            {
                fold_file_edits(&mut memory, file_md, now_ms);
            }
            record_sources(&mut memory, &events);
            memory.workspace = workspace.clone();
            if let Some(ops) = ops {
                apply_memory_ops(&mut memory, ops.clone(), now_ms);
                // Never lose a pre-upgrade free-form handoff: keep it in
                // 当前状态 unless the model already restated state.
                if !legacy.trim().is_empty() && memory.current_state.trim().is_empty() {
                    memory.current_state = clip_chars(legacy.trim(), CURRENT_STATE_MAX_CHARS);
                }
            }
            // Merged-ref evidence comes from the memory actually being
            // refreshed (after the file fold and the semantic ops), so a
            // finished task the agent just added is closed in this tick; the
            // git read itself stays cached per worktree per tick, and is
            // skipped entirely while no task is open.
            let refs = merged.for_memory(&memory, worktree_path);
            refresh_tasks(&mut memory, refs, now_ms);
            memory
        };
        let target = Path::new(worktree).join("HANDOFF.md");
        let prompt_memory = build(read_handoff_file(&target).as_deref(), None);
        let prompt = compaction_prompt(&prompt_memory, &legacy, &events);
        let semantic_ops = summarizer
            .summarize(&prompt)
            .ok()
            .and_then(|content| serde_json::from_str::<HandoffOps>(&content).ok())
            .filter(|ops| !ops.is_empty());
        let handoff_md = write_handoff_guarded(Path::new(worktree), |file_md| {
            let mut memory = build(file_md, semantic_ops.as_ref());
            if semantic_ops.is_none() {
                apply_fallback(&mut memory, &legacy, &events);
            }
            enforce_budget(&mut memory);
            render_handoff(&memory)
        });
        let (handoff_md, write_error) = match handoff_md {
            GuardedWrite::Written(handoff_md) => (handoff_md, None),
            GuardedWrite::Failed(handoff_md, error) => (handoff_md, Some(error)),
            // The file kept changing under us: keep the cursor so the next
            // tick retries instead of overwriting an agent edit.
            GuardedWrite::Conflict => return Err(HANDOFF_CONFLICT_ERROR.to_owned()),
        };
        store.put_work_state(&WorkState {
            project_ref: project_ref.to_owned(),
            worktree: worktree.to_owned(),
            handoff_md: handoff_md.clone(),
            through_event_id,
            log_cursors_json: previous.log_cursors_json.clone(),
            updated_at: now_ms,
        })?;
        if let Some(error) = write_error {
            return Err(error);
        }

        previous = WorkState {
            handoff_md,
            through_event_id,
            ..previous
        };
        if done {
            return Ok(());
        }
    }
    Ok(())
}

const COMPACTION_PROMPT_HEAD: &str = "You maintain one project relay memory shared across coding agents. Given the CURRENT structured memory (JSON), optional LEGACY context from an older free-form handoff, and NEW work events, return ONLY JSON of this exact shape:
{\"goals\":[{\"op\":\"add|update|remove\",\"id\":\"g1\",\"text\":\"...\"}],\"preferences\":[{\"op\":\"add|update|remove\",\"id\":\"p1\",\"text\":\"...\"}],\"mistakes\":[{\"op\":\"add|update|remove\",\"id\":\"m1\",\"text\":\"what went wrong + correct approach\"}],\"tasks\":[{\"op\":\"add|update|remove\",\"id\":\"t1\",\"text\":\"...\",\"status\":\"todo|doing|done|blocked\"}],\"current_state\":\"short current state\",\"recent\":[\"provider: one-line summary\"]}
Sections that matter: 公共原则/偏好规范=preferences (durable principles, conventions, user preferences), 错题集=mistakes (what went wrong + the correct approach), 任务清单=tasks (latest remaining tasks with status). Continuously distill rules from the events. A mistake that a test, lint or script can enforce should be turned into one (note that in the task list); keep only the reason in the mistake text. goals/current_state/recent are optional. Rules: apply ONLY incremental ops; prefer update over remove and never remove a goal/preference/mistake unless the events clearly supersede it; reuse existing ids, new ids use the section prefix with an unused number; do not invent facts; omit sections with no change; \"recent\" lists ONLY new lines to append, each prefixed with its provider. In this input, line breaks are written as the two characters \\n; use plain single-line text inside list item fields.

Current memory JSON:
";

/// Event-tail room kept when a huge memory has to give up its oldest whole
/// items, so the summarizer always sees some of the new work.
const PROMPT_EVENTS_RESERVE_BYTES: usize = 2_000;

fn prompt_head(memory: &RelayMemory, legacy: &str) -> String {
    let memory_json = serde_json::to_string(memory).unwrap_or_else(|_| "{}".to_owned());
    format!(
        "{COMPACTION_PROMPT_HEAD}{memory_json}\n\nLegacy context (may be empty):\n{legacy}\n\nNew events:\n"
    )
}

/// Append the event tail, clipping only it to the bytes the head leaves.
fn finish_prompt(head: String, events: &str) -> String {
    let budget = SEMANTIC_PROMPT_MAX_BYTES.saturating_sub(head.len());
    let mut prompt = head;
    prompt.push_str(&clip_utf8_bytes(events, budget));
    prompt
}

fn compaction_prompt(memory: &RelayMemory, legacy: &str, events: &[WorkEvent]) -> String {
    let mut thinned = memory.clone();
    thinned.recent.clear();
    thinned.current_state.clear();
    thinned.workspace = None;
    let legacy = clip_chars(legacy, HANDOFF_MAX_CHARS / 2);
    // The Edge /semantic/chat contract rejects message content containing any
    // control character (U+0000-U+001F, U+007F) with semantic_request_invalid,
    // so encode line breaks/tabs as visible escapes; the cap is measured in
    // BYTES, not chars.
    let events = escape_control_chars(&events.iter().map(format_event).collect::<String>());
    // The memory JSON is NEVER clipped: the summarizer must see the id and the
    // whole text of everything it is shown, otherwise it re-adds items it
    // cannot see and the document doubles. Under byte pressure the
    // re-derivable fields go first (rolling log, current state, git context),
    // then the legacy free-form context, then whole source-path entries, then
    // the OLDEST whole items — all of them stay in memory, and the dedupe on
    // write handles a re-add. Only the event tail is ever cut.
    for (candidate, legacy) in [
        (memory, legacy.as_str()),
        (&thinned, legacy.as_str()),
        (&thinned, ""),
    ] {
        let head = escape_control_chars(&prompt_head(candidate, legacy));
        if head.len() + events.len() <= SEMANTIC_PROMPT_MAX_BYTES {
            return finish_prompt(head, &events);
        }
    }
    let reserve = events.len().min(PROMPT_EVENTS_RESERVE_BYTES);
    let mut kept = thinned.clone();
    loop {
        let head = escape_control_chars(&prompt_head(&kept, ""));
        if head.len() + reserve <= SEMANTIC_PROMPT_MAX_BYTES {
            return finish_prompt(head, &events);
        }
        // Source-session paths are re-derivable bookkeeping the summarizer
        // does not need to distill, so whole entries leave first (oldest
        // first — the list is newest-last): the memory JSON is never clipped
        // and the distilled items stay.
        if !kept.sources.is_empty() {
            let drop_count = kept.sources.len().div_ceil(8).max(1);
            kept.sources.drain(..drop_count);
            continue;
        }
        // Drop whole items from the largest section's old end, so every
        // section keeps its newest entries and no text is ever cut.
        let biggest = [
            (0usize, kept.goals.len()),
            (1, kept.preferences.len()),
            (2, kept.mistakes.len()),
            (3, kept.tasks.len()),
        ]
        .into_iter()
        .max_by_key(|(_, len)| *len)
        .filter(|(_, len)| *len > 0);
        let Some((section, len)) = biggest else {
            // The fixed skeleton plus an empty memory always fits; keep the
            // hard byte invariant even if that ever stopped being true.
            return if head.len() > SEMANTIC_PROMPT_MAX_BYTES {
                clip_utf8_bytes(&head, SEMANTIC_PROMPT_MAX_BYTES)
            } else {
                finish_prompt(head, &events)
            };
        };
        let drop_count = len.div_ceil(8).max(1);
        match section {
            0 => {
                kept.goals.drain(..drop_count);
            }
            1 => {
                kept.preferences.drain(..drop_count);
            }
            2 => {
                kept.mistakes.drain(..drop_count);
            }
            _ => {
                kept.tasks.drain(..drop_count);
            }
        }
    }
}

fn escape_control_chars(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 16);
    for ch in text.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => {}
            ch if ch.is_control() && (ch as u32) < 0x80 => out.push(' '),
            ch => out.push(ch),
        }
    }
    out
}

/// Truncate to at most `max_bytes` UTF-8 bytes, ending on a char boundary and
/// marking truncation.
fn clip_utf8_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_owned();
    out.push('…');
    out
}

fn format_event(event: &WorkEvent) -> String {
    format!(
        "- [{}] {} ({}, {})\n",
        event.kind, event.summary, event.provider, event.source_ref
    )
}

/// (Re)write HANDOFF.md for every work_state whose file content differs from
/// (or is missing) the recorded handoff, independent of new events. This is
/// the retry path for a failed export and for worktrees reset after the fact.
/// It is also where finished tasks are closed on every tick, including ticks
/// that carried no new events for the scope.
fn reconcile_handoff_files(
    store: &StateStore,
    merged: &mut MergedRefCache,
    now_ms: i64,
) -> Result<(), String> {
    let mut first_error: Option<String> = None;
    for mut state in store.work_states()? {
        let worktree = Path::new(&state.worktree);
        // The handoff as recorded before this tick: a file that still matches
        // it carries no agent edit.
        let recorded = state.handoff_md.clone();
        let base = parsed_memory(&recorded);
        // The tick render: fold an agent-edited file first (stamping its edits
        // with the current tick, not the state's last write time, so an old
        // file cannot instantaneously age the tasks it adds), then close the
        // finished tasks. Folding before refreshing means an unchanged line
        // keeps its age, so a task closed by this tick is not resurrected by
        // the stale copy still on disk. Task auto-close must not depend on new
        // events arriving at all.
        let mut render = |file_md: Option<&str>| match &base {
            Some(memory) => {
                let mut memory = memory.clone();
                if let Some(file_md) = file_md.filter(|file_md| *file_md != recorded) {
                    fold_file_edits(&mut memory, file_md, now_ms);
                }
                // Merged-ref evidence is read from the memory actually being
                // refreshed, i.e. after the file fold, so a merged-PR task the
                // agent just added is closed in this same tick. The read is
                // still once per worktree per tick and skipped while the
                // memory holds no open task.
                let refs = merged.for_memory(&memory, worktree);
                refresh_tasks(&mut memory, refs, now_ms);
                render_handoff(&memory)
            }
            // A legacy free-form handoff has no memory to refresh yet; it is
            // migrated by compaction when new events arrive.
            None => recorded.clone(),
        };
        if !worktree.is_dir() {
            let refreshed = render(None);
            if refreshed != recorded {
                state.handoff_md = refreshed;
                if let Err(error) = store.put_work_state(&state) {
                    first_error.get_or_insert(error);
                }
            }
            continue;
        }
        let target = worktree.join("HANDOFF.md");
        let current = match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_file() => match fs::read_to_string(&target) {
                Ok(content) => Some(content),
                Err(error) => {
                    first_error.get_or_insert(format!("cannot read HANDOFF.md: {error}"));
                    continue;
                }
            },
            // Never touch a symlinked or otherwise non-regular HANDOFF.md;
            // surface it through write_handoff's refusal instead.
            Ok(_) => None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                first_error.get_or_insert(format!("cannot stat HANDOFF.md: {error}"));
                continue;
            }
        };
        let refreshed = match &current {
            Some(content) => render(Some(content)),
            None => render(None),
        };
        let needs_write = match &current {
            Some(content) => *content != refreshed,
            // Do not materialize empty placeholder files in worktrees that
            // never produced a handoff.
            None => !recorded.trim().is_empty(),
        };
        let mut handoff = refreshed;
        let mut error: Option<String> = None;
        if needs_write {
            (handoff, error) = match write_handoff_guarded(worktree, render) {
                GuardedWrite::Written(handoff) => (handoff, None),
                GuardedWrite::Failed(handoff, error) => (handoff, Some(error)),
                // The file kept changing: keep the recorded memory so the next
                // tick retries instead of racing the agent edit.
                GuardedWrite::Conflict => (recorded, Some(HANDOFF_CONFLICT_ERROR.to_owned())),
            };
        }
        if handoff != state.handoff_md {
            state.handoff_md = handoff;
            if let Err(error) = store.put_work_state(&state) {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = error {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Test helper: write against whatever HANDOFF.md currently holds.
#[cfg(test)]
fn write_handoff_current(worktree: &Path, handoff: &str) -> Result<(), String> {
    let expected = read_handoff_file(&worktree.join("HANDOFF.md"));
    write_handoff(worktree, handoff, expected.as_deref()).map(|_| ())
}

const HANDOFF_WRITE_ATTEMPTS: usize = 3;
const HANDOFF_CONFLICT_ERROR: &str =
    "HANDOFF.md kept changing during compaction; write skipped to preserve agent edits";

fn read_handoff_file(target: &Path) -> Option<String> {
    fs::read_to_string(target).ok()
}

enum GuardedWrite {
    Written(String),
    /// The rendered handoff plus the write error (e.g. a symlinked target).
    Failed(String, String),
    /// HANDOFF.md changed between read and write on every attempt.
    Conflict,
}

/// Lost-update guard: render from the HANDOFF.md content just read, then
/// re-read right before the (atomic temp+rename) write; when an agent edited
/// the file in between, render again from the new content (folding the edit
/// in) instead of overwriting it. Bounded attempts. The renderer may keep
/// per-tick state (the merged-ref cache), hence `FnMut`.
fn write_handoff_guarded(
    worktree: &Path,
    mut render: impl FnMut(Option<&str>) -> String,
) -> GuardedWrite {
    let target = worktree.join("HANDOFF.md");
    for _ in 0..HANDOFF_WRITE_ATTEMPTS {
        let seen = read_handoff_file(&target);
        let handoff = render(seen.as_deref());
        return match write_handoff(worktree, &handoff, seen.as_deref()) {
            Ok(true) => GuardedWrite::Written(handoff),
            Ok(false) => continue,
            Err(error) => GuardedWrite::Failed(handoff, error),
        };
    }
    GuardedWrite::Conflict
}

/// Write HANDOFF.md (plus agent-doc blocks and git excludes). `expected` is
/// the HANDOFF.md content the handoff was rendered from; returns Ok(false)
/// without writing when the file no longer matches it.
fn write_handoff(worktree: &Path, handoff: &str, expected: Option<&str>) -> Result<bool, String> {
    if !worktree.is_dir() {
        return Ok(true);
    }
    // Never drop a file into hidden/config folders (e.g. ~/.ssh) that became
    // an operational root; git checkouts are exempt.
    let hidden = worktree.components().any(|part| {
        part.as_os_str()
            .to_str()
            .is_some_and(|name| name.starts_with('.') && name.len() > 1)
    });
    if hidden && resolve_git_common_dir(worktree)?.is_none() {
        return Ok(true);
    }
    if !is_allowed_project_root(worktree) {
        return Ok(true);
    }
    let target = worktree.join("HANDOFF.md");
    if read_handoff_file(&target).as_deref() != expected {
        return Ok(false);
    }
    if expected != Some(handoff) {
        write_regular_file(&target, handoff, "HANDOFF.md")?;
    }
    let mut created = vec!["HANDOFF.md"];
    // Claude Code does not read AGENTS.md: only touch CLAUDE.md when it
    // already exists or Claude Code worked in this folder.
    let claude_used = handoff.lines().any(|line| line.starts_with("- claude "));
    for name in ["AGENTS.md", "CLAUDE.md"] {
        if name == "CLAUDE.md" && !claude_used && !worktree.join(name).exists() {
            continue;
        }
        if ensure_handoff_block(&worktree.join(name))? {
            created.push(name);
        }
    }
    let Some(git_common_dir) = resolve_git_common_dir(worktree)? else {
        return Ok(true);
    };
    // Only files we created (plus HANDOFF.md) are excluded; a pre-existing,
    // possibly tracked AGENTS.md/CLAUDE.md just carries a local block change.
    // Never commit anything.
    let tracked_or_listed = |name: &str| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["ls-files", "--error-unmatch", name])
            .output()
            .is_ok_and(|output| output.status.success())
    };
    created.retain(|name| *name == "HANDOFF.md" || !tracked_or_listed(name));
    add_git_excludes(&git_common_dir, &created)?;
    Ok(true)
}

/// Scope guard: the relay only ever writes project-local files
/// (HANDOFF.md / AGENTS.md / CLAUDE.md directly inside a project root). A
/// root is refused when it is the filesystem root, `$HOME` itself or one of
/// its ancestors, or anything under a hidden top-level `$HOME` folder (the
/// global agent config homes `~/.agents`, `~/.claude`, `~/.codex`,
/// `~/.config`, ... — even when they are Git checkouts).
pub(crate) fn is_allowed_project_root(path: &Path) -> bool {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    is_allowed_project_root_with_home(path, home.as_deref())
}

fn is_allowed_project_root_with_home(path: &Path, home: Option<&Path>) -> bool {
    let canonical = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let root = canonical(path);
    if root.parent().is_none() {
        return false;
    }
    let Some(home) = home.map(canonical) else {
        return true;
    };
    if home.starts_with(&root) {
        // $HOME itself or an ancestor of it (/home, /Users, ...).
        return false;
    }
    match root.strip_prefix(&home) {
        Ok(relative) => !relative
            .components()
            .next()
            .and_then(|first| first.as_os_str().to_str())
            .is_some_and(|name| name.starts_with('.')),
        Err(_) => true,
    }
}

pub(crate) const HANDOFF_BLOCK_START: &str = "<!-- herdr:handoff:start -->";
pub(crate) const HANDOFF_BLOCK_END: &str = "<!-- herdr:handoff:end -->";
const AGENT_DOC_MAX_BYTES: u64 = 256 * 1024;

fn handoff_block() -> String {
    format!(
        "{HANDOFF_BLOCK_START}\nRead `HANDOFF.md` (rules, task list, past session log paths; auto-maintained by herdr-mcp) before starting, and update it by editing its items in place (`- [id] text`, tasks `- [id] (status) text`) — your edits are merged automatically. Its rules apply to this project only; record preferences there and never edit global agent instruction files (~/.agents/AGENTS.md, ~/.claude/CLAUDE.md, ~/.codex/AGENTS.md).\n{HANDOFF_BLOCK_END}\n"
    )
}

/// Idempotently ensure `path` contains the managed handoff block, touching
/// only the text between the markers. Returns true when the file was newly
/// created. Oversized or non-regular files are left alone.
fn ensure_handoff_block(path: &Path) -> Result<bool, String> {
    let block = handoff_block();
    let existing = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata.len() > AGENT_DOC_MAX_BYTES {
                return Ok(false);
            }
            Some(
                fs::read_to_string(path)
                    .map_err(|error| format!("cannot read agent doc: {error}"))?,
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("cannot stat agent doc: {error}")),
    };
    let Some(existing) = existing else {
        write_regular_file(path, &block, "agent doc")?;
        return Ok(true);
    };
    let updated = match (
        existing.find(HANDOFF_BLOCK_START),
        existing.find(HANDOFF_BLOCK_END),
    ) {
        (Some(start), Some(end)) if end > start => {
            let end = end + HANDOFF_BLOCK_END.len();
            let end = if existing[end..].starts_with('\n') {
                end + 1
            } else {
                end
            };
            format!("{}{block}{}", &existing[..start], &existing[end..])
        }
        _ => {
            let mut text = existing.clone();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&block);
            text
        }
    };
    if updated != existing {
        write_regular_file(path, &updated, "agent doc")?;
    }
    Ok(false)
}

fn add_git_excludes(git_common_dir: &Path, names: &[&str]) -> Result<(), String> {
    let info = git_common_dir.join("info");
    if let Ok(metadata) = fs::symlink_metadata(&info)
        && !metadata.file_type().is_dir()
    {
        return Err(
            "refusing to write Git exclude: info path is a symlink or not a directory".to_owned(),
        );
    }
    fs::create_dir_all(&info)
        .map_err(|error| format!("cannot create Git info directory: {error}"))?;
    let exclude = info.join("exclude");
    let existing = match fs::symlink_metadata(&exclude) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                return Err(
                    "refusing to overwrite Git exclude: target is a symlink or non-regular file"
                        .to_owned(),
                );
            }
            fs::read_to_string(&exclude)
                .map_err(|error| format!("cannot read Git exclude file: {error}"))?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("cannot stat Git exclude file: {error}")),
    };
    let mut updated = existing.clone();
    for name in names {
        if updated.lines().any(|line| line.trim() == *name) {
            continue;
        }
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push_str(name);
        updated.push('\n');
    }
    if updated == existing {
        return Ok(());
    }
    write_regular_file(&exclude, &updated, "Git exclude file")
}

/// Write `content` to `path` atomically (temp file in the same directory +
/// rename), refusing to follow symlinks or clobber non-regular files.
fn write_regular_file(path: &Path, content: &str, context: &str) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.file_type().is_file())
    {
        return Err(format!(
            "refusing to overwrite {context}: target is a symlink or non-regular file"
        ));
    }
    let Some(dir) = path.parent() else {
        return Err(format!("cannot write {context}: no parent directory"));
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("handoff");
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = fs::write(&temp, content)
        .map_err(|error| format!("cannot write {context} temp file: {error}"))
        .and_then(|()| {
            fs::rename(&temp, path).map_err(|error| format!("cannot write {context}: {error}"))
        });
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn resolve_git_dir(worktree: &Path) -> Result<Option<PathBuf>, String> {
    let dot_git = worktree.join(".git");
    if dot_git.is_dir() {
        return Ok(Some(dot_git));
    }
    if !dot_git.is_file() {
        return Ok(None);
    }
    let marker = fs::read_to_string(&dot_git)
        .map_err(|error| format!("cannot read linked-worktree .git file: {error}"))?;
    let Some(raw) = marker.trim().strip_prefix("gitdir:") else {
        return Err("linked-worktree .git file has no gitdir marker".to_owned());
    };
    let path = PathBuf::from(raw.trim());
    Ok(Some(if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    }))
}

/// Git reads `info/exclude` from the COMMON dir, which differs from the
/// per-worktree gitdir for linked worktrees: `<gitdir>/commondir` (relative
/// to the gitdir) points at it when present.
fn resolve_git_common_dir(worktree: &Path) -> Result<Option<PathBuf>, String> {
    let Some(git_dir) = resolve_git_dir(worktree)? else {
        return Ok(None);
    };
    match fs::read_to_string(git_dir.join("commondir")) {
        Ok(raw) => {
            let raw = raw.trim();
            if raw.is_empty() {
                return Err("Git commondir file is empty".to_owned());
            }
            let path = PathBuf::from(raw);
            Ok(Some(if path.is_absolute() {
                path
            } else {
                git_dir.join(path)
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Some(git_dir)),
        Err(error) => Err(format!("cannot read Git commondir file: {error}")),
    }
}

pub(crate) fn start_background(store: &StateStore, cache: Arc<EventCache>) {
    let Some(path) = store.path().map(Path::to_path_buf) else {
        return;
    };
    let _ = thread::Builder::new()
        .name("herdr-mcp-work-compactor".to_owned())
        .spawn(move || {
            loop {
                let path = &path;
                let cache = Arc::clone(&cache);
                // AssertUnwindSafe: a panicking tick can only poison this
                // loop's own freshly-opened store handle, never shared state.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let store = StateStore::open(path)?;
                    let snapshot = cache.snapshot();
                    // Relay memory scope = the unified validated-root surface:
                    // managed Git roots plus vcs-less operational roots proven
                    // by the live topology. Project key = the git root when
                    // any, else the operational folder itself.
                    let topology = crate::projects::derive_routing(&snapshot);
                    let roots = crate::fs_security::validated_roots_from(&topology);
                    let refs = roots
                        .iter()
                        .map(|root| {
                            (
                                root.clone(),
                                crate::work_capture::project_ref_for(&snapshot, root),
                            )
                        })
                        .collect::<std::collections::BTreeMap<_, _>>();
                    let now = now_ms();
                    compact_tick(
                        &store,
                        &SemanticService::from_config(),
                        &move |store| {
                            crate::work_tailers::ingest_native_logs(store, &roots, &refs, now)
                        },
                        now,
                    )
                    .map(|_| ())
                }));
                thread::sleep(COMPACTION_INTERVAL);
            }
        });
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod audit_fix_tests {
    use super::*;

    #[test]
    fn empty_llm_ops_are_rejected_so_fallback_runs() {
        assert!(serde_json::from_str::<HandoffOps>("{}").unwrap().is_empty());
        assert!(
            serde_json::from_str::<HandoffOps>(r#"{"ok":true}"#)
                .unwrap()
                .is_empty()
        );
        assert!(
            !serde_json::from_str::<HandoffOps>(r#"{"recent":["pi: x"]}"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn next_id_saturates_and_remove_trims() {
        let items = vec![RelayItem {
            id: format!("g{}", u64::MAX),
            text: "x".to_owned(),
        }];
        assert_eq!(next_id(&items, "g"), format!("g{}", u64::MAX));
        let mut items = vec![RelayItem {
            id: "g1".to_owned(),
            text: "keep?".to_owned(),
        }];
        let ops: Vec<ItemOp> = serde_json::from_str(r#"[{"op":"remove","id":" g1 "}]"#).unwrap();
        apply_item_ops(&mut items, ops, "g");
        assert!(items.is_empty());
    }

    #[test]
    fn agent_docs_get_idempotent_managed_block() {
        let dir = std::env::temp_dir().join(format!("hrm-docs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("CLAUDE.md"), "# Mine\nkeep me\n").unwrap();
        write_handoff_current(&dir, "h1").unwrap();
        write_handoff_current(&dir, "h2").unwrap();
        let agents = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        let claude = fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
        assert!(agents.contains("HANDOFF.md") && agents.contains(HANDOFF_BLOCK_START));
        assert!(claude.starts_with("# Mine\nkeep me\n"));
        assert_eq!(claude.matches(HANDOFF_BLOCK_START).count(), 1);
        assert_eq!(agents.matches(HANDOFF_BLOCK_END).count(), 1);
        // Only the managed block is rewritten.
        fs::write(
            dir.join("CLAUDE.md"),
            claude.replace("auto-maintained by herdr-mcp", "stale") + "tail\n",
        )
        .unwrap();
        write_handoff_current(&dir, "h3").unwrap();
        let claude = fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
        assert!(claude.contains("auto-maintained by herdr-mcp") && claude.ends_with("tail\n"));
        assert_eq!(fs::read_to_string(dir.join("HANDOFF.md")).unwrap(), "h3");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_orders_principles_first_and_history_last_newest_first() {
        let mut memory = RelayMemory::default();
        memory.preferences.push(RelayItem {
            id: "p1".into(),
            text: "old pref".into(),
        });
        memory.preferences.push(RelayItem {
            id: "p2".into(),
            text: "new pref".into(),
        });
        memory.mistakes.push(RelayItem {
            id: "m1".into(),
            text: "tabs; use 4 spaces".into(),
        });
        memory.tasks.push(RelayTask {
            id: "t1".into(),
            text: "old task".into(),
            status: "blocked".into(),
            updated_ms: None,
        });
        memory.tasks.push(RelayTask {
            id: "t2".into(),
            text: "new task".into(),
            status: "todo".into(),
            updated_ms: None,
        });
        memory.tasks.push(RelayTask {
            id: "t3".into(),
            text: "finished task".into(),
            status: "done".into(),
            updated_ms: None,
        });
        memory.goals.push(RelayItem {
            id: "g1".into(),
            text: "hidden goal".into(),
        });
        memory.current_state = "hidden state".into();
        memory.sources = vec![
            "pi 2026-10-17 07:00 UTC /a.jsonl".into(),
            "omp 2026-10-17 08:00 UTC /b.jsonl".into(),
        ];
        let md = render_handoff(&memory);
        let body = &md[..md.find("<!--").unwrap()];
        let pos = |needle: &str| body.find(needle).unwrap();
        assert!(pos("## 规则") < pos("### 公共原则/偏好规范"));
        assert!(pos("### 公共原则/偏好规范") < pos("### 错题集"));
        assert!(pos("### 错题集") < pos("## 任务清单"));
        assert!(pos("## 任务清单") < pos("## 历史会话路径"));
        assert!(pos("new pref") < pos("old pref"));
        assert!(pos("new task") < pos("old task"));
        assert!(pos("/b.jsonl") < pos("/a.jsonl"));
        assert!(!body.contains("hidden goal") && !body.contains("hidden state"));
        // Closed work is never rendered.
        assert!(!body.contains("finished task"));
    }

    #[test]
    fn agent_edits_of_handoff_are_folded_back() {
        let mut memory = RelayMemory::default();
        memory.preferences.push(RelayItem {
            id: "p1".into(),
            text: "use tabs".into(),
        });
        memory.tasks.push(RelayTask {
            id: "t1".into(),
            text: "write add()".into(),
            status: "doing".into(),
            updated_ms: None,
        });
        let edited = render_handoff(&memory)
            .replace("- [p1] use tabs", "- [p1] use 4 spaces\n- never add deps")
            .replace("- [t1] (doing) write add()", "- [t1] (done) write add()");
        assert!(fold_file_edits(&mut memory, &edited, 10));
        let texts: Vec<_> = memory
            .preferences
            .iter()
            .map(|item| item.text.as_str())
            .collect();
        assert!(texts.contains(&"use 4 spaces") && texts.contains(&"never add deps"));
        assert_eq!(
            memory
                .preferences
                .iter()
                .filter(|item| item.id == "p1")
                .count(),
            1
        );
        assert_eq!(memory.tasks[0].status, "done");
        assert!(!fold_file_edits(&mut memory, "unrelated text", 10));
    }

    #[test]
    fn sources_record_native_log_paths_newest_last_deduped() {
        let mut memory = RelayMemory::default();
        let event = |id: i64, src: &str| WorkEvent {
            event_id: id,
            ts: 1_792_220_000_000 + id,
            project_ref: "p".into(),
            worktree: "/w".into(),
            provider: "pi".into(),
            account: None,
            kind: "user_prompt".into(),
            summary: "x".into(),
            source_ref: src.into(),
        };
        record_sources(
            &mut memory,
            &[
                event(1, "native-log:/s/a.jsonl"),
                event(2, "mcp:herdr_exec"),
                event(3, "native-log:/s/b.jsonl"),
                event(4, "native-log:/s/a.jsonl"),
            ],
        );
        assert_eq!(memory.sources.len(), 2);
        assert!(
            memory.sources[1].ends_with(" /s/a.jsonl") && memory.sources[1].starts_with("pi 2026-")
        );
    }

    #[test]
    fn hidden_non_git_root_gets_no_handoff_file() {
        let base = std::env::temp_dir().join(format!("hrm-{}", std::process::id()));
        let hidden = base.join(".secretdir");
        fs::create_dir_all(&hidden).unwrap();
        write_handoff_current(&hidden, "x").unwrap();
        assert!(!hidden.join("HANDOFF.md").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn relay_times_are_shanghai_with_explicit_offset() {
        // 2026-10-17T08:18:00Z == 16:18 in Asia/Shanghai.
        assert_eq!(relay_time(1_792_225_080_000), "2026-10-17 16:18 +08:00");
        // Day rollover.
        assert_eq!(relay_time(1_792_252_800_000), "2026-10-18 00:00 +08:00");
        assert_eq!(
            normalize_legacy_times("pi 2026-10-17 08:18 UTC /a.jsonl"),
            "pi 2026-10-17 16:18 +08:00 /a.jsonl"
        );
        let mut memory = RelayMemory {
            sources: vec!["pi 2026-10-16 23:30 UTC /old.jsonl".into()],
            ..Default::default()
        };
        let rendered = render_handoff(&memory);
        assert!(rendered.contains("pi 2026-10-17 07:30 +08:00 /old.jsonl"));
        assert!(
            !rendered
                .lines()
                .any(|line| line.starts_with("- ") && line.contains(" UTC "))
        );
        record_sources(&mut memory, &[]);
        assert_eq!(
            memory.sources,
            vec!["pi 2026-10-17 07:30 +08:00 /old.jsonl".to_owned()]
        );
    }

    #[test]
    fn scope_guard_refuses_home_ancestors_and_global_agent_dirs() {
        let base = std::env::temp_dir().join(format!("herdr-scope-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        let base = fs::canonicalize(base).unwrap();
        let home = base.join("home/someone");
        fs::create_dir_all(&home).unwrap();
        let allowed = |path: &Path| is_allowed_project_root_with_home(path, Some(&home));
        assert!(!allowed(Path::new("/")));
        assert!(!allowed(&base));
        assert!(!allowed(&base.join("home")));
        assert!(!allowed(&home));
        for hidden in [".agents", ".claude", ".codex", ".config/foo"] {
            assert!(!allowed(&home.join(hidden)), "{hidden}");
        }
        assert!(allowed(&home.join("code/proj")));
        assert!(allowed(Path::new("/workspace/proj")));
        assert!(allowed(&home.join("code/.hidden-proj")));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn handoff_texts_state_project_only_scope() {
        assert!(handoff_block().contains("this project only"));
        assert!(handoff_block().contains("~/.agents/AGENTS.md"));
        let rendered = render_handoff(&RelayMemory::default());
        assert!(rendered.contains(RULES_SCOPE_NOTE));
        // The note is not an item: folding the file back adds no rule.
        let mut memory = RelayMemory::default();
        fold_file_edits(&mut memory, &rendered, 10);
        assert!(memory.preferences.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_store::work_journal::NewWorkEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeSummarizer(Result<&'static str, &'static str>);

    impl WorkSummarizer for FakeSummarizer {
        fn summarize(&self, _prompt: &str) -> Result<String, String> {
            self.0.map(str::to_owned).map_err(str::to_owned)
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "herdr-work-compactor-{label}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn append(store: &StateStore, root: &Path, summary: &str, ts: i64) {
        store
            .append_work_event(&NewWorkEvent {
                ts,
                project_ref: "proj".to_owned(),
                worktree: root.to_string_lossy().into_owned(),
                provider: "test".to_owned(),
                account: None,
                kind: "edit".to_owned(),
                summary: summary.to_owned(),
                source_ref: "test:event".to_owned(),
                dedupe_key: None,
            })
            .unwrap();
    }

    #[test]
    fn semantic_success_compacts_and_preserves_cursor_state() {
        let root = temp_dir("success");
        fs::create_dir(root.join(".git")).unwrap();
        let store = StateStore::open(":memory:").unwrap();
        store
            .put_work_state(&WorkState {
                project_ref: "proj".to_owned(),
                worktree: root.to_string_lossy().into_owned(),
                handoff_md: "legacy old handoff".to_owned(),
                through_event_id: 0,
                log_cursors_json: r#"{"pi":7}"#.to_owned(),
                updated_at: 1,
            })
            .unwrap();
        append(&store, &root, "changed parser", 2);
        let calls = AtomicUsize::new(0);
        compact_tick(
            &store,
            &FakeSummarizer(Ok(
                r#"{"goals":[{"op":"add","id":"g1","text":"ship relay memory"}],"current_state":"parser rework underway","recent":["test: [edit] changed parser"]}"#,
            )),
            &|_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(0)
            },
            10,
        )
        .unwrap();
        let state = store
            .work_state("proj", root.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(state.handoff_md.starts_with("# HANDOFF"));
        assert!(state.handoff_md.contains("## 规则"));
        assert!(state.handoff_md.contains("## 任务清单"));
        assert!(state.handoff_md.contains("## 历史会话路径"));
        let memory = parsed_memory(&state.handoff_md).unwrap();
        assert_eq!(memory.goals.len(), 1);
        assert_eq!(memory.current_state, "parser rework underway");
        assert_eq!(memory.recent, vec!["test: [edit] changed parser"]);
        assert_eq!(state.log_cursors_json, r#"{"pi":7}"#);
        assert_eq!(
            fs::read_to_string(root.join("HANDOFF.md")).unwrap(),
            state.handoff_md
        );
        assert_eq!(
            fs::read_to_string(root.join(".git/info/exclude")).unwrap(),
            "HANDOFF.md\nAGENTS.md\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provider_failure_and_bad_json_use_bounded_rule_fallback() {
        for (label, fake) in [
            ("failure", FakeSummarizer(Err("offline"))),
            ("bad-json", FakeSummarizer(Ok("not json"))),
        ] {
            let root = temp_dir(label);
            let store = StateStore::open(":memory:").unwrap();
            store
                .put_work_state(&WorkState {
                    project_ref: "proj".to_owned(),
                    worktree: root.to_string_lossy().into_owned(),
                    handoff_md: "# Previous".to_owned(),
                    log_cursors_json: "{}".to_owned(),
                    ..WorkState::default()
                })
                .unwrap();
            append(&store, &root, "observable change", 2);
            compact_tick(&store, &fake, &|_| Ok(0), 10).unwrap();
            let handoff = store
                .work_state("proj", root.to_str().unwrap())
                .unwrap()
                .unwrap()
                .handoff_md;
            // The legacy free-form handoff migrates into 当前状态.
            assert!(handoff.contains("# Previous"), "{handoff}");
            assert!(handoff.contains("observable change"), "{handoff}");
            assert!(handoff.contains(MEMORY_OPEN), "{handoff}");
            assert!(handoff.chars().count() <= HANDOFF_MAX_CHARS);
            let memory = parsed_memory(&handoff).unwrap();
            assert!(memory.current_state.contains("# Previous"));
            assert!(
                memory
                    .recent
                    .iter()
                    .any(|line| line.contains("observable change"))
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn compact_tick_applies_event_retention_after_compaction() {
        let root = temp_dir("retention");
        let store = StateStore::open(":memory:").unwrap();
        append(&store, &root, "expired event", 0);
        let now = crate::state_store::work_journal::WORK_RETENTION_MS + 1;
        compact_tick(&store, &FakeSummarizer(Err("offline")), &|_| Ok(0), now).unwrap();
        assert!(
            store
                .latest_work_events("proj", root.to_str().unwrap(), 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .work_state("proj", root.to_str().unwrap())
                .unwrap()
                .is_some()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn linked_worktree_gitdir_gets_one_exclude_line() {
        let root = temp_dir("linked");
        let git_dir = temp_dir("git-dir");
        fs::write(
            root.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();
        write_handoff_current(&root, "handoff one").unwrap();
        write_handoff_current(&root, "handoff two").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("HANDOFF.md")).unwrap(),
            "handoff two"
        );
        assert_eq!(
            fs::read_to_string(git_dir.join("info/exclude")).unwrap(),
            "HANDOFF.md\nAGENTS.md\n"
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(git_dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_handoff_and_exclude_are_never_overwritten() {
        use std::os::unix::fs::symlink;

        // A symlinked HANDOFF.md must not overwrite the link target.
        let root = temp_dir("symlink-handoff");
        fs::create_dir(root.join(".git")).unwrap();
        let target = temp_dir("symlink-target").join("keep.md");
        fs::write(&target, "do not touch").unwrap();
        let link = root.join("HANDOFF.md");
        symlink(&target, &link).unwrap();
        let error = write_handoff_current(&root, "attacker content").unwrap_err();
        assert!(error.contains("symlink"), "unexpected error: {error}");
        assert_eq!(fs::read_to_string(&target).unwrap(), "do not touch");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself must be left in place"
        );
        let leftovers = fs::read_dir(&root)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".HANDOFF.md.")
            })
            .count();
        assert_eq!(leftovers, 0, "no temp files may be left behind");
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(target.parent().unwrap()).unwrap();

        // Same protection for the Git exclude file.
        let root = temp_dir("symlink-exclude");
        let info = root.join(".git/info");
        fs::create_dir_all(&info).unwrap();
        let target = temp_dir("exclude-target").join("exclude");
        fs::write(&target, "original\n").unwrap();
        symlink(&target, info.join("exclude")).unwrap();
        let error = write_handoff_current(&root, "handoff").unwrap_err();
        assert!(error.contains("symlink"), "unexpected error: {error}");
        assert_eq!(fs::read_to_string(&target).unwrap(), "original\n");
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(target.parent().unwrap()).unwrap();
    }

    struct RecordingSummarizer {
        prompts: std::sync::Mutex<Vec<String>>,
    }

    impl WorkSummarizer for RecordingSummarizer {
        fn summarize(&self, prompt: &str) -> Result<String, String> {
            self.prompts.lock().unwrap().push(prompt.to_owned());
            Ok(r#"{"current_state":"compacted","recent":["test: folded batch"]}"#.to_owned())
        }
    }

    fn prompt_event_lines(prompt: &str) -> usize {
        // Prompts carry escaped line breaks (Edge rejects control chars).
        prompt
            .split("\\n")
            .filter(|line| line.starts_with("- [edit]"))
            .count()
    }

    #[test]
    fn new_events_compact_in_bounded_batches_and_resume_next_tick() {
        let root = temp_dir("batches");
        let store = StateStore::open(":memory:").unwrap();
        let base_ts = now_ms();
        let mut ids = Vec::new();
        for index in 0..(EVENT_BATCH_SIZE * EVENT_BATCHES_PER_TICK + 50) {
            ids.push(
                store
                    .append_work_event(&NewWorkEvent {
                        ts: base_ts + index as i64,
                        project_ref: "proj".to_owned(),
                        worktree: root.to_string_lossy().into_owned(),
                        provider: "test".to_owned(),
                        account: None,
                        kind: "edit".to_owned(),
                        summary: format!("change {index}"),
                        source_ref: format!("test:{index}"),
                        dedupe_key: None,
                    })
                    .unwrap()
                    .unwrap(),
            );
        }
        let summarizer = RecordingSummarizer {
            prompts: std::sync::Mutex::new(Vec::new()),
        };

        // One tick folds at most EVENT_BATCHES_PER_TICK bounded batches and
        // advances the cursor only to the last event actually included.
        compact_tick(&store, &summarizer, &|_| Ok(0), now_ms()).unwrap();
        let state = store
            .work_state("proj", root.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            state.through_event_id,
            ids[EVENT_BATCH_SIZE * EVENT_BATCHES_PER_TICK - 1]
        );
        let prompts = summarizer.prompts.lock().unwrap();
        assert_eq!(prompts.len(), EVENT_BATCHES_PER_TICK);
        for prompt in prompts.iter() {
            assert!(prompt_event_lines(prompt) <= EVENT_BATCH_SIZE);
        }
        assert_eq!(prompt_event_lines(&prompts[0]), EVENT_BATCH_SIZE);
        assert!(prompts[0].contains("change 0"));
        assert!(!prompts[0].contains("change 100 ("));
        drop(prompts);
        // The backlog is still pending and is picked up by a later tick.
        assert!(!store.work_scopes_with_new_events().unwrap().is_empty());
        compact_tick(&store, &summarizer, &|_| Ok(0), now_ms()).unwrap();
        let state = store
            .work_state("proj", root.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(state.through_event_id, *ids.last().unwrap());
        assert!(store.work_scopes_with_new_events().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_or_stale_handoff_file_is_rewritten_without_new_events() {
        let root = temp_dir("reconcile");
        fs::create_dir(root.join(".git")).unwrap();
        let store = StateStore::open(":memory:").unwrap();
        // Simulate an export that never landed: state saved, no file.
        store
            .put_work_state(&WorkState {
                project_ref: "proj".to_owned(),
                worktree: root.to_string_lossy().into_owned(),
                handoff_md: "# recovered".to_owned(),
                through_event_id: 3,
                log_cursors_json: "{}".to_owned(),
                updated_at: 5,
            })
            .unwrap();
        assert_eq!(
            compact_tick(&store, &FakeSummarizer(Err("unused")), &|_| Ok(0), 10).unwrap(),
            0
        );
        assert_eq!(
            fs::read_to_string(root.join("HANDOFF.md")).unwrap(),
            "# recovered"
        );

        // A drifted file is rewritten even with no new events.
        fs::write(root.join("HANDOFF.md"), "stale").unwrap();
        compact_tick(&store, &FakeSummarizer(Err("unused")), &|_| Ok(0), 11).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("HANDOFF.md")).unwrap(),
            "# recovered"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// relay memory dedupe | Given repeated and truncated add ops for the same
    /// rule | When ops apply | Then one item keeps the longest text and no
    /// duplicate id is created.
    #[test]
    fn duplicate_and_truncated_adds_update_one_item() {
        let mut items: Vec<RelayItem> = Vec::new();
        let ops: Vec<ItemOp> = serde_json::from_str(
            r#"[{"op":"add","id":"p133","text":"Always run the full test suite."},{"op":"add","id":"p134","text":"always   run the full test suite"},{"op":"add","id":"p135","text":"Always run the full test suite…"}]"#,
        )
        .unwrap();
        apply_item_ops(&mut items, ops, "p");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "p133");
        assert_eq!(items[0].text, "Always run the full test suite.");

        // A clipped copy adds nothing and must not shorten the item.
        let ops: Vec<ItemOp> = serde_json::from_str(
            r#"[{"op":"add","id":"p136","text":"Always run the full test…"}]"#,
        )
        .unwrap();
        apply_item_ops(&mut items, ops, "p");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "p133");
        assert_eq!(items[0].text, "Always run the full test suite.");
    }

    /// relay memory dedupe | Given an older rule that ends in a truncation
    /// marker and a newer full copy of it | When dedupe merges the pair | Then
    /// the text without the marker is kept even though the clipped copy is not
    /// shorter.
    #[test]
    fn user_rule_dedupe_keeps_the_untruncated_text() {
        let mut items = vec![
            RelayItem {
                id: "p1".to_owned(),
                text: "Always run the full test…".to_owned(),
            },
            RelayItem {
                id: "p2".to_owned(),
                text: "Always run the full test.".to_owned(),
            },
        ];
        dedupe_items(&mut items);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].id, "p1");
        assert_eq!(items[0].text, "Always run the full test.");

        // Same choice on the write path (a later add restating the clipped id)
        // and for tasks.
        let mut written: Vec<RelayItem> = Vec::new();
        let ops: Vec<ItemOp> = serde_json::from_str(
            r#"[{"op":"add","id":"p1","text":"Always run the full test…"},{"op":"add","id":"p2","text":"Always run the full test."}]"#,
        )
        .unwrap();
        apply_item_ops(&mut written, ops, "p");
        assert_eq!(written.len(), 1, "{written:?}");
        assert_eq!(written[0].text, "Always run the full test.");

        let mut tasks = vec![
            RelayTask {
                id: "t1".to_owned(),
                text: "Always run the full test…".to_owned(),
                status: "doing".to_owned(),
                updated_ms: Some(3),
            },
            RelayTask {
                id: "t2".to_owned(),
                text: "Always run the full test.".to_owned(),
                status: "doing".to_owned(),
                updated_ms: Some(9),
            },
        ];
        dedupe_tasks(&mut tasks);
        assert_eq!(tasks.len(), 1, "{tasks:?}");
        assert_eq!(tasks[0].text, "Always run the full test.");
    }

    /// relay task dedupe | Given the same task under two ids where the newer
    /// copy is done | When ops apply, dedupe runs and finished tasks are
    /// refreshed away | Then the merged task keeps the newer status and no
    /// open duplicate of it survives.
    #[test]
    fn user_task_dedupe_keeps_the_newest_status() {
        let mut memory = RelayMemory::default();
        let add: HandoffOps = serde_json::from_str(
            r#"{"tasks":[{"op":"add","id":"t1","text":"Alpha","status":"todo"},{"op":"add","id":"t2","text":"Beta","status":"doing"}]}"#,
        )
        .unwrap();
        apply_memory_ops(&mut memory, add, 5);
        assert_eq!(memory.tasks.len(), 2, "{:?}", memory.tasks);

        // An explicit update of the newer id restates t1's text as done.
        let update: HandoffOps = serde_json::from_str(
            r#"{"tasks":[{"op":"update","id":"t2","text":"Alpha","status":"done"}]}"#,
        )
        .unwrap();
        apply_memory_ops(&mut memory, update, 9);
        assert_eq!(memory.tasks.len(), 1, "{:?}", memory.tasks);
        assert_eq!(memory.tasks[0].id, "t1");
        assert_eq!(memory.tasks[0].status, "done");
        assert_eq!(memory.tasks[0].updated_ms, Some(9));

        refresh_tasks(&mut memory, None, 10);
        assert!(
            !memory.tasks.iter().any(|task| task.status != "done"),
            "no open duplicate survives: {:?}",
            memory.tasks
        );
    }

    /// relay file edits | Given two lines for one task with conflicting statuses
    /// | When the file is folded back | Then the task keeps the status of the
    /// copy that was rendered higher up (the newest line).
    #[test]
    fn user_task_file_fold_keeps_the_newest_status() {
        let file =
            "## 任务清单\n\n- [t9] (done) 补齐 HANDOFF 去重\n- [t3] (todo) 补齐 HANDOFF 去重\n";
        let mut memory = RelayMemory::default();
        assert!(fold_file_edits(&mut memory, file, 7));
        assert_eq!(memory.tasks.len(), 1, "{:?}", memory.tasks);
        assert_eq!(memory.tasks[0].id, "t3");
        assert_eq!(memory.tasks[0].status, "done");
        refresh_tasks(&mut memory, None, 8);
        assert!(memory.tasks.is_empty(), "{:?}", memory.tasks);
    }

    /// relay memory dedupe | Given a prefix-related but not clipped pair of
    /// texts | When duplicate detection runs | Then neither swallows the
    /// other, while an explicitly clipped copy still folds into its source.
    #[test]
    fn prefix_dedupe_only_merges_a_clipped_copy() {
        assert!(!same_text("test", "test deployment"));
        let mut items: Vec<RelayItem> = Vec::new();
        let ops: Vec<ItemOp> = serde_json::from_str(
            r#"[{"op":"add","id":"p1","text":"test"},{"op":"add","id":"p2","text":"test deployment"}]"#,
        )
        .unwrap();
        apply_item_ops(&mut items, ops, "p");
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[0].text, "test");
        assert_eq!(items[1].text, "test deployment");

        assert!(same_text("test deploy…", "test deployment"));
        assert!(same_text("test deploy...", "test deployment"));
        let ops: Vec<ItemOp> =
            serde_json::from_str(r#"[{"op":"add","id":"p3","text":"test deploy…"}]"#).unwrap();
        apply_item_ops(&mut items, ops, "p");
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[1].id, "p2");
        assert_eq!(items[1].text, "test deployment");
    }

    /// relay memory dedupe | Given an explicit-id update that restates another
    /// item | When ops apply | Then the section holds one item with the
    /// earliest id.
    #[test]
    fn explicit_update_restating_another_item_leaves_one_item() {
        let mut memory = RelayMemory {
            preferences: vec![
                RelayItem {
                    id: "p1".to_owned(),
                    text: "Alpha".to_owned(),
                },
                RelayItem {
                    id: "p2".to_owned(),
                    text: "Beta".to_owned(),
                },
            ],
            ..RelayMemory::default()
        };
        let ops: HandoffOps =
            serde_json::from_str(r#"{"preferences":[{"op":"update","id":"p2","text":"alpha."}]}"#)
                .unwrap();
        apply_memory_ops(&mut memory, ops, 5);
        assert_eq!(memory.preferences.len(), 1, "{:?}", memory.preferences);
        assert_eq!(memory.preferences[0].id, "p1");
        assert_eq!(memory.preferences[0].text, "Alpha");
    }

    /// relay task dedupe | Given the same task added under fresh ids | When
    /// ops apply | Then one task exists with a fresh update stamp.
    #[test]
    fn duplicate_and_truncated_task_adds_update_one_task() {
        let mut tasks: Vec<RelayTask> = Vec::new();
        let ops: Vec<TaskOp> = serde_json::from_str(
            r#"[{"op":"add","id":"t1","text":"补齐 HANDOFF 去重","status":"doing"},{"op":"add","id":"t2","text":"补齐 HANDOFF 去重…","status":"done"}]"#,
        )
        .unwrap();
        apply_task_ops(&mut tasks, ops, 5);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "t1");
        assert_eq!(tasks[0].status, "done");
        assert_eq!(tasks[0].updated_ms, Some(5));
    }

    /// relay file edits | Given a newer full line above an older clipped copy
    /// of the same rule | When the file is folded back | Then one item keeps
    /// the oldest id and the longer, non-truncated text.
    #[test]
    fn duplicate_file_lines_keep_the_oldest_id_and_fullest_text() {
        let file = "## 规则\n\n### 公共原则/偏好规范\n\n- [p9] Always run the full test suite before reporting green.\n- [p1] Always run the full test…\n";
        let mut memory = RelayMemory::default();
        assert!(fold_file_edits(&mut memory, file, 7));
        assert_eq!(memory.preferences.len(), 1, "{:?}", memory.preferences);
        assert_eq!(memory.preferences[0].id, "p1");
        assert_eq!(
            memory.preferences[0].text,
            "Always run the full test suite before reporting green."
        );
    }

    /// relay file edits | Given duplicate rule and task lines in HANDOFF.md |
    /// When the file is folded back | Then each duplicate collapses to the
    /// oldest id (the file is rendered newest-first).
    #[test]
    fn duplicate_file_lines_fold_to_the_oldest_id() {
        let file = "## 规则\n\n### 公共原则/偏好规范\n\n- [p9] Always run the full test suite.\n- [p1] always run the full test suite\n\n### 错题集\n\n## 任务清单\n\n- [t7] (todo) 补齐 HANDOFF 去重\n- [t3] (todo) 补齐 HANDOFF 去重\n";
        let mut memory = RelayMemory::default();
        assert!(fold_file_edits(&mut memory, file, 7));
        assert_eq!(memory.preferences.len(), 1);
        assert_eq!(memory.preferences[0].id, "p1");
        assert_eq!(memory.tasks.len(), 1);
        assert_eq!(memory.tasks[0].id, "t3");
    }

    /// relay prompt | Given a memory whose 30 source-path entries alone
    /// exceed the semantic message budget | When the compaction prompt is
    /// built | Then whole sources are dropped oldest-first rather than cutting
    /// anything, the prompt fits the cap, its memory JSON still parses and the
    /// new events survive.
    #[test]
    fn compaction_prompt_drops_whole_sources_before_clipping_anything() {
        let memory = RelayMemory {
            sources: (0..30)
                .map(|index| {
                    format!(
                        "pi 2026-10-{:02} 10:00 UTC /logs/{index}.jsonl {}",
                        index + 1,
                        "x".repeat(3_000)
                    )
                })
                .collect(),
            ..RelayMemory::default()
        };
        let events: Vec<WorkEvent> = (0..3)
            .map(|index| WorkEvent {
                event_id: index as i64 + 1,
                ts: index as i64,
                project_ref: "proj".to_owned(),
                worktree: "/repo".to_owned(),
                provider: "test".to_owned(),
                account: None,
                kind: "edit".to_owned(),
                summary: format!("source drop {index}"),
                source_ref: format!("test:{index}"),
            })
            .collect();
        let prompt = compaction_prompt(&memory, "", &events);
        assert!(
            prompt.len() <= SEMANTIC_PROMPT_MAX_BYTES,
            "{}",
            prompt.len()
        );
        let json = prompt
            .split("Current memory JSON:\\n")
            .nth(1)
            .and_then(|rest| rest.split("\\n\\nLegacy context").next())
            .expect("memory JSON in the prompt");
        // Parsing the whole document is the real invariant: the memory JSON is
        // never clipped, only whole entries may leave the prompt copy.
        let parsed: RelayMemory = serde_json::from_str(json).expect("whole memory JSON");
        assert!(parsed.sources.len() < memory.sources.len());
        for source in &parsed.sources {
            assert!(memory.sources.contains(source), "source text is never cut");
        }
        assert!(prompt.contains("New events:"));
        for index in 0..3 {
            assert!(prompt.contains(&format!("source drop {index}")), "{index}");
        }
        assert_eq!(memory.sources.len(), 30, "the stored memory keeps them all");
    }

    /// relay prompt | Given 400 long rules that cannot fit the semantic
    /// message budget | When the compaction prompt is built | Then it never
    /// exceeds the limit, its memory JSON still parses, every item it carries
    /// is whole, and the real summarizer request is accepted.
    #[test]
    fn compaction_prompt_stays_inside_the_semantic_message_limit() {
        let mut memory = memory_with_sections();
        for index in 0..400 {
            memory.preferences.push(RelayItem {
                id: format!("p{}", 1000 + index),
                text: format!("rule {index} {}", "x".repeat(400)),
            });
        }
        let events: Vec<WorkEvent> = (0..5)
            .map(|index| WorkEvent {
                event_id: index as i64 + 1,
                ts: index as i64,
                project_ref: "proj".to_owned(),
                worktree: "/repo".to_owned(),
                provider: "test".to_owned(),
                account: None,
                kind: "edit".to_owned(),
                summary: format!("change {index}"),
                source_ref: format!("test:{index}"),
            })
            .collect();
        let prompt = compaction_prompt(&memory, "", &events);
        assert!(
            prompt.len() <= SEMANTIC_PROMPT_MAX_BYTES,
            "{}",
            prompt.len()
        );
        let json = prompt
            .split("Current memory JSON:\\n")
            .nth(1)
            .and_then(|rest| rest.split("\\n\\nLegacy context").next())
            .expect("memory JSON in the prompt");
        let parsed: RelayMemory = serde_json::from_str(json).expect("whole memory JSON");

        // Only the OLDEST whole items are left out of the prompt; what is
        // included is complete, and the memory itself keeps everything.
        assert!(!parsed.preferences.is_empty());
        assert!(parsed.preferences.len() < memory.preferences.len());
        assert_eq!(memory.preferences.len(), 401);
        for item in &parsed.preferences {
            let source = memory
                .preferences
                .iter()
                .find(|source| source.id == item.id)
                .expect("included item exists in memory");
            assert_eq!(item.text, source.text, "item text was cut: {}", item.id);
            assert!(!item.text.ends_with('…'), "clipped item: {}", item.id);
        }
        // The newest rule survives and some events still ride along.
        assert_eq!(
            parsed.preferences.last().unwrap().id,
            "p1399",
            "newest rule must be kept"
        );
        for index in 0..5 {
            assert!(prompt.contains(&format!("change {index}")), "event missing");
        }
        // The request the summarizer actually sends passes semantic validation.
        assert!(crate::semantic::chat_request_valid(
            &[summarizer_message(&prompt)],
            &summarizer_options()
        ));
    }

    /// relay tasks | Given a merged PR, a merged branch, a stale task, a done
    /// task and a fresh open task | When the tick refreshes the task list |
    /// Then only the fresh open task survives.
    #[test]
    fn merged_prs_stale_and_done_tasks_are_closed() {
        let root = temp_dir("task-refresh");
        git(&root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("a.txt"), "a").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "feat(relay): compactor (#12)"]);
        git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(
            &root,
            &["update-ref", "refs/remotes/origin/fix/legacy-scope", "HEAD"],
        );

        let now = now_ms();
        let task = |id: &str, text: &str, status: &str, updated: Option<i64>| RelayTask {
            id: id.to_owned(),
            text: text.to_owned(),
            status: status.to_owned(),
            updated_ms: updated,
        };
        let mut memory = RelayMemory {
            tasks: vec![
                task("t1", "完成 PR #12 的合并验证", "doing", Some(now)),
                task("t2", "补完 fix/legacy-scope 的收尾", "todo", Some(now)),
                task(
                    "t3",
                    "30 天没动过的旧任务",
                    "todo",
                    Some(now - (STALE_TASK_DAYS + 1) * DAY_MS),
                ),
                task("t4", "仍然有效的任务", "doing", Some(now)),
                task("t5", "已经做完的任务", "done", Some(now)),
            ],
            ..RelayMemory::default()
        };
        refresh_tasks(&mut memory, Some(&merged_refs(&root)), now);
        let ids: Vec<&str> = memory.tasks.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(ids, ["t4"]);
        // Closed work is not rendered either.
        let rendered = render_handoff(&memory);
        for closed in ["t1", "t2", "t3", "t5"] {
            assert!(!rendered.contains(closed), "task {closed} still rendered");
        }

        // A legacy task without a stamp starts its clock on first load ...
        let mut legacy = RelayMemory {
            tasks: vec![task("t6", "没有时间戳的旧任务", "todo", None)],
            ..RelayMemory::default()
        };
        let refs = merged_refs(&root);
        refresh_tasks(&mut legacy, Some(&refs), now);
        assert_eq!(legacy.tasks.len(), 1);
        assert_eq!(legacy.tasks[0].updated_ms, Some(now));
        // ... and ages out only once it is genuinely stale.
        refresh_tasks(
            &mut legacy,
            Some(&refs),
            now + (STALE_TASK_DAYS + 1) * DAY_MS,
        );
        assert!(legacy.tasks.is_empty());

        // No open task closes nothing: git is not consulted at all.
        let mut closed_only = RelayMemory {
            tasks: vec![task("t7", "已完成的旧任务", "done", Some(now))],
            ..RelayMemory::default()
        };
        let mut cache = MergedRefCache::default();
        assert!(cache.for_memory(&closed_only, &root).is_none());
        refresh_tasks(&mut closed_only, None, now);
        assert!(closed_only.tasks.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    /// relay tasks | Given merge evidence in origin/main subjects and a merged
    /// branch | When the task list is refreshed | Then only a trailing `(#N)`
    /// or `Merge pull request #N` and a token-bounded branch name close a task.
    #[test]
    fn only_trailing_pr_and_token_bounded_branch_evidence_closes_tasks() {
        let root = temp_dir("merged-evidence");
        git(&root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("a.txt"), "a").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "fix: workaround for #123 (#456)"]);
        fs::write(root.join("b.txt"), "b").unwrap();
        git(&root, &["add", "."]);
        git(
            &root,
            &["commit", "-qm", "Merge pull request #789 from x/fix-b"],
        );
        git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(
            &root,
            &["update-ref", "refs/remotes/origin/fix/foo", "HEAD"],
        );

        let refs = merged_refs(&root);
        assert!(refs.prs.contains(&456), "{:?}", refs.prs);
        assert!(refs.prs.contains(&789), "{:?}", refs.prs);
        // "#123" is an issue reference inside the subject, not merge evidence.
        assert!(!refs.prs.contains(&123), "{:?}", refs.prs);

        let now = now_ms();
        let task = |id: &str, text: &str| RelayTask {
            id: id.to_owned(),
            text: text.to_owned(),
            status: "doing".to_owned(),
            updated_ms: Some(now),
        };
        let mut memory = RelayMemory {
            tasks: vec![
                task("t1", "跟进 issue #123 的根因"),
                task("t2", "完成 PR #456 的合并验证"),
                task("t3", "收尾 /pull/789 的合并"),
                task("t4", "收尾 fix/foo 分支"),
                task("t5", "收尾 fix/foobar 分支"),
            ],
            ..RelayMemory::default()
        };
        refresh_tasks(&mut memory, Some(&refs), now);
        let ids: Vec<&str> = memory.tasks.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(ids, ["t1", "t5"]);
        fs::remove_dir_all(root).unwrap();
    }

    /// relay tasks | Given a merged PR number | When the task text references
    /// it as an issue or as a bare `#N` | Then the task stays open, and only
    /// explicit PR semantics close it.
    #[test]
    fn only_pr_semantics_close_a_task_by_merged_number() {
        let refs = MergedRefs {
            prs: vec![456],
            branches: Vec::new(),
        };
        for closed in [
            "完成 PR #456 合并",
            "完成 PR#456 合并",
            "完成 pr 456 合并",
            "完成 PR  #456 合并",
            "https://github.com/example/repo/pull/456",
            // CJK text can sit directly against the marker.
            "完成PR#456合并",
        ] {
            assert!(refs.matched_by(closed), "must close: {closed}");
        }
        for open in [
            "调查 issue #456",
            "看下 #456 的原因",
            "issue #456 的根因",
            "PR 4567",
            "PRs #456",
            "PR-456",
        ] {
            assert!(!refs.matched_by(open), "must stay open: {open}");
        }
    }

    /// relay tasks | Given a stored handoff whose only task is stale and no
    /// new work events | When a real tick runs | Then the task is closed and
    /// HANDOFF.md is re-rendered without it.
    #[test]
    fn tick_closes_stale_tasks_without_new_events() {
        let root = temp_dir("tick-autoclose");
        fs::create_dir(root.join(".git")).unwrap();
        let store = StateStore::open(":memory:").unwrap();
        let now = now_ms();
        let stale = now - (STALE_TASK_DAYS + 1) * DAY_MS;
        let handoff = render_handoff(&RelayMemory {
            tasks: vec![RelayTask {
                id: "t1".to_owned(),
                text: "30 天没动过的旧任务".to_owned(),
                status: "todo".to_owned(),
                updated_ms: Some(stale),
            }],
            ..RelayMemory::default()
        });
        fs::write(root.join("HANDOFF.md"), &handoff).unwrap();
        store
            .put_work_state(&WorkState {
                project_ref: "proj".to_owned(),
                worktree: root.to_string_lossy().into_owned(),
                handoff_md: handoff.clone(),
                updated_at: stale,
                ..WorkState::default()
            })
            .unwrap();

        // No appended events: `work_scopes_with_new_events` is empty, so only
        // the per-tick task refresh can close this task.
        assert!(store.work_scopes_with_new_events().unwrap().is_empty());
        compact_tick(&store, &FakeSummarizer(Err("down")), &|_| Ok(0), now).unwrap();

        let state = store
            .work_state("proj", root.to_str().unwrap())
            .unwrap()
            .unwrap();
        let memory = parsed_memory(&state.handoff_md).unwrap();
        assert!(memory.tasks.is_empty(), "{:?}", memory.tasks);
        let on_disk = fs::read_to_string(root.join("HANDOFF.md")).unwrap();
        assert!(!on_disk.contains("[t1]"), "{on_disk}");
        assert_eq!(on_disk, state.handoff_md);
        fs::remove_dir_all(root).unwrap();
    }

    /// relay tasks | Given a stored memory with no open task and a worktree
    /// whose PR is already merged into origin/main | When an agent adds that
    /// task to HANDOFF.md and the tick folds the edit | Then merged-ref
    /// evidence comes from the memory actually refreshed, so the task is
    /// closed in the same tick.
    #[test]
    fn task_added_by_file_edit_is_closed_by_merged_refs_in_the_same_tick() {
        let root = temp_dir("reconcile-merged-edit");
        git(&root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("a.txt"), "a").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "feat(relay): compactor (#42)"]);
        git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);

        let store = StateStore::open(":memory:").unwrap();
        let now = now_ms();
        let handoff = render_handoff(&RelayMemory::default());
        store
            .put_work_state(&WorkState {
                project_ref: "proj".to_owned(),
                worktree: root.to_string_lossy().into_owned(),
                handoff_md: handoff.clone(),
                updated_at: now,
                ..WorkState::default()
            })
            .unwrap();
        // The stored memory holds no open task, so merged-ref evidence is only
        // needed after the agent edit below is folded in.
        fs::write(
            root.join("HANDOFF.md"),
            handoff.replace(
                "## 任务清单\n",
                "## 任务清单\n\n- [t4] (todo) 完成 PR #42 的收尾\n",
            ),
        )
        .unwrap();

        compact_tick(&store, &FakeSummarizer(Err("down")), &|_| Ok(0), now).unwrap();
        let state = store
            .work_state("proj", root.to_str().unwrap())
            .unwrap()
            .unwrap();
        let memory = parsed_memory(&state.handoff_md).unwrap();
        assert!(memory.tasks.is_empty(), "{:?}", memory.tasks);
        let on_disk = fs::read_to_string(root.join("HANDOFF.md")).unwrap();
        assert!(!on_disk.contains("PR #42"), "{on_disk}");
        assert_eq!(on_disk, state.handoff_md);
        fs::remove_dir_all(root).unwrap();
    }

    /// relay tasks | Given a state last written 31 days ago and an agent edit
    /// that adds a task | When the next tick folds that edit | Then the new
    /// task is stamped with the tick, not the old state time, and survives.
    #[test]
    fn folded_file_edit_uses_the_tick_time_not_the_stale_state_time() {
        let root = temp_dir("reconcile-clock");
        fs::create_dir(root.join(".git")).unwrap();
        let store = StateStore::open(":memory:").unwrap();
        let now = now_ms();
        let stale = now - (STALE_TASK_DAYS + 1) * DAY_MS;
        let handoff = render_handoff(&RelayMemory::default());
        store
            .put_work_state(&WorkState {
                project_ref: "proj".to_owned(),
                worktree: root.to_string_lossy().into_owned(),
                handoff_md: handoff.clone(),
                updated_at: stale,
                ..WorkState::default()
            })
            .unwrap();
        // An agent adds a task to the file long after the state was written.
        fs::write(
            root.join("HANDOFF.md"),
            handoff.replace("## 任务清单\n", "## 任务清单\n\n- [t9] (todo) 新接的任务\n"),
        )
        .unwrap();

        compact_tick(&store, &FakeSummarizer(Err("down")), &|_| Ok(0), now).unwrap();
        let state = store
            .work_state("proj", root.to_str().unwrap())
            .unwrap()
            .unwrap();
        let memory = parsed_memory(&state.handoff_md).unwrap();
        assert_eq!(memory.tasks.len(), 1, "{:?}", memory.tasks);
        assert_eq!(memory.tasks[0].text, "新接的任务");
        assert_eq!(memory.tasks[0].updated_ms, Some(now));

        // A later tick keeps it: the fold no longer back-dates the task.
        compact_tick(&store, &FakeSummarizer(Err("down")), &|_| Ok(0), now + 1000).unwrap();
        let state = store
            .work_state("proj", root.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(parsed_memory(&state.handoff_md).unwrap().tasks.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn real_linked_worktree_registers_exclude_in_common_dir() {
        let git_available = std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        if !git_available {
            eprintln!("git unavailable; skipping real linked-worktree test");
            return;
        }
        let git = |cwd: &Path, args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let base = temp_dir("real-worktree");
        let repo = base.join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        git(&repo, &["add", "seed.txt"]);
        git(&repo, &["commit", "-m", "seed"]);
        let linked = base.join("linked");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                linked.to_str().unwrap(),
                "HEAD",
            ],
        );

        write_handoff_current(&linked, "# linked handoff").unwrap();

        assert_eq!(
            fs::read_to_string(linked.join("HANDOFF.md")).unwrap(),
            "# linked handoff"
        );
        // Git consults <commondir>/info/exclude, not the per-worktree gitdir.
        let common_exclude = repo.join(".git/info/exclude");
        assert!(
            fs::read_to_string(common_exclude)
                .unwrap()
                .lines()
                .any(|line| line.trim() == "HANDOFF.md")
        );
        let ignored = std::process::Command::new("git")
            .args(["check-ignore", "HANDOFF.md"])
            .current_dir(&linked)
            .output()
            .unwrap();
        assert!(
            ignored.status.success(),
            "git check-ignore must ignore HANDOFF.md in the linked worktree: {}",
            String::from_utf8_lossy(&ignored.stderr)
        );
        git(
            &repo,
            &["worktree", "remove", "--force", linked.to_str().unwrap()],
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn semantic_prompt_truncates_on_utf8_byte_budget() {
        // CJK chars are 3 bytes each: a char-count clip would blow the 65,536
        // BYTE semantic message limit; the byte-budget clip must not.
        let cjk_previous = "工程进展".repeat(20_000);
        let events: Vec<WorkEvent> = (0..EVENT_BATCH_SIZE)
            .map(|index| WorkEvent {
                event_id: index as i64 + 1,
                ts: index as i64,
                project_ref: "proj".to_owned(),
                worktree: "/repo".to_owned(),
                provider: "test".to_owned(),
                account: None,
                kind: "edit".to_owned(),
                summary: "工作日志记录".repeat(50),
                source_ref: format!("test:{index}"),
            })
            .collect();
        let prompt = compaction_prompt(&RelayMemory::default(), &cjk_previous, &events);
        assert!(prompt.len() <= SEMANTIC_PROMPT_MAX_BYTES);
        assert!(prompt.ends_with('…'));

        assert!(
            !prompt
                .chars()
                .any(|ch| (ch as u32) < 0x20 || ch == '\u{7f}')
        );
        let edged = compaction_prompt(&RelayMemory::default(), "a\nb\tc\u{1}", &[]);
        assert!(edged.contains("a\\nb\\tc "));
        assert!(!edged.chars().any(|ch| (ch as u32) < 0x20 || ch == '\u{7f}'));

        let short = "small prompt";
        assert_eq!(clip_utf8_bytes(short, SEMANTIC_PROMPT_MAX_BYTES), short);
        // Budget that lands inside a multibyte char must back off to a
        // boundary and stay valid UTF-8 (guaranteed by `String`).
        let boundary = clip_utf8_bytes(&"汉".repeat(10), '…'.len_utf8() + 7);
        assert!(boundary.len() <= '…'.len_utf8() + 7);
        assert!((boundary.len() - '…'.len_utf8()).is_multiple_of('汉'.len_utf8()));
    }

    fn memory_with_sections() -> RelayMemory {
        RelayMemory {
            goals: vec![RelayItem {
                id: "g1".to_owned(),
                text: "ship unified relay memory".to_owned(),
            }],
            preferences: vec![RelayItem {
                id: "p1".to_owned(),
                text: "run the cargo gate before reporting green".to_owned(),
            }],
            mistakes: vec![RelayItem {
                id: "m1".to_owned(),
                text: "edited an applied migration once; never again".to_owned(),
            }],
            tasks: vec![RelayTask {
                id: "t1".to_owned(),
                text: "wire work.pickup".to_owned(),
                status: "doing".to_owned(),
                updated_ms: None,
            }],
            current_state: "mid-implementation".to_owned(),
            recent: vec!["pi: started relay memory lane".to_owned()],
            sources: Vec::new(),
            workspace: None,
        }
    }

    /// relay memory merge | Given long-lived sections and a semantic rewrite
    /// that touches nothing else | When ops apply | Then goals/preferences/
    /// mistakes survive and only the requested fields change.
    #[test]
    fn merge_ops_never_wipe_long_lived_sections_on_rewrite() {
        let mut memory = memory_with_sections();
        let ops: HandoffOps = serde_json::from_str(
            r#"{"goals":[{"op":"add","id":"g2","text":"land pickup resolution"}],"tasks":[{"op":"update","id":"t1","status":"blocked"},{"op":"add","text":"write tests"}],"current_state":"compacting","recent":["claude: folded new events"]}"#,
        )
        .unwrap();
        apply_memory_ops(&mut memory, ops, 10);
        assert_eq!(memory.goals.len(), 2);
        assert_eq!(
            memory.preferences[0].text,
            "run the cargo gate before reporting green"
        );
        assert_eq!(
            memory.mistakes[0].text,
            "edited an applied migration once; never again"
        );
        assert_eq!(memory.tasks[0].status, "blocked");
        assert_eq!(memory.tasks[1].id, "t2");
        assert_eq!(memory.tasks[1].status, "todo");
        assert_eq!(memory.current_state, "compacting");
        assert_eq!(memory.recent.len(), 2);

        // An empty/sectionless response (a "rewrite" that mentions nothing)
        // leaves every long-lived section intact.
        let noop: HandoffOps = serde_json::from_str(r#"{"recent":["codex: heartbeat"]}"#).unwrap();
        apply_memory_ops(&mut memory, noop, 11);
        assert_eq!(memory.goals.len(), 2);
        assert_eq!(memory.preferences.len(), 1);
        assert_eq!(memory.mistakes.len(), 1);
        assert_eq!(memory.tasks.len(), 2);
        assert_eq!(memory.recent.len(), 3);

        // Removal requires an explicit id op; unknown ops are ignored.
        let remove: HandoffOps = serde_json::from_str(
            r#"{"goals":[{"op":"remove","id":"g1"}],"mistakes":[{"op":"clear"}]}"#,
        )
        .unwrap();
        apply_memory_ops(&mut memory, remove, 12);
        assert_eq!(memory.goals.len(), 1);
        assert_eq!(memory.goals[0].id, "g2");
        assert_eq!(memory.mistakes.len(), 1);

        // The rendered document keeps the fixed section order and round-trips.
        let rendered = render_handoff(&memory);
        let order = [
            "## 规则",
            "### 公共原则/偏好规范",
            "### 错题集",
            "## 任务清单",
            "## 历史会话路径",
        ]
        .iter()
        .map(|heading| rendered.find(heading).unwrap())
        .collect::<Vec<_>>();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{rendered}");
        let parsed = parsed_memory(&rendered).unwrap();
        assert_eq!(parsed, memory);
    }

    /// relay memory fallback | Given a provider failure | When the rule
    /// fallback runs | Then long-lived sections stay byte-identical and new
    /// events append to the bounded rolling log.
    #[test]
    fn fallback_keeps_sections_and_appends_recent_bounded() {
        let mut memory = memory_with_sections();
        memory.recent = (0..RECENT_MAX_LINES)
            .map(|index| format!("pi: old line {index}"))
            .collect();
        let before = memory.clone();
        let events: Vec<WorkEvent> = (0..5)
            .map(|index| WorkEvent {
                event_id: index as i64 + 1,
                ts: index as i64,
                project_ref: "proj".to_owned(),
                worktree: "/repo".to_owned(),
                provider: "codex".to_owned(),
                account: None,
                kind: "edit".to_owned(),
                summary: format!("change {index}"),
                source_ref: format!("test:{index}"),
            })
            .collect();
        apply_fallback(&mut memory, "", &events);
        assert_eq!(memory.goals, before.goals);
        assert_eq!(memory.preferences, before.preferences);
        assert_eq!(memory.mistakes, before.mistakes);
        assert_eq!(memory.tasks, before.tasks);
        assert_eq!(memory.recent.len(), RECENT_MAX_LINES);
        assert_eq!(memory.recent.last().unwrap(), "codex: [edit] change 4");
        assert!(!memory.recent.iter().any(|line| line == "pi: old line 0"));
        assert!(memory.current_state.contains("codex: [edit] change 4"));
        assert!(memory.current_state.contains("mid-implementation"));
    }

    /// relay memory budget | Given 300 long preferences plus a rolling log and
    /// closed tasks | When the budget is enforced and the document rendered |
    /// Then only droppable things go (recent lines, extra sources, closed
    /// tasks) and every item keeps its complete text — no "…" is appended.
    #[test]
    fn budget_never_truncates_item_text() {
        let mut memory = memory_with_sections();
        for index in 0..300 {
            memory.preferences.push(RelayItem {
                id: format!("p{index}"),
                text: format!(
                    "rule {index}: {}",
                    "keep this whole sentence. ".repeat(10).trim_end()
                ),
            });
        }
        memory.recent = (0..40)
            .map(|index| format!("pi: {}", "x".repeat(60) + &index.to_string()))
            .collect();
        memory.sources = (0..20)
            .map(|index| format!("pi 2026-10-17 08:00 +08:00 /s/{index}.jsonl"))
            .collect();
        memory.tasks = (0..30)
            .map(|index| RelayTask {
                id: format!("t{index}"),
                text: "done work ".to_owned() + &"y".repeat(40),
                status: "done".to_owned(),
                updated_ms: None,
            })
            .collect();
        memory.tasks.push(RelayTask {
            id: "tx".to_owned(),
            text: format!("still open task keeps its text {}", "z".repeat(80)),
            status: "todo".to_owned(),
            updated_ms: None,
        });

        enforce_budget(&mut memory);
        let rendered = render_handoff(&memory);

        // Droppable state is gone and the open task survives whole.
        assert!(memory.recent.is_empty());
        assert!(memory.sources.len() <= 5);
        assert!(memory.tasks.iter().all(|task| task.status != "done"));
        assert_eq!(memory.tasks.len(), 1);
        assert!(rendered.contains(&"z".repeat(80)));
        // Item text is never shortened: the document stays over budget instead
        // of clipping a rule into a fragment.
        assert!(rendered.chars().count() > HANDOFF_MAX_CHARS);
        assert_eq!(memory.preferences.len(), 301);
        for item in &memory.preferences {
            assert!(
                rendered.contains(&format!("[{}] {}", item.id, single_line(&item.text))),
                "item text was altered: {}",
                item.id
            );
        }
        assert_eq!(
            memory.mistakes[0].text,
            "edited an applied migration once; never again"
        );
        assert!(
            rendered
                .lines()
                .filter(|line| line.starts_with("- ["))
                .all(|line| !line.ends_with('…')),
            "an item line was truncated"
        );
        assert!(parsed_memory(&rendered).is_some());
    }

    /// relay memory legacy migration | Given a legacy free-form handoff |
    /// When the semantic path runs | Then the legacy text is offered as
    /// context and the structured memory block is established.
    #[test]
    fn legacy_free_form_handoff_migrates_into_structured_memory() {
        let legacy = "# Free form\nOld notes: migrating parser, blocked on review";
        let (memory, context) = split_memory(&format!("{legacy}\n"));
        assert!(memory.goals.is_empty());
        assert_eq!(context, legacy);

        // Round-trip: a rendered document splits back into memory, no legacy.
        let rendered = render_handoff(&memory_with_sections());
        assert!(rendered.contains(MEMORY_OPEN));
        let (round_trip, context) = split_memory(&rendered);
        assert_eq!(round_trip, memory_with_sections());
        assert_eq!(context, "");
        // An open marker inside item text cannot shadow the real block.
        let mut hostile = memory_with_sections();
        hostile.goals[0].text = "notes <!-- relay-memory: {\"goals\":[]}".to_owned();
        let rendered = render_handoff(&hostile);
        assert_eq!(split_memory(&rendered).0, hostile);

        // Fallback migration: legacy lands in 当前状态.
        let mut memory = RelayMemory::default();
        apply_fallback(
            &mut memory,
            legacy,
            &[WorkEvent {
                event_id: 1,
                ts: 1,
                project_ref: "proj".to_owned(),
                worktree: "/repo".to_owned(),
                provider: "pi".to_owned(),
                account: None,
                kind: "edit".to_owned(),
                summary: "touched parser".to_owned(),
                source_ref: "test:1".to_owned(),
            }],
        );
        assert!(memory.current_state.contains("Old notes"));
        assert_eq!(memory.recent, vec!["pi: [edit] touched parser"]);

        // Semantic migration: the prompt carries the legacy context.
        let root = temp_dir("legacy");
        let store = StateStore::open(":memory:").unwrap();
        store
            .put_work_state(&WorkState {
                project_ref: "proj".to_owned(),
                worktree: root.to_string_lossy().into_owned(),
                handoff_md: legacy.to_owned(),
                ..WorkState::default()
            })
            .unwrap();
        append(&store, &root, "new event", 2);
        let summarizer = RecordingSummarizer {
            prompts: std::sync::Mutex::new(Vec::new()),
        };
        compact_tick(&store, &summarizer, &|_| Ok(0), 10).unwrap();
        let prompts = summarizer.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("Old notes"), "{}", prompts[0]);
        drop(prompts);
        let handoff = store
            .work_state("proj", root.to_str().unwrap())
            .unwrap()
            .unwrap()
            .handoff_md;
        let migrated = parsed_memory(&handoff).unwrap();
        assert_eq!(migrated.current_state, "compacted");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn concurrent_agent_edit_between_read_and_write_is_folded_not_lost() {
        let root = temp_dir("guard");
        fs::create_dir(root.join(".git")).unwrap();
        let target = root.join("HANDOFF.md");
        let initial = render_handoff(&RelayMemory::default());
        fs::write(&target, &initial).unwrap();
        let edited = initial.replace("### 错题集\n", "### 错题集\n\n- never force-push main\n");
        let renders = AtomicUsize::new(0);
        // Simulate an agent editing HANDOFF.md right after the first read.
        let result = write_handoff_guarded(&root, |file_md| {
            if renders.fetch_add(1, Ordering::SeqCst) == 0 {
                fs::write(&target, &edited).unwrap();
            }
            let mut memory = RelayMemory::default();
            if let Some(file_md) = file_md {
                fold_file_edits(&mut memory, file_md, 10);
            }
            memory.tasks.push(RelayTask {
                id: "t9".to_owned(),
                text: "compactor task".to_owned(),
                status: "todo".to_owned(),
                updated_ms: None,
            });
            render_handoff(&memory)
        });
        assert!(matches!(result, GuardedWrite::Written(_)));
        assert_eq!(renders.load(Ordering::SeqCst), 2);
        let on_disk = fs::read_to_string(&target).unwrap();
        assert!(on_disk.contains("never force-push main"), "{on_disk}");
        assert!(on_disk.contains("compactor task"));

        // An agent that never stops editing: bounded retries, file untouched.
        let result = write_handoff_guarded(&root, |_| {
            fs::write(
                &target,
                format!("agent {}", renders.fetch_add(1, Ordering::SeqCst)),
            )
            .unwrap();
            "compactor".to_owned()
        });
        assert!(matches!(result, GuardedWrite::Conflict));
        assert!(fs::read_to_string(&target).unwrap().starts_with("agent "));
        assert!(!fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    struct EditingSummarizer(PathBuf);

    impl WorkSummarizer for EditingSummarizer {
        fn summarize(&self, _prompt: &str) -> Result<String, String> {
            // An agent edits HANDOFF.md while the (slow) summarizer runs.
            let target = self.0.join("HANDOFF.md");
            let md = fs::read_to_string(&target).unwrap();
            fs::write(
                &target,
                md.replace("### 错题集\n", "### 错题集\n\n- agent added mistake\n"),
            )
            .unwrap();
            Ok(r#"{"tasks":[{"op":"add","text":"llm task"}]}"#.to_owned())
        }
    }

    #[test]
    fn compaction_keeps_agent_edit_made_during_summarize() {
        let root = temp_dir("guard-scope");
        fs::create_dir(root.join(".git")).unwrap();
        let store = StateStore::open(":memory:").unwrap();
        append(&store, &root, "first", 1);
        compact_tick(&store, &FakeSummarizer(Err("down")), &|_| Ok(0), 2).unwrap();
        append(&store, &root, "second", 3);
        compact_tick(&store, &EditingSummarizer(root.clone()), &|_| Ok(0), 4).unwrap();
        let on_disk = fs::read_to_string(root.join("HANDOFF.md")).unwrap();
        assert!(on_disk.contains("agent added mistake"), "{on_disk}");
        assert!(on_disk.contains("llm task"));
        let state = store
            .work_state("proj", root.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(state.handoff_md, on_disk);
        fs::remove_dir_all(root).unwrap();
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(status.status.success(), "{args:?}: {status:?}");
    }

    #[test]
    fn git_context_lists_branch_files_and_next_step_and_skips_non_git() {
        let plain = temp_dir("nogit");
        assert_eq!(git_context(&plain), None);
        fs::remove_dir_all(plain).unwrap();

        let root = temp_dir("gitctx");
        git(&root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("base.txt"), "b").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "base"]);
        git(&root, &["checkout", "-qb", "feat/x"]);
        for i in 0..10 {
            fs::write(root.join(format!("c{i}.rs")), "c").unwrap();
        }
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "feat"]);
        let clean = git_context(&root).unwrap();
        assert_eq!(clean.branch, "feat/x");
        assert_eq!(clean.changed_total, 10);
        assert_eq!(clean.changed_files.len(), CHANGED_FILES_MAX);
        assert!(
            clean.next_step.starts_with("push feat/x"),
            "{}",
            clean.next_step
        );
        fs::write(root.join("base.txt"), "changed").unwrap();
        fs::write(root.join("new.md"), "n").unwrap();
        let dirty = git_context(&root).unwrap();
        assert_eq!(dirty.changed_total, 12);
        assert!(
            dirty
                .changed_files
                .starts_with(&["base.txt".to_owned(), "new.md".to_owned()])
        );
        assert!(
            dirty.next_step.contains("2 uncommitted"),
            "{}",
            dirty.next_step
        );

        let memory = RelayMemory {
            tasks: vec![
                RelayTask {
                    id: "t1".into(),
                    text: "open".into(),
                    status: "doing".into(),
                    updated_ms: None,
                },
                RelayTask {
                    id: "t2".into(),
                    text: "old".into(),
                    status: "done".into(),
                    updated_ms: None,
                },
            ],
            workspace: Some(dirty),
            ..RelayMemory::default()
        };
        let rendered = render_handoff(&memory);
        // Only the open task is rendered, so only it carries the context line.
        assert!(!rendered.contains("[t2]"), "{rendered}");
        assert_eq!(
            rendered.matches("↳ branch `feat/x`").count(),
            1,
            "{rendered}"
        );
        assert!(rendered.contains("files(12): base.txt, new.md"));
        assert!(rendered.contains(RULES_EXECUTABLE_NOTE));
        // The context line is not folded back as a task.
        let mut folded = memory.clone();
        assert!(fold_file_edits(&mut folded, &rendered, 20));
        assert_eq!(folded.tasks.len(), 1);
        assert_eq!(folded.tasks[0].text, "open");
        fs::remove_dir_all(root).unwrap();
    }
}
