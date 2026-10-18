//! Passive work journal (relay/handoff) storage: two tables.
//!
//! Forward-additive *auxiliary* schema (not a numbered migration), like the
//! browser archive/dispatch tables: it only adds tables, so a pinned
//! schema-15 binary (PROD rollback target) can still open the same DB.

// The tool-call capture consumer lives in `crate::work_capture`; tailers and
// the compactor land in follow-up lanes, so unused items stay allow-listed.
#![allow(dead_code)]

use super::StateStore;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub(crate) const WORK_SUMMARY_MAX_CHARS: usize = 300;
pub(crate) const WORK_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

const WORK_JOURNAL_AUXILIARY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS work_events (
    event_id    INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          INTEGER NOT NULL,
    project_ref TEXT NOT NULL,
    worktree    TEXT NOT NULL,
    provider    TEXT NOT NULL,
    account     TEXT,
    kind        TEXT NOT NULL,
    summary     TEXT NOT NULL,
    source_ref  TEXT NOT NULL,
    dedupe_key  TEXT,
    CHECK (length(summary) <= 300)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_work_events_dedupe
    ON work_events(dedupe_key) WHERE dedupe_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_work_events_scope
    ON work_events(project_ref, worktree, event_id);
CREATE INDEX IF NOT EXISTS idx_work_events_ts ON work_events(ts);

CREATE TABLE IF NOT EXISTS work_state (
    project_ref      TEXT NOT NULL,
    worktree         TEXT NOT NULL,
    handoff_md       TEXT NOT NULL DEFAULT '',
    through_event_id INTEGER NOT NULL DEFAULT 0,
    log_cursors_json TEXT NOT NULL DEFAULT '{}',
    updated_at       INTEGER NOT NULL,
    PRIMARY KEY (project_ref, worktree)
);
"#;

pub(super) fn ensure_work_journal_auxiliary_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(WORK_JOURNAL_AUXILIARY_SCHEMA)
        .map_err(|error| format!("cannot ensure work journal auxiliary schema: {error}"))
}

/// Truncate to at most `max` chars on a char boundary, marking truncation.
pub(crate) fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NewWorkEvent {
    pub ts: i64,
    pub project_ref: String,
    pub worktree: String,
    pub provider: String,
    pub account: Option<String>,
    pub kind: String,
    pub summary: String,
    pub source_ref: String,
    pub dedupe_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkEvent {
    pub event_id: i64,
    pub ts: i64,
    pub project_ref: String,
    pub worktree: String,
    pub provider: String,
    pub account: Option<String>,
    pub kind: String,
    pub summary: String,
    pub source_ref: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkState {
    pub project_ref: String,
    pub worktree: String,
    pub handoff_md: String,
    pub through_event_id: i64,
    pub log_cursors_json: String,
    pub updated_at: i64,
}

const EVENT_COLUMNS: &str =
    "event_id, ts, project_ref, worktree, provider, account, kind, summary, source_ref";

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkEvent> {
    Ok(WorkEvent {
        event_id: row.get(0)?,
        ts: row.get(1)?,
        project_ref: row.get(2)?,
        worktree: row.get(3)?,
        provider: row.get(4)?,
        account: row.get(5)?,
        kind: row.get(6)?,
        summary: row.get(7)?,
        source_ref: row.get(8)?,
    })
}

fn sql_err(context: &'static str) -> impl Fn(rusqlite::Error) -> String {
    move |error| format!("{context}: {error}")
}

impl StateStore {
    /// Append one event (summary clipped to 300 chars). Returns `None` when
    /// `dedupe_key` was already recorded (idempotent re-ingest).
    pub(crate) fn append_work_event(&self, event: &NewWorkEvent) -> Result<Option<i64>, String> {
        let summary = clip_chars(event.summary.trim(), WORK_SUMMARY_MAX_CHARS);
        let changed = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO work_events (ts, project_ref, worktree, provider, account, \
                 kind, summary, source_ref, dedupe_key) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    event.ts,
                    event.project_ref,
                    event.worktree,
                    event.provider,
                    event.account,
                    event.kind,
                    summary,
                    event.source_ref,
                    event.dedupe_key,
                ],
            )
            .map_err(sql_err("cannot append work event"))?;
        Ok((changed == 1).then(|| self.conn.last_insert_rowid()))
    }

    /// Events for one project/worktree with `event_id > after`, oldest first.
    pub(crate) fn work_events_after(
        &self,
        project_ref: &str,
        worktree: &str,
        after_event_id: i64,
        limit: usize,
    ) -> Result<Vec<WorkEvent>, String> {
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM work_events WHERE project_ref = ?1 AND worktree = ?2 \
                 AND event_id > ?3 ORDER BY event_id LIMIT ?4"
            ))
            .map_err(sql_err("cannot read work events"))?;
        statement
            .query_map(
                params![project_ref, worktree, after_event_id, limit as i64],
                event_from_row,
            )
            .and_then(Iterator::collect)
            .map_err(sql_err("cannot read work events"))
    }

    /// The newest `limit` events for one project/worktree, oldest first.
    pub(crate) fn latest_work_events(
        &self,
        project_ref: &str,
        worktree: &str,
        limit: usize,
    ) -> Result<Vec<WorkEvent>, String> {
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM work_events WHERE project_ref = ?1 AND worktree = ?2 \
                 ORDER BY event_id DESC LIMIT ?3"
            ))
            .map_err(sql_err("cannot read work events"))?;
        let mut rows: Vec<WorkEvent> = statement
            .query_map(params![project_ref, worktree, limit as i64], event_from_row)
            .and_then(Iterator::collect)
            .map_err(sql_err("cannot read work events"))?;
        rows.reverse();
        Ok(rows)
    }

    /// Every (project_ref, worktree) with events newer than its
    /// `work_state.through_event_id` (or with no state yet).
    pub(crate) fn work_scopes_with_new_events(&self) -> Result<Vec<(String, String)>, String> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT e.project_ref, e.worktree FROM work_events e \
                 LEFT JOIN work_state s ON s.project_ref = e.project_ref AND s.worktree = e.worktree \
                 GROUP BY e.project_ref, e.worktree \
                 HAVING MAX(e.event_id) > COALESCE(MAX(s.through_event_id), 0) \
                 ORDER BY e.project_ref, e.worktree",
            )
            .map_err(sql_err("cannot list work scopes"))?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .and_then(Iterator::collect)
            .map_err(sql_err("cannot list work scopes"))
    }

    pub(crate) fn work_state(
        &self,
        project_ref: &str,
        worktree: &str,
    ) -> Result<Option<WorkState>, String> {
        self.conn
            .query_row(
                "SELECT project_ref, worktree, handoff_md, through_event_id, log_cursors_json, \
                 updated_at FROM work_state WHERE project_ref = ?1 AND worktree = ?2",
                params![project_ref, worktree],
                |row| {
                    Ok(WorkState {
                        project_ref: row.get(0)?,
                        worktree: row.get(1)?,
                        handoff_md: row.get(2)?,
                        through_event_id: row.get(3)?,
                        log_cursors_json: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(sql_err("cannot read work state"))
    }

    pub(crate) fn work_states(&self) -> Result<Vec<WorkState>, String> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT project_ref, worktree, handoff_md, through_event_id, log_cursors_json, \
                 updated_at FROM work_state ORDER BY project_ref, worktree",
            )
            .map_err(sql_err("cannot list work state"))?;
        statement
            .query_map([], |row| {
                Ok(WorkState {
                    project_ref: row.get(0)?,
                    worktree: row.get(1)?,
                    handoff_md: row.get(2)?,
                    through_event_id: row.get(3)?,
                    log_cursors_json: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })
            .and_then(Iterator::collect)
            .map_err(sql_err("cannot list work state"))
    }

    /// Upsert the per-project/worktree state row.
    pub(crate) fn put_work_state(&self, state: &WorkState) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO work_state (project_ref, worktree, handoff_md, through_event_id, \
                 log_cursors_json, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(project_ref, worktree) DO UPDATE SET handoff_md = excluded.handoff_md, \
                 through_event_id = excluded.through_event_id, \
                 log_cursors_json = excluded.log_cursors_json, updated_at = excluded.updated_at",
                params![
                    state.project_ref,
                    state.worktree,
                    state.handoff_md,
                    state.through_event_id,
                    state.log_cursors_json,
                    state.updated_at,
                ],
            )
            .map_err(sql_err("cannot save work state"))?;
        Ok(())
    }

    /// Retention: delete events older than 30 days. `work_state` is kept.
    pub(crate) fn delete_expired_work_events(&self, now_ms: i64) -> Result<usize, String> {
        self.conn
            .execute(
                "DELETE FROM work_events WHERE ts < ?1",
                [now_ms - WORK_RETENTION_MS],
            )
            .map_err(sql_err("cannot delete expired work events"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(worktree: &str, summary: &str, dedupe: Option<&str>, ts: i64) -> NewWorkEvent {
        NewWorkEvent {
            ts,
            project_ref: "proj".into(),
            worktree: worktree.into(),
            provider: "herdr-mcp".into(),
            account: None,
            kind: "tool_call".into(),
            summary: summary.into(),
            source_ref: "mcp:herdr_exec".into(),
            dedupe_key: dedupe.map(str::to_owned),
        }
    }

    #[test]
    fn events_are_clipped_deduped_and_scoped() {
        let store = StateStore::open(":memory:").unwrap();
        let long = "x".repeat(900);
        assert!(
            store
                .append_work_event(&event("/w1", &long, Some("k1"), 1))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store
                .append_work_event(&event("/w1", "dup", Some("k1"), 1))
                .unwrap(),
            None
        );
        store
            .append_work_event(&event("/w1", "b", None, 2))
            .unwrap();
        store
            .append_work_event(&event("/w2", "c", None, 3))
            .unwrap();
        let rows = store.work_events_after("proj", "/w1", 0, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].summary.chars().count(), WORK_SUMMARY_MAX_CHARS);
        let latest = store.latest_work_events("proj", "/w1", 1).unwrap();
        assert_eq!(latest[0].summary, "b");
        assert_eq!(
            store
                .work_events_after("proj", "/w1", rows[0].event_id, 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn state_tracks_new_event_scopes_and_survives_retention() {
        let store = StateStore::open(":memory:").unwrap();
        let first = store
            .append_work_event(&event("/w1", "old", None, 0))
            .unwrap()
            .unwrap();
        store
            .append_work_event(&event("/w2", "x", None, 0))
            .unwrap();
        assert_eq!(store.work_scopes_with_new_events().unwrap().len(), 2);
        store
            .put_work_state(&WorkState {
                project_ref: "proj".into(),
                worktree: "/w1".into(),
                handoff_md: "# handoff".into(),
                through_event_id: first,
                log_cursors_json: r#"{"a":1}"#.into(),
                updated_at: 5,
            })
            .unwrap();
        assert_eq!(
            store.work_scopes_with_new_events().unwrap(),
            vec![("proj".to_owned(), "/w2".to_owned())]
        );
        let now = WORK_RETENTION_MS + 10;
        store
            .append_work_event(&event("/w1", "fresh", None, now))
            .unwrap();
        assert_eq!(store.delete_expired_work_events(now).unwrap(), 2);
        let state = store.work_state("proj", "/w1").unwrap().unwrap();
        assert_eq!(state.handoff_md, "# handoff");
        assert_eq!(store.work_states().unwrap().len(), 1);
        assert_eq!(
            store.latest_work_events("proj", "/w1", 10).unwrap().len(),
            1
        );
    }
}
