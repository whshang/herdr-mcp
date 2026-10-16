//! Passive work journal (relay/handoff MVP) storage.
//!
//! Like the browser archive/dispatch tables, this is a forward-additive
//! *auxiliary* schema rather than a numbered migration: it only adds new
//! tables, never alters schema-15 tables, so a pinned schema-15 binary
//! (PROD rollback target) can still open a DB that DEV has written journal
//! rows into. Rows are never hard-deleted: retention only clears long-form
//! `detail` of unreferenced events and marks rows archived.

use super::StateStore;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub(crate) const WORK_SUMMARY_MAX_CHARS: usize = 300;
pub(crate) const WORK_DETAIL_MAX_CHARS: usize = 2_000;
pub(crate) const WORK_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

const WORK_JOURNAL_AUXILIARY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS work_events (
    event_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    lane          TEXT NOT NULL,
    project_ref   TEXT,
    provider      TEXT NOT NULL,
    account       TEXT,
    kind          TEXT NOT NULL,
    summary       TEXT NOT NULL,
    detail        TEXT,
    source_kind   TEXT NOT NULL,
    source_ref    TEXT NOT NULL,
    dedupe_key    TEXT,
    occurred_at   INTEGER NOT NULL,
    ingested_at   INTEGER NOT NULL,
    referenced    INTEGER NOT NULL DEFAULT 0,
    archived_at   INTEGER,
    CHECK (length(summary) <= 300),
    CHECK (detail IS NULL OR length(detail) <= 2000),
    CHECK (referenced IN (0, 1))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_work_events_dedupe
    ON work_events(dedupe_key) WHERE dedupe_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_work_events_lane
    ON work_events(lane, event_id);
CREATE INDEX IF NOT EXISTS idx_work_events_occurred
    ON work_events(occurred_at);

CREATE TABLE IF NOT EXISTS work_ingest_cursors (
    source_key        TEXT PRIMARY KEY NOT NULL,
    path              TEXT NOT NULL,
    inode             INTEGER NOT NULL,
    offset            INTEGER NOT NULL,
    format            TEXT,
    quarantined_count INTEGER NOT NULL DEFAULT 0,
    updated_at        INTEGER NOT NULL,
    CHECK (offset >= 0)
);

CREATE TABLE IF NOT EXISTS work_ingest_quarantine (
    quarantine_id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_key    TEXT NOT NULL,
    offset        INTEGER NOT NULL,
    reason        TEXT NOT NULL,
    sample        TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    CHECK (length(sample) <= 300)
);

CREATE TABLE IF NOT EXISTS work_compaction_leases (
    lane            TEXT PRIMARY KEY NOT NULL,
    holder          TEXT NOT NULL,
    lease_until     INTEGER NOT NULL,
    rerun_requested INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS work_checkpoints (
    lane             TEXT PRIMARY KEY NOT NULL,
    through_event_id INTEGER NOT NULL,
    version          INTEGER NOT NULL,
    state_text       TEXT NOT NULL,
    decisions_text   TEXT NOT NULL,
    llm_pending      INTEGER NOT NULL DEFAULT 0,
    llm_at           INTEGER,
    updated_at       INTEGER NOT NULL,
    CHECK (version > 0),
    CHECK (llm_pending IN (0, 1))
);

CREATE TABLE IF NOT EXISTS work_segments (
    segment_id       INTEGER PRIMARY KEY AUTOINCREMENT,
    lane             TEXT NOT NULL,
    from_event_id    INTEGER NOT NULL,
    through_event_id INTEGER NOT NULL,
    summary          TEXT NOT NULL,
    status           TEXT NOT NULL DEFAULT 'active',
    merged_into      INTEGER,
    llm_pending      INTEGER NOT NULL DEFAULT 1,
    created_at       INTEGER NOT NULL,
    archived_at      INTEGER,
    CHECK (status IN ('active', 'archived', 'merged')),
    CHECK (through_event_id >= from_event_id)
);
CREATE INDEX IF NOT EXISTS idx_work_segments_lane
    ON work_segments(lane, status, segment_id);
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
    pub lane: String,
    pub project_ref: Option<String>,
    pub provider: String,
    pub account: Option<String>,
    pub kind: String,
    pub summary: String,
    pub detail: Option<String>,
    pub source_kind: String,
    pub source_ref: String,
    pub dedupe_key: Option<String>,
    pub occurred_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkEvent {
    pub event_id: i64,
    pub lane: String,
    pub project_ref: Option<String>,
    pub provider: String,
    pub account: Option<String>,
    pub kind: String,
    pub summary: String,
    pub detail: Option<String>,
    pub source_kind: String,
    pub source_ref: String,
    pub occurred_at: i64,
    pub referenced: bool,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IngestCursor {
    pub source_key: String,
    pub path: String,
    pub inode: i64,
    pub offset: i64,
    pub format: Option<String>,
    pub quarantined_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkCheckpoint {
    pub lane: String,
    pub through_event_id: i64,
    pub version: i64,
    pub state_text: String,
    pub decisions_text: String,
    pub llm_pending: bool,
    pub llm_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkSegment {
    pub segment_id: i64,
    pub lane: String,
    pub from_event_id: i64,
    pub through_event_id: i64,
    pub summary: String,
    pub status: String,
    pub merged_into: Option<i64>,
    pub llm_pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaseOutcome {
    Acquired,
    /// Another holder owns a live lease; a rerun was recorded for it.
    Coalesced,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct RetentionReport {
    pub detail_cleared: usize,
    pub archived: usize,
    pub segments_archived: usize,
}

const EVENT_COLUMNS: &str = "event_id, lane, project_ref, provider, account, kind, summary, detail, \
     source_kind, source_ref, occurred_at, referenced, archived_at";

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkEvent> {
    Ok(WorkEvent {
        event_id: row.get(0)?,
        lane: row.get(1)?,
        project_ref: row.get(2)?,
        provider: row.get(3)?,
        account: row.get(4)?,
        kind: row.get(5)?,
        summary: row.get(6)?,
        detail: row.get(7)?,
        source_kind: row.get(8)?,
        source_ref: row.get(9)?,
        occurred_at: row.get(10)?,
        referenced: row.get::<_, i64>(11)? != 0,
        archived_at: row.get(12)?,
    })
}

fn sql_err(context: &str) -> impl Fn(rusqlite::Error) -> String + '_ {
    move |error| format!("{context}: {error}")
}

impl StateStore {
    /// Append one event. Summary/detail are clipped to their budgets. Returns
    /// `None` when `dedupe_key` was already recorded (idempotent re-ingest).
    pub(crate) fn append_work_event(
        &self,
        event: &NewWorkEvent,
        now_ms: i64,
    ) -> Result<Option<i64>, String> {
        let summary = clip_chars(event.summary.trim(), WORK_SUMMARY_MAX_CHARS);
        let detail = event
            .detail
            .as_deref()
            .map(|detail| clip_chars(detail, WORK_DETAIL_MAX_CHARS));
        let changed = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO work_events (lane, project_ref, provider, account, kind, \
                 summary, detail, source_kind, source_ref, dedupe_key, occurred_at, ingested_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    event.lane,
                    event.project_ref,
                    event.provider,
                    event.account,
                    event.kind,
                    summary,
                    detail,
                    event.source_kind,
                    event.source_ref,
                    event.dedupe_key,
                    event.occurred_at,
                    now_ms,
                ],
            )
            .map_err(sql_err("cannot append work event"))?;
        Ok((changed == 1).then(|| self.conn.last_insert_rowid()))
    }

    pub(crate) fn work_events_after(
        &self,
        lane: &str,
        after_event_id: i64,
        limit: usize,
    ) -> Result<Vec<WorkEvent>, String> {
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM work_events WHERE lane = ?1 AND event_id > ?2 \
                 ORDER BY event_id LIMIT ?3"
            ))
            .map_err(sql_err("cannot read work events"))?;
        statement
            .query_map(params![lane, after_event_id, limit as i64], event_from_row)
            .and_then(Iterator::collect)
            .map_err(sql_err("cannot read work events"))
    }

    /// Latest events for every lane except `lane`, newest first.
    pub(crate) fn work_events_other_lanes(
        &self,
        lane: &str,
        project_ref: Option<&str>,
        after_event_id: i64,
        limit: usize,
    ) -> Result<Vec<WorkEvent>, String> {
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM work_events WHERE lane <> ?1 AND event_id > ?2 \
                 AND (?3 IS NULL OR project_ref = ?3) ORDER BY event_id DESC LIMIT ?4"
            ))
            .map_err(sql_err("cannot read work events"))?;
        statement
            .query_map(
                params![lane, after_event_id, project_ref, limit as i64],
                event_from_row,
            )
            .and_then(Iterator::collect)
            .map_err(sql_err("cannot read work events"))
    }

    /// Case-insensitive substring search over summaries and retained detail.
    pub(crate) fn search_work_events(
        &self,
        query: &str,
        lane: Option<&str>,
        project_ref: Option<&str>,
        limit: usize,
    ) -> Result<Vec<WorkEvent>, String> {
        let pattern = format!(
            "%{}%",
            query
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT {EVENT_COLUMNS} FROM work_events \
                 WHERE (summary LIKE ?1 ESCAPE '\\' OR detail LIKE ?1 ESCAPE '\\' \
                        OR source_ref LIKE ?1 ESCAPE '\\') \
                   AND (?2 IS NULL OR lane = ?2) AND (?3 IS NULL OR project_ref = ?3) \
                 ORDER BY event_id DESC LIMIT ?4"
            ))
            .map_err(sql_err("cannot search work events"))?;
        statement
            .query_map(
                params![pattern, lane, project_ref, limit as i64],
                event_from_row,
            )
            .and_then(Iterator::collect)
            .map_err(sql_err("cannot search work events"))
    }

    pub(crate) fn work_lane_stats(&self, lane: &str) -> Result<(i64, Option<i64>), String> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(event_id), 0), MAX(ingested_at) FROM work_events WHERE lane = ?1",
                [lane],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(sql_err("cannot read work lane stats"))
    }

    pub(crate) fn work_lanes(&self) -> Result<Vec<(String, Option<String>)>, String> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT lane, MAX(project_ref) FROM work_events GROUP BY lane ORDER BY MAX(event_id) DESC",
            )
            .map_err(sql_err("cannot list work lanes"))?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .and_then(Iterator::collect)
            .map_err(sql_err("cannot list work lanes"))
    }

    pub(crate) fn mark_work_events_referenced(&self, event_ids: &[i64]) -> Result<(), String> {
        for event_id in event_ids {
            self.conn
                .execute(
                    "UPDATE work_events SET referenced = 1 WHERE event_id = ?1",
                    [event_id],
                )
                .map_err(sql_err("cannot mark work event referenced"))?;
        }
        Ok(())
    }

    // -- ingest cursors ----------------------------------------------------

    pub(crate) fn ingest_cursor(&self, source_key: &str) -> Result<Option<IngestCursor>, String> {
        self.conn
            .query_row(
                "SELECT source_key, path, inode, offset, format, quarantined_count \
                 FROM work_ingest_cursors WHERE source_key = ?1",
                [source_key],
                |row| {
                    Ok(IngestCursor {
                        source_key: row.get(0)?,
                        path: row.get(1)?,
                        inode: row.get(2)?,
                        offset: row.get(3)?,
                        format: row.get(4)?,
                        quarantined_count: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(sql_err("cannot read ingest cursor"))
    }

    pub(crate) fn save_ingest_cursor(
        &self,
        cursor: &IngestCursor,
        now_ms: i64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO work_ingest_cursors (source_key, path, inode, offset, format, \
                 quarantined_count, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT(source_key) DO UPDATE SET path = excluded.path, inode = excluded.inode, \
                 offset = excluded.offset, format = excluded.format, \
                 quarantined_count = excluded.quarantined_count, updated_at = excluded.updated_at",
                params![
                    cursor.source_key,
                    cursor.path,
                    cursor.inode,
                    cursor.offset,
                    cursor.format,
                    cursor.quarantined_count,
                    now_ms
                ],
            )
            .map_err(sql_err("cannot save ingest cursor"))?;
        Ok(())
    }

    pub(crate) fn quarantine_ingest(
        &self,
        source_key: &str,
        offset: i64,
        reason: &str,
        sample: &str,
        now_ms: i64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO work_ingest_quarantine (source_key, offset, reason, sample, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![source_key, offset, reason, clip_chars(sample, 300), now_ms],
            )
            .map_err(sql_err("cannot quarantine ingest data"))?;
        Ok(())
    }

    pub(crate) fn ingest_quarantine_count(&self, source_key: &str) -> Result<i64, String> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM work_ingest_quarantine WHERE source_key = ?1",
                [source_key],
                |row| row.get(0),
            )
            .map_err(sql_err("cannot count quarantine"))
    }

    // -- compaction leases / checkpoints ----------------------------------

    /// Try to take the single compaction lease for `lane`. If another holder
    /// has a live lease, record a coalesced rerun request instead.
    pub(crate) fn acquire_compaction_lease(
        &mut self,
        lane: &str,
        holder: &str,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<LeaseOutcome, String> {
        self.transaction(|tx| {
            let current: Option<(String, i64)> = tx
                .query_row(
                    "SELECT holder, lease_until FROM work_compaction_leases WHERE lane = ?1",
                    [lane],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(sql_err("cannot read compaction lease"))?;
            match current {
                Some((owner, until)) if owner != holder && until > now_ms => {
                    tx.execute(
                        "UPDATE work_compaction_leases SET rerun_requested = 1 WHERE lane = ?1",
                        [lane],
                    )
                    .map_err(sql_err("cannot coalesce compaction rerun"))?;
                    Ok(LeaseOutcome::Coalesced)
                }
                _ => {
                    tx.execute(
                        "INSERT INTO work_compaction_leases (lane, holder, lease_until, rerun_requested) \
                         VALUES (?1, ?2, ?3, 0) ON CONFLICT(lane) DO UPDATE SET holder = excluded.holder, \
                         lease_until = excluded.lease_until, rerun_requested = 0",
                        params![lane, holder, now_ms + ttl_ms],
                    )
                    .map_err(sql_err("cannot acquire compaction lease"))?;
                    Ok(LeaseOutcome::Acquired)
                }
            }
        })
    }

    /// Release the lease. Returns whether a coalesced rerun was requested
    /// while it was held (the caller should run once more).
    pub(crate) fn release_compaction_lease(
        &self,
        lane: &str,
        holder: &str,
    ) -> Result<bool, String> {
        let rerun: Option<i64> = self
            .conn
            .query_row(
                "SELECT rerun_requested FROM work_compaction_leases WHERE lane = ?1 AND holder = ?2",
                params![lane, holder],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_err("cannot read compaction lease"))?;
        self.conn
            .execute(
                "DELETE FROM work_compaction_leases WHERE lane = ?1 AND holder = ?2",
                params![lane, holder],
            )
            .map_err(sql_err("cannot release compaction lease"))?;
        Ok(rerun.unwrap_or(0) != 0)
    }

    pub(crate) fn work_checkpoint(&self, lane: &str) -> Result<Option<WorkCheckpoint>, String> {
        self.conn
            .query_row(
                "SELECT lane, through_event_id, version, state_text, decisions_text, llm_pending, \
                 llm_at, updated_at FROM work_checkpoints WHERE lane = ?1",
                [lane],
                |row| {
                    Ok(WorkCheckpoint {
                        lane: row.get(0)?,
                        through_event_id: row.get(1)?,
                        version: row.get(2)?,
                        state_text: row.get(3)?,
                        decisions_text: row.get(4)?,
                        llm_pending: row.get::<_, i64>(5)? != 0,
                        llm_at: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(sql_err("cannot read work checkpoint"))
    }

    /// Optimistic-lock checkpoint write. `expected_version` is the version the
    /// writer read (0 = no checkpoint yet). Returns `false` on a lost race.
    pub(crate) fn save_work_checkpoint(
        &self,
        checkpoint: &WorkCheckpoint,
        expected_version: i64,
    ) -> Result<bool, String> {
        let changed = if expected_version == 0 {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO work_checkpoints (lane, through_event_id, version, state_text, \
                     decisions_text, llm_pending, llm_at, updated_at) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        checkpoint.lane,
                        checkpoint.through_event_id,
                        checkpoint.state_text,
                        checkpoint.decisions_text,
                        checkpoint.llm_pending as i64,
                        checkpoint.llm_at,
                        checkpoint.updated_at
                    ],
                )
                .map_err(sql_err("cannot insert work checkpoint"))?
        } else {
            self.conn
                .execute(
                    "UPDATE work_checkpoints SET through_event_id = ?2, version = version + 1, \
                     state_text = ?3, decisions_text = ?4, llm_pending = ?5, llm_at = ?6, updated_at = ?7 \
                     WHERE lane = ?1 AND version = ?8 AND through_event_id <= ?2",
                    params![
                        checkpoint.lane,
                        checkpoint.through_event_id,
                        checkpoint.state_text,
                        checkpoint.decisions_text,
                        checkpoint.llm_pending as i64,
                        checkpoint.llm_at,
                        checkpoint.updated_at,
                        expected_version
                    ],
                )
                .map_err(sql_err("cannot update work checkpoint"))?
        };
        Ok(changed == 1)
    }

    // -- segments ------------------------------------------------------------

    pub(crate) fn insert_work_segment(
        &self,
        lane: &str,
        from_event_id: i64,
        through_event_id: i64,
        summary: &str,
        llm_pending: bool,
        now_ms: i64,
    ) -> Result<i64, String> {
        self.conn
            .execute(
                "INSERT INTO work_segments (lane, from_event_id, through_event_id, summary, llm_pending, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    lane,
                    from_event_id,
                    through_event_id,
                    clip_chars(summary, WORK_DETAIL_MAX_CHARS),
                    llm_pending as i64,
                    now_ms
                ],
            )
            .map_err(sql_err("cannot insert work segment"))?;
        let segment_id = self.conn.last_insert_rowid();
        self.conn
            .execute(
                "UPDATE work_events SET referenced = 1 WHERE lane = ?1 AND event_id BETWEEN ?2 AND ?3",
                params![lane, from_event_id, through_event_id],
            )
            .map_err(sql_err("cannot mark segment events referenced"))?;
        Ok(segment_id)
    }

    pub(crate) fn active_work_segments(&self, lane: &str) -> Result<Vec<WorkSegment>, String> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT segment_id, lane, from_event_id, through_event_id, summary, status, merged_into, \
                 llm_pending FROM work_segments WHERE lane = ?1 AND status = 'active' ORDER BY segment_id",
            )
            .map_err(sql_err("cannot read work segments"))?;
        statement
            .query_map([lane], |row| {
                Ok(WorkSegment {
                    segment_id: row.get(0)?,
                    lane: row.get(1)?,
                    from_event_id: row.get(2)?,
                    through_event_id: row.get(3)?,
                    summary: row.get(4)?,
                    status: row.get(5)?,
                    merged_into: row.get(6)?,
                    llm_pending: row.get::<_, i64>(7)? != 0,
                })
            })
            .and_then(Iterator::collect)
            .map_err(sql_err("cannot read work segments"))
    }

    /// Orbital-style op: archive a segment (kept, hidden from pickup).
    pub(crate) fn archive_work_segment(
        &self,
        segment_id: i64,
        now_ms: i64,
    ) -> Result<bool, String> {
        let changed = self
            .conn
            .execute(
                "UPDATE work_segments SET status = 'archived', archived_at = ?2 \
                 WHERE segment_id = ?1 AND status = 'active'",
                params![segment_id, now_ms],
            )
            .map_err(sql_err("cannot archive work segment"))?;
        Ok(changed == 1)
    }

    /// Orbital-style op: merge `source` into `target` (source kept, status
    /// `merged`, target summary replaced by the merged text).
    pub(crate) fn merge_work_segments(
        &mut self,
        target: i64,
        source: i64,
        merged_summary: &str,
        now_ms: i64,
    ) -> Result<bool, String> {
        if target == source {
            return Ok(false);
        }
        let summary = clip_chars(merged_summary, WORK_DETAIL_MAX_CHARS);
        self.transaction(|tx| {
            let lanes: Vec<String> = {
                let mut statement = tx
                    .prepare(
                        "SELECT lane FROM work_segments WHERE segment_id IN (?1, ?2) AND status = 'active'",
                    )
                    .map_err(sql_err("cannot read work segments"))?;
                statement
                    .query_map(params![target, source], |row| row.get(0))
                    .and_then(Iterator::collect)
                    .map_err(sql_err("cannot read work segments"))?
            };
            if lanes.len() != 2 || lanes[0] != lanes[1] {
                return Ok(false);
            }
            tx.execute(
                "UPDATE work_segments SET status = 'merged', merged_into = ?2, archived_at = ?3 \
                 WHERE segment_id = ?1",
                params![source, target, now_ms],
            )
            .map_err(sql_err("cannot merge work segment"))?;
            tx.execute(
                "UPDATE work_segments SET summary = ?2, llm_pending = 0, \
                 from_event_id = MIN(from_event_id, (SELECT from_event_id FROM work_segments WHERE segment_id = ?3)), \
                 through_event_id = MAX(through_event_id, (SELECT through_event_id FROM work_segments WHERE segment_id = ?3)) \
                 WHERE segment_id = ?1",
                params![target, summary, source],
            )
            .map_err(sql_err("cannot merge work segment"))?;
            Ok(true)
        })
    }

    pub(crate) fn set_work_segment_summary(
        &self,
        segment_id: i64,
        summary: &str,
    ) -> Result<bool, String> {
        let changed = self
            .conn
            .execute(
                "UPDATE work_segments SET summary = ?2, llm_pending = 0 WHERE segment_id = ?1 AND status = 'active'",
                params![segment_id, clip_chars(summary, WORK_DETAIL_MAX_CHARS)],
            )
            .map_err(sql_err("cannot update work segment"))?;
        Ok(changed == 1)
    }

    // -- retention -----------------------------------------------------------

    /// 30-day retention: unreferenced events lose their long-form detail
    /// (summary + source ref stay); every old event is marked archived;
    /// old merged/archived-eligible segments are archived. Nothing is deleted
    /// and referenced excerpts are kept verbatim.
    pub(crate) fn sweep_work_retention(&self, now_ms: i64) -> Result<RetentionReport, String> {
        let cutoff = now_ms - WORK_RETENTION_MS;
        let detail_cleared = self
            .conn
            .execute(
                "UPDATE work_events SET detail = NULL \
                 WHERE occurred_at < ?1 AND referenced = 0 AND detail IS NOT NULL",
                [cutoff],
            )
            .map_err(sql_err("cannot sweep work event detail"))?;
        let archived = self
            .conn
            .execute(
                "UPDATE work_events SET archived_at = ?2 WHERE occurred_at < ?1 AND archived_at IS NULL",
                params![cutoff, now_ms],
            )
            .map_err(sql_err("cannot archive work events"))?;
        let segments_archived = self
            .conn
            .execute(
                "UPDATE work_segments SET status = 'archived', archived_at = ?2 \
                 WHERE status = 'active' AND created_at < ?1 AND segment_id NOT IN ( \
                   SELECT MAX(segment_id) FROM work_segments WHERE status = 'active' GROUP BY lane)",
                params![cutoff, now_ms],
            )
            .map_err(sql_err("cannot archive work segments"))?;
        Ok(RetentionReport {
            detail_cleared,
            archived,
            segments_archived,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(lane: &str, summary: &str, dedupe: Option<&str>, at: i64) -> NewWorkEvent {
        NewWorkEvent {
            lane: lane.into(),
            project_ref: Some("proj".into()),
            provider: "herdr-mcp".into(),
            kind: "tool_call".into(),
            summary: summary.into(),
            detail: Some("detail text".into()),
            source_kind: "mcp_tool".into(),
            source_ref: "herdr_exec".into(),
            dedupe_key: dedupe.map(str::to_owned),
            occurred_at: at,
            ..NewWorkEvent::default()
        }
    }

    #[test]
    fn events_are_clipped_deduped_and_searchable() {
        let store = StateStore::open(":memory:").unwrap();
        let long = "x".repeat(900);
        let first = store
            .append_work_event(&event("a", &long, Some("k1"), 1), 10)
            .unwrap();
        assert!(first.is_some());
        assert_eq!(
            store
                .append_work_event(&event("a", "dup", Some("k1"), 1), 10)
                .unwrap(),
            None
        );
        store
            .append_work_event(&event("b", "Ran cargo_test 100%", None, 2), 10)
            .unwrap();
        let rows = store.work_events_after("a", 0, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].summary.chars().count(), WORK_SUMMARY_MAX_CHARS);
        let found = store
            .search_work_events("cargo_test 100%", None, None, 10)
            .unwrap();
        assert_eq!(found.len(), 1);
        assert!(
            store
                .search_work_events("cargo%test", None, None, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .work_events_other_lanes("a", Some("proj"), 0, 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn checkpoint_uses_optimistic_lock_and_never_moves_backwards() {
        let store = StateStore::open(":memory:").unwrap();
        let mut cp = WorkCheckpoint {
            lane: "a".into(),
            through_event_id: 5,
            version: 0,
            state_text: "s".into(),
            decisions_text: "d".into(),
            llm_pending: true,
            llm_at: None,
            updated_at: 1,
        };
        assert!(store.save_work_checkpoint(&cp, 0).unwrap());
        assert!(!store.save_work_checkpoint(&cp, 0).unwrap());
        cp.through_event_id = 9;
        assert!(store.save_work_checkpoint(&cp, 1).unwrap());
        assert!(
            !store.save_work_checkpoint(&cp, 1).unwrap(),
            "stale version loses"
        );
        cp.through_event_id = 3;
        assert!(
            !store.save_work_checkpoint(&cp, 2).unwrap(),
            "cannot move backwards"
        );
        let saved = store.work_checkpoint("a").unwrap().unwrap();
        assert_eq!((saved.version, saved.through_event_id), (2, 9));
    }

    #[test]
    fn lease_is_single_holder_with_coalesced_rerun() {
        let mut store = StateStore::open(":memory:").unwrap();
        assert_eq!(
            store.acquire_compaction_lease("a", "h1", 0, 100).unwrap(),
            LeaseOutcome::Acquired
        );
        assert_eq!(
            store.acquire_compaction_lease("a", "h2", 10, 100).unwrap(),
            LeaseOutcome::Coalesced
        );
        assert!(store.release_compaction_lease("a", "h1").unwrap());
        assert_eq!(
            store.acquire_compaction_lease("a", "h2", 20, 100).unwrap(),
            LeaseOutcome::Acquired
        );
        assert!(!store.release_compaction_lease("a", "h2").unwrap());
        // Expired leases can be taken over.
        store.acquire_compaction_lease("a", "h1", 0, 5).unwrap();
        assert_eq!(
            store.acquire_compaction_lease("a", "h2", 50, 5).unwrap(),
            LeaseOutcome::Acquired
        );
    }

    #[test]
    fn cursors_and_quarantine_round_trip() {
        let store = StateStore::open(":memory:").unwrap();
        assert!(store.ingest_cursor("k").unwrap().is_none());
        let cursor = IngestCursor {
            source_key: "k".into(),
            path: "/p".into(),
            inode: 7,
            offset: 42,
            format: Some("claude_jsonl".into()),
            quarantined_count: 0,
        };
        store.save_ingest_cursor(&cursor, 1).unwrap();
        assert_eq!(store.ingest_cursor("k").unwrap().unwrap(), cursor);
        store
            .quarantine_ingest("k", 42, "unparseable", &"z".repeat(800), 1)
            .unwrap();
        assert_eq!(store.ingest_quarantine_count("k").unwrap(), 1);
    }

    #[test]
    fn segments_merge_and_archive_without_deleting() {
        let mut store = StateStore::open(":memory:").unwrap();
        for i in 0..4 {
            store
                .append_work_event(&event("a", &format!("e{i}"), None, 1), 1)
                .unwrap();
        }
        let s1 = store
            .insert_work_segment("a", 1, 2, "one", true, 1)
            .unwrap();
        let s2 = store
            .insert_work_segment("a", 3, 4, "two", true, 1)
            .unwrap();
        assert!(store.merge_work_segments(s1, s2, "one+two", 2).unwrap());
        let active = store.active_work_segments("a").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(
            (active[0].from_event_id, active[0].through_event_id),
            (1, 4)
        );
        assert!(!active[0].llm_pending);
        assert!(store.archive_work_segment(s1, 3).unwrap());
        assert!(store.active_work_segments("a").unwrap().is_empty());
        let total: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM work_segments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 2);
        assert!(
            store
                .work_events_after("a", 0, 10)
                .unwrap()
                .iter()
                .all(|e| e.referenced)
        );
    }

    #[test]
    fn retention_keeps_referenced_excerpts_and_never_deletes() {
        let store = StateStore::open(":memory:").unwrap();
        let old = 0;
        let now = WORK_RETENTION_MS + 1_000;
        let kept = store
            .append_work_event(&event("a", "ref", None, old), 0)
            .unwrap()
            .unwrap();
        store
            .append_work_event(&event("a", "unref", None, old), 0)
            .unwrap();
        store
            .append_work_event(&event("a", "fresh", None, now), now)
            .unwrap();
        store.mark_work_events_referenced(&[kept]).unwrap();
        let report = store.sweep_work_retention(now).unwrap();
        assert_eq!(report.detail_cleared, 1);
        assert_eq!(report.archived, 2);
        let rows = store.work_events_after("a", 0, 10).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].detail.as_deref(), Some("detail text"));
        assert_eq!(rows[1].detail, None);
        assert_eq!(rows[1].summary, "unref");
        assert_eq!(rows[2].archived_at, None);
    }
}
