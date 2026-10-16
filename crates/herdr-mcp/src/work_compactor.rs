use crate::semantic::{SemanticChatMessage, SemanticChatOptions, SemanticService};
use crate::state_store::StateStore;
use crate::state_store::work_journal::{WorkEvent, WorkState, clip_chars};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HANDOFF_MAX_CHARS: usize = 16_000;
const FALLBACK_EVENT_LIMIT: usize = 20;
const EVENT_PAGE_SIZE: usize = 500;
const COMPACTION_INTERVAL: Duration = Duration::from_secs(10 * 60);
const SUMMARY_MAX_TOKENS: u32 = 4_000;

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

#[derive(Deserialize)]
struct HandoffResponse {
    handoff_md: String,
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
    let previous = store
        .work_state(project_ref, worktree)?
        .unwrap_or_else(|| WorkState {
            project_ref: project_ref.to_owned(),
            worktree: worktree.to_owned(),
            log_cursors_json: "{}".to_owned(),
            ..WorkState::default()
        });
    let events = all_new_events(store, project_ref, worktree, previous.through_event_id)?;
    let Some(through_event_id) = events.last().map(|event| event.event_id) else {
        return Ok(());
    };

    let prompt = compaction_prompt(&previous.handoff_md, &events);
    let semantic_handoff = summarizer
        .summarize(&prompt)
        .ok()
        .and_then(|content| serde_json::from_str::<HandoffResponse>(&content).ok())
        .map(|response| response.handoff_md)
        .filter(|handoff| !handoff.trim().is_empty());
    let handoff_md = match semantic_handoff {
        Some(handoff) => clip_chars(handoff.trim(), HANDOFF_MAX_CHARS),
        None => fallback_handoff(
            &previous.handoff_md,
            &store.latest_work_events(project_ref, worktree, FALLBACK_EVENT_LIMIT)?,
        ),
    };

    store.put_work_state(&WorkState {
        project_ref: project_ref.to_owned(),
        worktree: worktree.to_owned(),
        handoff_md: handoff_md.clone(),
        through_event_id,
        log_cursors_json: previous.log_cursors_json,
        updated_at: now_ms,
    })?;
    write_handoff(Path::new(worktree), &handoff_md)
}

fn all_new_events(
    store: &StateStore,
    project_ref: &str,
    worktree: &str,
    after: i64,
) -> Result<Vec<WorkEvent>, String> {
    let mut result = Vec::new();
    let mut cursor = after;
    loop {
        let page = store.work_events_after(project_ref, worktree, cursor, EVENT_PAGE_SIZE)?;
        let Some(last) = page.last() else {
            break;
        };
        cursor = last.event_id;
        let done = page.len() < EVENT_PAGE_SIZE;
        result.extend(page);
        if done {
            break;
        }
    }
    Ok(result)
}

fn compaction_prompt(previous: &str, events: &[WorkEvent]) -> String {
    let mut prompt = String::from(
        "Rewrite a concise engineering handoff from the previous handoff and new work events. Preserve decisions, current state, blockers, and next actions. Do not invent facts. Return only JSON shaped {\"handoff_md\":\"...\"}. Keep handoff_md under 16000 characters.\n\nPrevious handoff:\n",
    );
    prompt.push_str(&clip_chars(previous, HANDOFF_MAX_CHARS));
    prompt.push_str("\n\nNew events:\n");
    for event in events.iter().rev().take(100).rev() {
        prompt.push_str(&format_event(event));
    }
    clip_chars(&prompt, 60_000)
}

fn fallback_handoff(previous: &str, events: &[WorkEvent]) -> String {
    let mut handoff = clip_chars(previous.trim(), HANDOFF_MAX_CHARS / 2);
    if !handoff.is_empty() {
        handoff.push_str("\n\n");
    }
    handoff.push_str("## Recent work events\n");
    for event in events.iter().rev().take(FALLBACK_EVENT_LIMIT).rev() {
        handoff.push_str(&format_event(event));
    }
    clip_chars(&handoff, HANDOFF_MAX_CHARS)
}

fn format_event(event: &WorkEvent) -> String {
    format!(
        "- [{}] {} ({}, {})\n",
        event.kind, event.summary, event.provider, event.source_ref
    )
}

fn write_handoff(worktree: &Path, handoff: &str) -> Result<(), String> {
    if !worktree.is_dir() {
        return Ok(());
    }
    fs::write(worktree.join("HANDOFF.md"), handoff)
        .map_err(|error| format!("cannot write HANDOFF.md: {error}"))?;
    let Some(git_dir) = resolve_git_dir(worktree)? else {
        return Ok(());
    };
    let info = git_dir.join("info");
    fs::create_dir_all(&info)
        .map_err(|error| format!("cannot create Git info directory: {error}"))?;
    let exclude = info.join("exclude");
    let existing = match fs::read_to_string(&exclude) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("cannot read Git exclude file: {error}")),
    };
    if existing.lines().any(|line| line.trim() == "HANDOFF.md") {
        return Ok(());
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str("HANDOFF.md\n");
    fs::write(&exclude, updated).map_err(|error| format!("cannot write Git exclude file: {error}"))
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

pub(crate) fn start_background(store: &StateStore) {
    let Some(path) = store.path().map(Path::to_path_buf) else {
        return;
    };
    let _ = thread::Builder::new()
        .name("herdr-mcp-work-compactor".to_owned())
        .spawn(move || {
            loop {
                let _ = std::panic::catch_unwind(|| {
                    let store = StateStore::open(&path)?;
                    compact_tick(
                        &store,
                        &SemanticService::from_config(),
                        &|_| Ok(0),
                        now_ms(),
                    )
                    .map(|_| ())
                });
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
                handoff_md: "old".to_owned(),
                through_event_id: 0,
                log_cursors_json: r#"{"pi":7}"#.to_owned(),
                updated_at: 1,
            })
            .unwrap();
        append(&store, &root, "changed parser", 2);
        let calls = AtomicUsize::new(0);
        compact_tick(
            &store,
            &FakeSummarizer(Ok(r##"{"handoff_md":"# New handoff"}"##)),
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
        assert_eq!(state.handoff_md, "# New handoff");
        assert_eq!(state.log_cursors_json, r#"{"pi":7}"#);
        assert_eq!(
            fs::read_to_string(root.join("HANDOFF.md")).unwrap(),
            "# New handoff"
        );
        assert_eq!(
            fs::read_to_string(root.join(".git/info/exclude")).unwrap(),
            "HANDOFF.md\n"
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
            assert!(handoff.contains("# Previous"));
            assert!(handoff.contains("observable change"));
            assert!(handoff.chars().count() <= HANDOFF_MAX_CHARS);
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
            "HANDOFF.md\n"
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(git_dir).unwrap();
    }
}
