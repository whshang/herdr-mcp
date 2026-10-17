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
/// Item texts are only ever clipped, never dropped; this is the smallest
/// clipping floor before budget enforcement gives up growing the document.
const ITEM_TEXT_FLOOR: usize = 40;
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

impl WorkSummarizer for SemanticService {
    fn summarize(&self, prompt: &str) -> Result<String, String> {
        self.chat_background(
            &[SemanticChatMessage {
                role: "user".to_owned(),
                content: prompt.to_owned(),
            }],
            SemanticChatOptions {
                max_tokens: Some(SUMMARY_MAX_TOKENS),
                json_response: true,
                ..SemanticChatOptions::default()
            },
        )
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
    /// ## 最近对话摘要 (each line prefixed with the provider, newest last)
    #[serde(default)]
    pub recent: Vec<String>,
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
}

fn task_status_todo() -> String {
    "todo".to_owned()
}

const TASK_STATUSES: [&str; 4] = ["todo", "doing", "done", "blocked"];

/// Incremental ops the summarizer returns. Only these apply; the previous
/// memory is the merge base, so an LLM rewrite cannot wipe sections.
#[derive(Debug, Deserialize, Default)]
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

#[derive(Debug, Deserialize)]
struct ItemOp {
    op: String,
    id: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
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

/// Whitespace-collapse a value onto one line for list rendering.
fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Render the fixed-section handoff document plus the trailing structured
/// block. Section order is contractual (entry-agnostic readers rely on it).
pub(crate) fn render_handoff(memory: &RelayMemory) -> String {
    // Order: durable principles first, then lessons, newest tasks, state,
    // goals, and the older rolling history last; lists newest-first.
    let mut out = String::from("# HANDOFF\n");
    render_items(&mut out, "公共原则/偏好与规范", &memory.preferences);
    render_items(&mut out, "错题集", &memory.mistakes);
    out.push_str("\n## 最新任务（剩余任务）\n");
    for task in memory.tasks.iter().rev() {
        out.push_str(&format!(
            "\n- [{}] ({}) {}",
            task.id,
            task.status,
            single_line(&task.text)
        ));
    }
    out.push_str("\n\n## 当前状态\n\n");
    out.push_str(memory.current_state.trim());
    out.push('\n');
    render_items(&mut out, "目标与计划", &memory.goals);
    out.push_str("\n\n## 最近对话摘要\n");
    for line in memory.recent.iter().rev() {
        out.push_str(&format!("\n- {}", single_line(line)));
    }
    // `<!--` inside item text must never terminate the block early or shadow
    // the outer open marker; \u0021 decodes back to `!` in valid JSON.
    let json = serde_json::to_string(memory)
        .unwrap_or_else(|_| "{}".to_owned())
        .replace("<!--", "<\\u0021--");
    out.push_str(&format!("\n\n{MEMORY_OPEN}{json}{MEMORY_CLOSE}\n"));
    out
}

fn render_items(out: &mut String, heading: &str, items: &[RelayItem]) {
    out.push_str(&format!("\n## {heading}\n"));
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
                match op
                    .id
                    .map(|id| id.trim().to_owned())
                    .filter(|id| !id.is_empty())
                {
                    Some(id) => {
                        if let Some(existing) = items.iter_mut().find(|item| item.id == id) {
                            existing.text = text;
                        } else {
                            items.push(RelayItem { id, text });
                        }
                    }
                    None => {
                        let id = next_id(items, prefix);
                        items.push(RelayItem { id, text });
                    }
                }
            }
            _ => {}
        }
    }
}

fn apply_task_ops(tasks: &mut Vec<RelayTask>, ops: Vec<TaskOp>) {
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
                match id {
                    Some(id) if tasks.iter().any(|task| task.id == id) => {
                        let existing = tasks
                            .iter_mut()
                            .find(|task| task.id == id)
                            .expect("checked");
                        if let Some(text) = text {
                            existing.text = text;
                        }
                        if let Some(status) = status {
                            existing.status = status;
                        }
                    }
                    _ => {
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
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

/// Merge a valid semantic response into the memory. Sections the response
/// does not mention keep their previous items by construction.
fn apply_memory_ops(memory: &mut RelayMemory, ops: HandoffOps) {
    apply_item_ops(&mut memory.goals, ops.goals, "g");
    apply_item_ops(&mut memory.preferences, ops.preferences, "p");
    apply_item_ops(&mut memory.mistakes, ops.mistakes, "m");
    apply_task_ops(&mut memory.tasks, ops.tasks);
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

/// Enforce the rendered budget: trim the rolling log first, then the oldest
/// done tasks. Goals/preferences/mistakes are never dropped silently — their
/// (and open tasks') text is clipped when everything else is exhausted.
fn enforce_budget(memory: &mut RelayMemory) {
    for _ in 0..200 {
        if render_handoff(memory).chars().count() <= HANDOFF_MAX_CHARS {
            return;
        }
        if !memory.recent.is_empty() {
            memory.recent.remove(0);
            continue;
        }
        if let Some(index) = memory.tasks.iter().position(|task| task.status == "done") {
            memory.tasks.remove(index);
            continue;
        }
        if !clip_longest_text(memory) {
            return;
        }
    }
}

/// Halve the longest long-lived item text (floor-bounded) or the current
/// state. Returns false when nothing over the floor remains to clip.
fn clip_longest_text(memory: &mut RelayMemory) -> bool {
    let mut best: Option<(u8, usize, usize)> = None;
    let consider = |best: &mut Option<(u8, usize, usize)>, which, index, len: usize| {
        if len > ITEM_TEXT_FLOOR && best.is_none_or(|(_, _, current)| len > current) {
            *best = Some((which, index, len));
        }
    };
    for (which, items) in [
        (0_u8, &memory.goals),
        (1, &memory.preferences),
        (2, &memory.mistakes),
    ] {
        for (index, item) in items.iter().enumerate() {
            consider(&mut best, which, index, item.text.chars().count());
        }
    }
    for (index, task) in memory.tasks.iter().enumerate() {
        consider(&mut best, 3, index, task.text.chars().count());
    }
    let state_len = memory.current_state.chars().count();
    if state_len > 80 && best.is_none_or(|(_, _, current)| state_len > current) {
        best = Some((4, 0, state_len));
    }
    let Some((which, index, len)) = best else {
        return false;
    };
    let target = ITEM_TEXT_FLOOR.max(len / 2);
    let text = match which {
        0 => &mut memory.goals[index].text,
        1 => &mut memory.preferences[index].text,
        2 => &mut memory.mistakes[index].text,
        3 => &mut memory.tasks[index].text,
        _ => &mut memory.current_state,
    };
    *text = clip_chars(text, target);
    true
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

    for (project_ref, worktree) in scopes {
        if let Err(error) = compact_scope(store, summarizer, &project_ref, &worktree, now_ms) {
            first_error.get_or_insert(error);
        } else {
            compacted += 1;
        }
    }

    // The file export can fail after the cursor advanced (or the worktree was
    // reset afterwards); re-derive HANDOFF.md whenever it drifts from state.
    if let Err(error) = reconcile_handoff_files(store) {
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
        let (mut memory, legacy) = split_memory(&previous.handoff_md);
        let prompt = compaction_prompt(&memory, &legacy, &events);
        let semantic_ops = summarizer
            .summarize(&prompt)
            .ok()
            .and_then(|content| serde_json::from_str::<HandoffOps>(&content).ok())
            .filter(|ops| !ops.is_empty());
        match semantic_ops {
            Some(ops) => {
                apply_memory_ops(&mut memory, ops);
                // Never lose a pre-upgrade free-form handoff: keep it in
                // 当前状态 unless the model already restated state.
                if !legacy.trim().is_empty() && memory.current_state.trim().is_empty() {
                    memory.current_state = clip_chars(legacy.trim(), CURRENT_STATE_MAX_CHARS);
                }
            }
            None => apply_fallback(&mut memory, &legacy, &events),
        }
        enforce_budget(&mut memory);
        let handoff_md = render_handoff(&memory);

        store.put_work_state(&WorkState {
            project_ref: project_ref.to_owned(),
            worktree: worktree.to_owned(),
            handoff_md: handoff_md.clone(),
            through_event_id,
            log_cursors_json: previous.log_cursors_json.clone(),
            updated_at: now_ms,
        })?;
        write_handoff(Path::new(worktree), &handoff_md)?;

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

fn compaction_prompt(memory: &RelayMemory, legacy: &str, events: &[WorkEvent]) -> String {
    let memory_json = serde_json::to_string(memory).unwrap_or_else(|_| "{}".to_owned());
    let mut prompt = String::from(
        "You maintain one project relay memory shared across coding agents. Given the CURRENT structured memory (JSON), optional LEGACY context from an older free-form handoff, and NEW work events, return ONLY JSON of this exact shape:
{\"goals\":[{\"op\":\"add|update|remove\",\"id\":\"g1\",\"text\":\"...\"}],\"preferences\":[{\"op\":\"add|update|remove\",\"id\":\"p1\",\"text\":\"...\"}],\"mistakes\":[{\"op\":\"add|update|remove\",\"id\":\"m1\",\"text\":\"what went wrong + correct approach\"}],\"tasks\":[{\"op\":\"add|update|remove\",\"id\":\"t1\",\"text\":\"...\",\"status\":\"todo|doing|done|blocked\"}],\"current_state\":\"short current state\",\"recent\":[\"provider: one-line summary\"]}
Sections: 目标与计划=goals, 偏好与规范=preferences, 错题集=mistakes, 剩余任务=tasks. Rules: apply ONLY incremental ops; prefer update over remove and never remove a goal/preference/mistake unless the events clearly supersede it; reuse existing ids, new ids use the section prefix with an unused number; do not invent facts; omit sections with no change; \"recent\" lists ONLY new lines to append, each prefixed with its provider. In this input, line breaks are written as the two characters \\n; use plain single-line text inside list item fields.

Current memory JSON:
",
    );
    prompt.push_str(&clip_chars(&memory_json, HANDOFF_MAX_CHARS));
    prompt.push_str("\n\nLegacy context (may be empty):\n");
    prompt.push_str(&clip_chars(legacy, HANDOFF_MAX_CHARS / 2));
    prompt.push_str("\n\nNew events:\n");
    for event in events {
        prompt.push_str(&format_event(event));
    }
    // The Edge /semantic/chat contract rejects message content containing any
    // control character (U+0000-U+001F, U+007F) with semantic_request_invalid,
    // so encode line breaks/tabs as visible escapes before the byte clip.
    // The semantic message cap is measured in bytes, not chars; truncate on a
    // UTF-8 char boundary against the byte budget.
    clip_utf8_bytes(&escape_control_chars(&prompt), SEMANTIC_PROMPT_MAX_BYTES)
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
fn reconcile_handoff_files(store: &StateStore) -> Result<(), String> {
    let mut first_error: Option<String> = None;
    for state in store.work_states()? {
        let worktree = Path::new(&state.worktree);
        if !worktree.is_dir() {
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
        let needs_write = match current {
            Some(content) => content != state.handoff_md,
            // Do not materialize empty placeholder files in worktrees that
            // never produced a handoff.
            None => !state.handoff_md.trim().is_empty(),
        };
        if needs_write && let Err(error) = write_handoff(worktree, &state.handoff_md) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn write_handoff(worktree: &Path, handoff: &str) -> Result<(), String> {
    if !worktree.is_dir() {
        return Ok(());
    }
    // Never drop a file into hidden/config folders (e.g. ~/.ssh) that became
    // an operational root; git checkouts are exempt.
    let hidden = worktree.components().any(|part| {
        part.as_os_str()
            .to_str()
            .is_some_and(|name| name.starts_with('.') && name.len() > 1)
    });
    if hidden && resolve_git_common_dir(worktree)?.is_none() {
        return Ok(());
    }
    if is_home_dir(worktree) {
        return Ok(());
    }
    write_regular_file(&worktree.join("HANDOFF.md"), handoff, "HANDOFF.md")?;
    let mut created = vec!["HANDOFF.md"];
    for name in ["AGENTS.md", "CLAUDE.md"] {
        if ensure_handoff_block(&worktree.join(name))? {
            created.push(name);
        }
    }
    let Some(git_common_dir) = resolve_git_common_dir(worktree)? else {
        return Ok(());
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
    add_git_excludes(&git_common_dir, &created)
}

fn is_home_dir(path: &Path) -> bool {
    std::env::var_os("HOME").is_some_and(|home| {
        let home = PathBuf::from(home);
        fs::canonicalize(&home).unwrap_or(home)
            == fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    })
}

pub(crate) const HANDOFF_BLOCK_START: &str = "<!-- herdr:handoff:start -->";
pub(crate) const HANDOFF_BLOCK_END: &str = "<!-- herdr:handoff:end -->";
const AGENT_DOC_MAX_BYTES: u64 = 256 * 1024;

fn handoff_block() -> String {
    format!(
        "{HANDOFF_BLOCK_START}\n## Handoff (auto-maintained by herdr-mcp)\n\nBefore starting work in this folder, read `HANDOFF.md` first: it is the shared relay memory (preferences, mistakes to avoid, latest tasks, current state, goals, recent history) across all agents. It is regenerated automatically from session logs; do not edit it by hand. Only this marked block is managed by herdr-mcp.\n{HANDOFF_BLOCK_END}\n"
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
        write_handoff(&dir, "h1").unwrap();
        write_handoff(&dir, "h2").unwrap();
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
        write_handoff(&dir, "h3").unwrap();
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
        memory.goals.push(RelayItem {
            id: "g1".into(),
            text: "goal".into(),
        });
        memory.recent = vec!["pi: first".into(), "omp: second".into()];
        let md = render_handoff(&memory);
        let pos = |needle: &str| md.find(needle).unwrap();
        assert!(pos("公共原则/偏好与规范") < pos("错题集"));
        assert!(pos("错题集") < pos("最新任务"));
        assert!(pos("最新任务") < pos("当前状态"));
        assert!(pos("当前状态") < pos("目标与计划"));
        assert!(pos("目标与计划") < pos("最近对话摘要"));
        assert!(pos("new pref") < pos("old pref"));
        assert!(pos("omp: second") < pos("pi: first"));
    }

    #[test]
    fn hidden_non_git_root_gets_no_handoff_file() {
        let base = std::env::temp_dir().join(format!("hrm-{}", std::process::id()));
        let hidden = base.join(".secretdir");
        fs::create_dir_all(&hidden).unwrap();
        write_handoff(&hidden, "x").unwrap();
        assert!(!hidden.join("HANDOFF.md").exists());
        let _ = fs::remove_dir_all(&base);
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
        assert!(state.handoff_md.contains("## 目标与计划"));
        assert!(state.handoff_md.contains("- [g1] ship relay memory"));
        assert!(state.handoff_md.contains("parser rework underway"));
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
            "HANDOFF.md\nAGENTS.md\nCLAUDE.md\n"
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
        write_handoff(&root, "handoff one").unwrap();
        write_handoff(&root, "handoff two").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("HANDOFF.md")).unwrap(),
            "handoff two"
        );
        assert_eq!(
            fs::read_to_string(git_dir.join("info/exclude")).unwrap(),
            "HANDOFF.md\nAGENTS.md\nCLAUDE.md\n"
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
        let error = write_handoff(&root, "attacker content").unwrap_err();
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
        let error = write_handoff(&root, "handoff").unwrap_err();
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

        write_handoff(&linked, "# linked handoff").unwrap();

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
            }],
            current_state: "mid-implementation".to_owned(),
            recent: vec!["pi: started relay memory lane".to_owned()],
        }
    }

    /// relay memory merge | Given long-lived sections and a semantic rewrite
    /// that touches nothing else | When ops apply | Then goals/preferences/
    /// mistakes survive and only the requested fields change.
    #[test]
    fn merge_ops_never_wipe_long_lived_sections_on_rewrite() {
        let mut memory = memory_with_sections();
        let ops: HandoffOps = serde_json::from_str(
            r#"{"goals":[{"op":"add","id":"g2","text":"land pickup resolution"}],"tasks":[{"op":"update","id":"t1","status":"done"},{"op":"add","text":"write tests"}],"current_state":"compacting","recent":["claude: folded new events"]}"#,
        )
        .unwrap();
        apply_memory_ops(&mut memory, ops);
        assert_eq!(memory.goals.len(), 2);
        assert_eq!(
            memory.preferences[0].text,
            "run the cargo gate before reporting green"
        );
        assert_eq!(
            memory.mistakes[0].text,
            "edited an applied migration once; never again"
        );
        assert_eq!(memory.tasks[0].status, "done");
        assert_eq!(memory.tasks[1].id, "t2");
        assert_eq!(memory.tasks[1].status, "todo");
        assert_eq!(memory.current_state, "compacting");
        assert_eq!(memory.recent.len(), 2);

        // An empty/sectionless response (a "rewrite" that mentions nothing)
        // leaves every long-lived section intact.
        let noop: HandoffOps = serde_json::from_str(r#"{"recent":["codex: heartbeat"]}"#).unwrap();
        apply_memory_ops(&mut memory, noop);
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
        apply_memory_ops(&mut memory, remove);
        assert_eq!(memory.goals.len(), 1);
        assert_eq!(memory.goals[0].id, "g2");
        assert_eq!(memory.mistakes.len(), 1);

        // The rendered document keeps the fixed section order and round-trips.
        let rendered = render_handoff(&memory);
        let order = [
            "## 公共原则/偏好与规范",
            "## 错题集",
            "## 最新任务",
            "## 当前状态",
            "## 目标与计划",
            "## 最近对话摘要",
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

    /// relay memory budget | Given an over-budget memory | When enforce_budget
    /// runs | Then recent lines go first, done tasks second, goals/preferences/
    /// mistakes are clipped but never dropped.
    #[test]
    fn budget_trims_recent_then_done_tasks_and_never_drops_long_lived_items() {
        let mut memory = memory_with_sections();
        memory.recent = (0..40)
            .map(|index| format!("pi: {}", "x".repeat(60) + &index.to_string()))
            .collect();
        memory.tasks = (0..30)
            .map(|index| RelayTask {
                id: format!("t{index}"),
                text: "done work ".to_owned() + &"y".repeat(40),
                status: "done".to_owned(),
            })
            .collect();
        memory.tasks.push(RelayTask {
            id: "tx".to_owned(),
            text: format!("still open task keeps its text {}", "z".repeat(80)),
            status: "todo".to_owned(),
        });
        memory.goals[0].text = "goal ".to_owned() + &"长".repeat(9_000);
        let base_len = render_handoff(&memory).chars().count();
        assert!(base_len > HANDOFF_MAX_CHARS * 2);

        enforce_budget(&mut memory);
        let rendered = render_handoff(&memory);
        assert!(
            rendered.chars().count() <= HANDOFF_MAX_CHARS,
            "rendered {} chars",
            rendered.chars().count()
        );
        // Recent trimmed first, then the done tasks; open task remains.
        assert!(memory.recent.len() < RECENT_MAX_LINES);
        assert!(memory.tasks.iter().all(|task| task.status != "done"));
        assert_eq!(memory.tasks.len(), 1);
        // Long-lived sections survive with clipped (not dropped) text.
        assert_eq!(memory.goals.len(), 1);
        assert!(!memory.goals[0].text.is_empty());
        assert_eq!(
            memory.preferences[0].text,
            "run the cargo gate before reporting green"
        );
        assert_eq!(memory.mistakes.len(), 1);
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
}
