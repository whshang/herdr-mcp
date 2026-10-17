use crate::semantic::{SemanticChatMessage, SemanticChatOptions, SemanticService};
use crate::state_cache::EventCache;
use crate::state_store::StateStore;
use crate::state_store::work_journal::{WorkEvent, WorkState, clip_chars};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HANDOFF_MAX_CHARS: usize = 16_000;
const FALLBACK_EVENT_LIMIT: usize = 20;
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

        let prompt = compaction_prompt(&previous.handoff_md, &events);
        let semantic_handoff = summarizer
            .summarize(&prompt)
            .ok()
            .and_then(|content| serde_json::from_str::<HandoffResponse>(&content).ok())
            .map(|response| response.handoff_md)
            .filter(|handoff| !handoff.trim().is_empty());
        let handoff_md = match semantic_handoff {
            Some(handoff) => clip_chars(handoff.trim(), HANDOFF_MAX_CHARS),
            None => fallback_handoff(&previous.handoff_md, &events),
        };

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

fn compaction_prompt(previous: &str, events: &[WorkEvent]) -> String {
    let mut prompt = String::from(
        "Rewrite a concise engineering handoff from the previous handoff and new work events. Preserve decisions, current state, blockers, and next actions. Do not invent facts. Return only JSON shaped {\"handoff_md\":\"...\"}. Keep handoff_md under 16000 characters.\n\nPrevious handoff:\n",
    );
    prompt.push_str(&clip_chars(previous, HANDOFF_MAX_CHARS));
    prompt.push_str("\n\nNew events:\n");
    for event in events {
        prompt.push_str(&format_event(event));
    }
    // The semantic message cap is measured in bytes, not chars; truncate on a
    // UTF-8 char boundary against the byte budget.
    clip_utf8_bytes(&prompt, SEMANTIC_PROMPT_MAX_BYTES)
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
    write_regular_file(&worktree.join("HANDOFF.md"), handoff, "HANDOFF.md")?;
    let Some(git_common_dir) = resolve_git_common_dir(worktree)? else {
        return Ok(());
    };
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
    if existing.lines().any(|line| line.trim() == "HANDOFF.md") {
        return Ok(());
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str("HANDOFF.md\n");
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
                    let roots = crate::fs_security::managed_roots(&snapshot);
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
            Ok(r##"{"handoff_md":"# compacted"}"##.to_owned())
        }
    }

    fn prompt_event_lines(prompt: &str) -> usize {
        prompt
            .lines()
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
        let prompt = compaction_prompt(&cjk_previous, &events);
        assert!(prompt.len() <= SEMANTIC_PROMPT_MAX_BYTES);
        assert!(prompt.ends_with('…'));

        let short = "small prompt";
        assert_eq!(clip_utf8_bytes(short, SEMANTIC_PROMPT_MAX_BYTES), short);
        // Budget that lands inside a multibyte char must back off to a
        // boundary and stay valid UTF-8 (guaranteed by `String`).
        let boundary = clip_utf8_bytes(&"汉".repeat(10), '…'.len_utf8() + 7);
        assert!(boundary.len() <= '…'.len_utf8() + 7);
        assert!((boundary.len() - '…'.len_utf8()).is_multiple_of('汉'.len_utf8()));
    }
}
