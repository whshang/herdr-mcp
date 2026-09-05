//! Shared local state store backed by SQLite.
//!
//! This is the *foundation* layer for the Reliability Kernel (rust-rearchitecture
//! Phase 7/8): durable operation ledgers, idempotency records, and exec session
//! metadata that must survive a runtime restart. It deliberately does **not**:
//!
//! - expose an MCP tool (no public/epoch-2 contract change);
//! - hold Herdr live workspace/pane/agent snapshots (live state is authoritative);
//! - hold Git live state, stdout/stderr blobs, or API keys/secrets;
//! - know anything about `prompt`, `exec_sessions` runtime behavior, or `runtime`.
//!
//! The store guarantees a well-formed, versioned SQLite file plus a small
//! transactional API. `herdr_prompt` is the first live consumer through the
//! durable operations ledger; exec-session and continuity consumers remain
//! incremental follow-ups.

// Some foundation APIs are intentionally ahead of their consumers. Keep them
// available while Reliability Kernel features migrate onto this shared store.
#![allow(dead_code)]

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// How long a writer waits on a locked database before erroring.
pub const BUSY_TIMEOUT_MS: i64 = 5_000;

/// Max migration version the current binary understands. Keep this numeric so
/// release manifests can pin rollback-compatible durable-state readers; tests
/// assert it remains exactly equal to the append-only migration count.
pub const SCHEMA_VERSION: i64 = 5;

/// Meta-table key holding the applied schema version. Stored as a string.
const META_SCHEMA_VERSION: &str = "schema_version";

// ---------------------------------------------------------------------------
// Migrations
//
// Every migration is explicit, idempotent (`IF NOT EXISTS`), and applied inside
// a single transaction together with the version bump, so a partially-applied
// migration can never be observed as "complete". Never edit an applied
// migration in place: append a new entry. A store whose stored version is
// *higher* than `MIGRATIONS.len()` is refused (fail-closed, no silent downgrade).
// ---------------------------------------------------------------------------

/// Migration 1: the initial durable tables.
///
/// Schema design notes:
/// * `operations.idempotency_key` has a plain (non-UNIQUE) index on purpose. A
///   global UNIQUE would over-constrain idempotency: whether the same key is
///   a true duplicate depends on the operation `kind`/scope (two distinct
///   logical operations could legitimately share a key only if scoped). The
///   exact scoping rule is a later decision; a partial index keeps lookups
///   fast without locking in a wrong uniqueness contract. Decide and enforce
///   scoped uniqueness in the migration that first inserts real operations.
/// * `exec_sessions` stores metadata only — never stdout/stderr payloads.
const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS operations (
    op_id              TEXT PRIMARY KEY NOT NULL,
    kind               TEXT NOT NULL,
    idempotency_key    TEXT,
    request_hash       TEXT,
    phase              TEXT,
    state              TEXT,
    runtime_generation TEXT,
    boot_id            TEXT,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL,
    expires_at         INTEGER
);

CREATE INDEX IF NOT EXISTS idx_operations_idempotency
    ON operations(idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_operations_expires
    ON operations(expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS exec_sessions (
    session_id    TEXT PRIMARY KEY,
    pid           INTEGER,
    process_group INTEGER,
    started_at    INTEGER NOT NULL,
    ended_at      INTEGER,
    exit_code     INTEGER,
    signal        TEXT,
    state         TEXT NOT NULL,
    expires_at    INTEGER
);

CREATE INDEX IF NOT EXISTS idx_exec_sessions_expires
    ON exec_sessions(expires_at) WHERE expires_at IS NOT NULL;
"#;

/// Migration 2: make operation idempotency durable and replayable.
///
/// The stored idempotency value is an application-provided digest rather than
/// the raw public key. Uniqueness is scoped by operation kind so unrelated
/// mutation families can intentionally reuse the same external key.
const MIGRATION_V2: &str = r#"
ALTER TABLE operations ADD COLUMN result_json TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_operations_kind_idempotency
    ON operations(kind, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
"#;

/// Migration 3: durable runtime-generation identity and bounded service
/// lifecycle evidence. Secrets and full launchd plists are deliberately not
/// stored here; the database only owns control-plane identity/status facts.
const MIGRATION_V3: &str = r#"
CREATE TABLE IF NOT EXISTS runtime_generations (
    generation_id TEXT PRIMARY KEY NOT NULL,
    runtime_path   TEXT NOT NULL,
    sha256         TEXT NOT NULL,
    source         TEXT NOT NULL,
    state          TEXT NOT NULL,
    installed_at   INTEGER NOT NULL,
    activated_at   INTEGER,
    deactivated_at INTEGER
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_runtime_generation_active
    ON runtime_generations(state) WHERE state = 'active';

CREATE TABLE IF NOT EXISTS service_events (
    event_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    action        TEXT NOT NULL,
    outcome       TEXT NOT NULL,
    generation_id TEXT,
    at            INTEGER NOT NULL,
    detail        TEXT
);

CREATE INDEX IF NOT EXISTS idx_service_events_at
    ON service_events(at DESC);
"#;

/// Migration 4: post-commit service rollback identity. Sensitive plist bytes
/// remain in mode-0600 backup files; SQLite stores only owned backup paths and
/// lifecycle/fencing metadata so a browser-UAT failure can deterministically
/// return to the exact previous service after install has already committed.
const MIGRATION_V4: &str = r#"
CREATE TABLE IF NOT EXISTS service_rollbacks (
    rollback_id             TEXT PRIMARY KEY NOT NULL,
    source_kind             TEXT NOT NULL,
    activated_generation_id TEXT NOT NULL,
    server_plist_backup     TEXT,
    watchdog_plist_backup   TEXT,
    previous_current_target TEXT,
    server_was_loaded       INTEGER NOT NULL,
    watchdog_was_loaded     INTEGER NOT NULL,
    created_at              INTEGER NOT NULL,
    state                   TEXT NOT NULL,
    consumed_at             INTEGER
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_service_rollbacks_ready
    ON service_rollbacks(state) WHERE state = 'ready';
CREATE INDEX IF NOT EXISTS idx_service_rollbacks_created
    ON service_rollbacks(created_at DESC);
"#;

/// Migration 5: crash-safe browser conversation continuity.
/// Raw turns are append-only and idempotent by `(continuity_id, message_id)`.
const MIGRATION_V5: &str = r#"
CREATE TABLE IF NOT EXISTS continuity_chains (
    continuity_id TEXT PRIMARY KEY NOT NULL,
    title          TEXT,
    project_id     TEXT,
    status         TEXT NOT NULL DEFAULT 'active',
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_continuity_chains_updated
    ON continuity_chains(updated_at DESC);

CREATE TABLE IF NOT EXISTS continuity_bindings (
    continuity_id   TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    workspace_id    TEXT NOT NULL DEFAULT '',
    bound_at        INTEGER NOT NULL,
    PRIMARY KEY (continuity_id, conversation_id, workspace_id),
    FOREIGN KEY (continuity_id) REFERENCES continuity_chains(continuity_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_continuity_bindings_conversation
    ON continuity_bindings(conversation_id);

CREATE TABLE IF NOT EXISTS continuity_turns (
    continuity_id   TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    message_id      TEXT NOT NULL,
    role            TEXT NOT NULL,
    text            TEXT NOT NULL,
    fingerprint     TEXT,
    observed_at     INTEGER NOT NULL,
    PRIMARY KEY (continuity_id, message_id),
    FOREIGN KEY (continuity_id) REFERENCES continuity_chains(continuity_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_continuity_turns_order
    ON continuity_turns(continuity_id, observed_at DESC);

CREATE TABLE IF NOT EXISTS continuity_checkpoints (
    continuity_id      TEXT NOT NULL,
    checkpoint_id      TEXT NOT NULL,
    through_message_id TEXT,
    summary            TEXT NOT NULL,
    anchors_json       TEXT,
    created_at         INTEGER NOT NULL,
    PRIMARY KEY (continuity_id, checkpoint_id),
    FOREIGN KEY (continuity_id) REFERENCES continuity_chains(continuity_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_continuity_checkpoints_latest
    ON continuity_checkpoints(continuity_id, created_at DESC);

CREATE TABLE IF NOT EXISTS continuity_transfers (
    transfer_id            TEXT PRIMARY KEY NOT NULL,
    continuity_id          TEXT NOT NULL,
    source_conversation_id TEXT,
    target_conversation_id TEXT,
    state                  TEXT NOT NULL,
    created_at             INTEGER NOT NULL,
    updated_at             INTEGER NOT NULL,
    FOREIGN KEY (continuity_id) REFERENCES continuity_chains(continuity_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_continuity_transfers_chain
    ON continuity_transfers(continuity_id, updated_at DESC);
"#;

/// Ordered, append-only migration list. Index `i` (0-based) upgrades from
/// version `i` to version `i + 1`.
const MIGRATIONS: &[&str] = &[
    MIGRATION_V1,
    MIGRATION_V2,
    MIGRATION_V3,
    MIGRATION_V4,
    MIGRATION_V5,
];

/// Existing durable operation found while reserving an idempotency key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecord {
    pub op_id: String,
    pub request_hash: String,
    pub phase: Option<String>,
    pub state: Option<String>,
    pub result_json: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: Option<i64>,
}

/// Result of atomically reserving an operation idempotency slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationReservation {
    Reserved,
    Existing(OperationRecord),
}

#[derive(Debug, Clone, Copy)]
pub struct ContinuityTurnInput<'a> {
    pub continuity_id: &'a str,
    pub conversation_id: &'a str,
    pub workspace_id: Option<&'a str>,
    pub project_id: Option<&'a str>,
    pub title: Option<&'a str>,
    pub message_id: &'a str,
    pub role: &'a str,
    pub text: &'a str,
    pub fingerprint: Option<&'a str>,
    pub observed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuityTurnRecord {
    pub conversation_id: String,
    pub message_id: String,
    pub role: String,
    pub text: String,
    pub observed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuityResumeRecord {
    pub continuity_id: String,
    pub title: Option<String>,
    pub project_id: Option<String>,
    pub status: String,
    pub checkpoint: Option<String>,
    pub turns: Vec<ContinuityTurnRecord>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuityCandidate {
    pub continuity_id: String,
    pub title: Option<String>,
    pub project_id: Option<String>,
    pub status: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ContinuitySearchInput<'a> {
    pub project_id: Option<&'a str>,
    pub workspace_id: Option<&'a str>,
    pub conversation_id: Option<&'a str>,
    pub query: Option<&'a str>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuitySearchCandidate {
    pub continuity_id: String,
    pub title: Option<String>,
    pub project_id: Option<String>,
    pub status: String,
    pub updated_at: i64,
    pub workspace_ids: Vec<String>,
    pub recent_user_excerpt: Option<String>,
    pub recent_assistant_excerpt: Option<String>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// An opened state store. Single-owner (not `Sync`); transaction helpers take
/// `&mut self`.
#[derive(Debug)]
pub struct StateStore {
    path: Option<PathBuf>,
    conn: Connection,
}

impl StateStore {
    /// Open (creating and migrating as needed) a database at `path`.
    ///
    /// Parent directories are created and secured `0o700` on unix; the DB file
    /// itself is secured `0o600`. Passing `:memory:` opens a throwaway
    /// in-memory database (no WAL, no file, no permissions).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        if path.as_os_str() == ":memory:" {
            let mut conn = open_connection(None)?;
            migrate(&mut conn)?;
            return Ok(Self { path: None, conn });
        }

        ensure_parent_dirs(path)?;
        prepare_db_file(path)?;
        let mut conn = open_connection(Some(path))?;
        #[cfg(unix)]
        set_mode(path, 0o600)?;
        migrate(&mut conn)?;
        Ok(Self {
            path: Some(path.to_path_buf()),
            conn,
        })
    }

    /// Open a database file named `name` inside a state directory, creating and
    /// securing the directory first. A `db_name` ending in `.db` or `.sqlite`
    /// keeps its suffix; otherwise `.db` is appended.
    pub fn open_in_dir(state_dir: impl AsRef<Path>, db_name: &str) -> Result<Self, String> {
        let dir = state_dir.as_ref();
        reject_symlink(dir, "state dir")?;
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("cannot create state dir {}: {error}", dir.display()))?;
        #[cfg(unix)]
        secure_dir(dir)?;
        let file_name = db_file_name(db_name)?;
        let path = dir.join(file_name);
        prepare_db_file(&path)?;
        let mut conn = open_connection(Some(&path))?;
        #[cfg(unix)]
        set_mode(&path, 0o600)?;
        migrate(&mut conn)?;
        Ok(Self {
            path: Some(path),
            conn,
        })
    }

    /// Run `body` inside a single transaction. On `Ok` the transaction commits;
    /// on `Err` it rolls back and the error is returned. Use for the future
    /// browser-binding cutover and operation-ledger updates that must be atomic.
    pub fn transaction<T>(
        &mut self,
        body: impl FnOnce(&mut Tx) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut tx = self
            .conn
            .transaction()
            .map_err(|error| format!("cannot begin transaction: {error}"))?;
        let mut wrapper = Tx { conn: &mut tx };
        match body(&mut wrapper) {
            Ok(value) => {
                tx.commit()
                    .map_err(|error| format!("cannot commit transaction: {error}"))?;
                Ok(value)
            }
            Err(error) => {
                let _ = tx.rollback();
                Err(error)
            }
        }
    }

    /// Execute a batch of SQL (used by tests and future migrations).
    pub fn execute_batch(&self, sql: &str) -> Result<(), String> {
        self.conn
            .execute_batch(sql)
            .map_err(|error| format!("state store batch failed: {error}"))
    }

    /// Execute a single SQL statement with no parameters.
    pub fn execute(&self, sql: &str) -> Result<usize, String> {
        self.conn
            .execute(sql, [])
            .map_err(|error| format!("state store execute failed: {error}"))
    }

    /// Query the first column of the first row as i64, if present.
    pub fn scalar_i64(&self, sql: &str) -> Result<Option<i64>, String> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|error| format!("state store prepare failed: {error}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|error| format!("state store query failed: {error}"))?;
        match rows
            .next()
            .map_err(|error| format!("state store step failed: {error}"))?
        {
            Some(row) => row
                .get(0)
                .map(Some)
                .map_err(|error| format!("state store get failed: {error}")),
            None => Ok(None),
        }
    }

    /// Query the first column of the first row as a text scalar, if present.
    pub fn scalar_text(&self, sql: &str) -> Result<Option<String>, String> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|error| format!("state store prepare failed: {error}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|error| format!("state store query failed: {error}"))?;
        match rows
            .next()
            .map_err(|error| format!("state store step failed: {error}"))?
        {
            Some(row) => row
                .get(0)
                .map(Some)
                .map_err(|error| format!("state store get failed: {error}")),
            None => Ok(None),
        }
    }

    /// List all table names in the schema.
    pub fn table_names(&self) -> Result<Vec<String>, String> {
        list_names(
            &self.conn,
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
        )
    }

    /// List all index names in the schema.
    pub fn index_names(&self) -> Result<Vec<String>, String> {
        list_names(
            &self.conn,
            "SELECT name FROM sqlite_master WHERE type='index' ORDER BY name",
        )
    }

    /// Current journal mode as reported by SQLite (`wal`, `memory`, ...).
    pub fn journal_mode(&self) -> Result<String, String> {
        self.scalar_text("PRAGMA journal_mode")
            .map(|value| value.unwrap_or_else(|| "unknown".to_owned()))
    }

    /// Current `foreign_keys` pragma value (1 when on).
    pub fn foreign_keys(&self) -> Result<i64, String> {
        self.scalar_i64("PRAGMA foreign_keys")
            .map(|value| value.unwrap_or(0))
    }

    /// Current `busy_timeout` pragma in milliseconds.
    pub fn busy_timeout(&self) -> Result<i64, String> {
        self.scalar_i64("PRAGMA busy_timeout")
            .map(|value| value.unwrap_or(0))
    }

    /// Stored schema version from the `meta` table.
    pub fn schema_version(&self) -> Result<i64, String> {
        let version = self.scalar_text(&format!(
            "SELECT value FROM meta WHERE key = '{META_SCHEMA_VERSION}'"
        ))?;
        let value =
            version.ok_or_else(|| "meta schema_version missing after migration".to_owned())?;
        value
            .parse::<i64>()
            .map_err(|_| format!("invalid meta schema_version {value:?}; refusing to continue"))
    }

    /// Path of the open database file, if any (None for in-memory).
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Basic diagnostics: path, version, journal mode, foreign_keys, timeout.
    pub fn diagnostics(&self) -> Result<StateStoreDiagnostics, String> {
        Ok(StateStoreDiagnostics {
            path: self.path().map(|p| p.display().to_string()),
            schema_version: self.schema_version()?,
            journal_mode: self.journal_mode()?,
            foreign_keys: self.foreign_keys()?,
            busy_timeout_ms: self.busy_timeout()?,
        })
    }

    /// Atomically reserve `(kind, idempotency_key)` for one mutation request.
    ///
    /// Expired rows are removed first. `INSERT OR IGNORE` plus the scoped
    /// unique index makes concurrent processes converge on one authoritative
    /// row without a read-then-write race.
    pub fn reserve_operation(
        &mut self,
        kind: &str,
        idempotency_key: &str,
        request_hash: &str,
        op_id: &str,
        now_ms: i64,
        expires_at: i64,
    ) -> Result<OperationReservation, String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("cannot begin operation reservation: {error}"))?;
        tx.execute(
            "DELETE FROM operations WHERE expires_at IS NOT NULL AND expires_at <= ?1",
            params![now_ms],
        )
        .map_err(|error| format!("cannot prune expired operations: {error}"))?;
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO operations (
                    op_id, kind, idempotency_key, request_hash, phase, state,
                    created_at, updated_at, expires_at
                 ) VALUES (?1, ?2, ?3, ?4, 'reserved', 'pending', ?5, ?5, ?6)",
                params![
                    op_id,
                    kind,
                    idempotency_key,
                    request_hash,
                    now_ms,
                    expires_at
                ],
            )
            .map_err(|error| format!("cannot reserve operation: {error}"))?;
        if inserted == 1 {
            tx.commit()
                .map_err(|error| format!("cannot commit operation reservation: {error}"))?;
            return Ok(OperationReservation::Reserved);
        }

        let existing = tx
            .query_row(
                "SELECT op_id, request_hash, phase, state, result_json,
                        created_at, updated_at, expires_at
                 FROM operations
                 WHERE kind = ?1 AND idempotency_key = ?2",
                params![kind, idempotency_key],
                |row| {
                    Ok(OperationRecord {
                        op_id: row.get(0)?,
                        request_hash: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        phase: row.get(2)?,
                        state: row.get(3)?,
                        result_json: row.get(4)?,
                        created_at: row.get(5)?,
                        updated_at: row.get(6)?,
                        expires_at: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("cannot read reserved operation: {error}"))?;
        tx.commit()
            .map_err(|error| format!("cannot commit operation lookup: {error}"))?;
        existing.map(OperationReservation::Existing).ok_or_else(|| {
            "operation reservation was ignored without an authoritative existing row".to_owned()
        })
    }

    /// Mark one reserved operation complete and persist the bounded replay
    /// payload. A missing/mismatched row is an integrity error, not an upsert.
    pub fn complete_operation(
        &mut self,
        kind: &str,
        idempotency_key: &str,
        request_hash: &str,
        result_json: &str,
        now_ms: i64,
        expires_at: i64,
    ) -> Result<(), String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("cannot begin operation completion: {error}"))?;
        let changed = tx
            .execute(
                "UPDATE operations
                 SET phase = 'settled', state = 'complete', result_json = ?1,
                     updated_at = ?2, expires_at = ?3
                 WHERE kind = ?4 AND idempotency_key = ?5 AND request_hash = ?6",
                params![
                    result_json,
                    now_ms,
                    expires_at,
                    kind,
                    idempotency_key,
                    request_hash
                ],
            )
            .map_err(|error| format!("cannot complete operation: {error}"))?;
        if changed != 1 {
            return Err(format!(
                "operation completion expected one matching row, updated {changed}"
            ));
        }
        tx.commit()
            .map_err(|error| format!("cannot commit operation completion: {error}"))
    }

    pub fn append_continuity_turn(
        &mut self,
        input: ContinuityTurnInput<'_>,
    ) -> Result<bool, String> {
        let ContinuityTurnInput {
            continuity_id,
            conversation_id,
            workspace_id,
            project_id,
            title,
            message_id,
            role,
            text,
            fingerprint,
            observed_at,
        } = input;
        if continuity_id.is_empty() || conversation_id.is_empty() || message_id.is_empty() {
            return Err("continuity_id, conversation_id and message_id are required".to_owned());
        }
        if !matches!(role, "user" | "assistant") {
            return Err(format!("unsupported continuity role {role:?}"));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("cannot begin continuity append: {error}"))?;
        tx.execute(
            "INSERT INTO continuity_chains (
                continuity_id, title, project_id, status, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'active', ?4, ?4)
             ON CONFLICT(continuity_id) DO UPDATE SET
                title = COALESCE(excluded.title, continuity_chains.title),
                project_id = COALESCE(excluded.project_id, continuity_chains.project_id),
                updated_at = MAX(continuity_chains.updated_at, excluded.updated_at)",
            params![continuity_id, title, project_id, observed_at],
        )
        .map_err(|error| format!("cannot upsert continuity chain: {error}"))?;
        tx.execute(
            "INSERT OR IGNORE INTO continuity_bindings (
                continuity_id, conversation_id, workspace_id, bound_at
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                continuity_id,
                conversation_id,
                workspace_id.unwrap_or(""),
                observed_at
            ],
        )
        .map_err(|error| format!("cannot persist continuity binding: {error}"))?;
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO continuity_turns (
                    continuity_id, conversation_id, message_id, role, text,
                    fingerprint, observed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    continuity_id,
                    conversation_id,
                    message_id,
                    role,
                    text,
                    fingerprint,
                    observed_at
                ],
            )
            .map_err(|error| format!("cannot append continuity turn: {error}"))?;
        tx.commit()
            .map_err(|error| format!("cannot commit continuity append: {error}"))?;
        Ok(inserted == 1)
    }

    pub fn continuity_resume(
        &self,
        continuity_id: &str,
        max_turns: usize,
    ) -> Result<Option<ContinuityResumeRecord>, String> {
        let chain = self
            .conn
            .query_row(
                "SELECT continuity_id, title, project_id, status, updated_at
                 FROM continuity_chains WHERE continuity_id = ?1",
                [continuity_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("cannot read continuity chain: {error}"))?;
        let Some((continuity_id, title, project_id, status, updated_at)) = chain else {
            return Ok(None);
        };
        let checkpoint = self
            .conn
            .query_row(
                "SELECT summary FROM continuity_checkpoints
                 WHERE continuity_id = ?1 ORDER BY created_at DESC LIMIT 1",
                [&continuity_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("cannot read continuity checkpoint: {error}"))?;
        let limit = i64::try_from(max_turns.clamp(1, 64)).unwrap_or(64);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT conversation_id, message_id, role, text, observed_at
                 FROM continuity_turns WHERE continuity_id = ?1
                 ORDER BY observed_at DESC LIMIT ?2",
            )
            .map_err(|error| format!("cannot prepare continuity turns query: {error}"))?;
        let rows = stmt
            .query_map(params![continuity_id, limit], |row| {
                Ok(ContinuityTurnRecord {
                    conversation_id: row.get(0)?,
                    message_id: row.get(1)?,
                    role: row.get(2)?,
                    text: row.get(3)?,
                    observed_at: row.get(4)?,
                })
            })
            .map_err(|error| format!("cannot query continuity turns: {error}"))?;
        let mut turns = Vec::new();
        for row in rows {
            turns.push(row.map_err(|error| format!("cannot decode continuity turn: {error}"))?);
        }
        turns.reverse();
        // Keep resume payloads bounded. The browser stores raw history durably;
        // a fresh context window should receive only the most recent useful tail.
        let mut bytes = turns.iter().map(|turn| turn.text.len()).sum::<usize>();
        while turns.len() > 1 && bytes > 64 * 1024 {
            bytes = bytes.saturating_sub(turns[0].text.len());
            turns.remove(0);
        }
        Ok(Some(ContinuityResumeRecord {
            continuity_id,
            title,
            project_id,
            status,
            checkpoint,
            turns,
            updated_at,
        }))
    }

    pub fn continuity_candidates(&self, limit: usize) -> Result<Vec<ContinuityCandidate>, String> {
        let limit = i64::try_from(limit.clamp(1, 20)).unwrap_or(20);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT continuity_id, title, project_id, status, updated_at
                 FROM continuity_chains WHERE status = 'active'
                 ORDER BY updated_at DESC LIMIT ?1",
            )
            .map_err(|error| format!("cannot prepare continuity candidates query: {error}"))?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(ContinuityCandidate {
                    continuity_id: row.get(0)?,
                    title: row.get(1)?,
                    project_id: row.get(2)?,
                    status: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            })
            .map_err(|error| format!("cannot query continuity candidates: {error}"))?;
        let mut result = Vec::new();
        for row in rows {
            result
                .push(row.map_err(|error| format!("cannot decode continuity candidate: {error}"))?);
        }
        Ok(result)
    }

    pub fn continuity_search(
        &self,
        input: ContinuitySearchInput<'_>,
    ) -> Result<Vec<ContinuitySearchCandidate>, String> {
        let limit = i64::try_from(input.limit.clamp(1, 10)).unwrap_or(10);
        let project_id = input
            .project_id
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let workspace_id = input
            .workspace_id
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let conversation_id = input
            .conversation_id
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let query = input.query.map(str::trim).filter(|value| !value.is_empty());

        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.continuity_id, c.title, c.project_id, c.status, c.updated_at
                 FROM continuity_chains c
                 WHERE c.status = 'active'
                   AND (?1 IS NULL OR c.project_id = ?1)
                   AND (?2 IS NULL OR EXISTS (
                       SELECT 1 FROM continuity_bindings bw
                       WHERE bw.continuity_id = c.continuity_id AND bw.workspace_id = ?2
                   ))
                   AND (?3 IS NULL OR EXISTS (
                       SELECT 1 FROM continuity_bindings bc
                       WHERE bc.continuity_id = c.continuity_id AND bc.conversation_id = ?3
                   ))
                   AND (?4 IS NULL
                        OR instr(lower(COALESCE(c.title, '')), lower(?4)) > 0
                        OR EXISTS (
                            SELECT 1 FROM continuity_turns tq
                            WHERE tq.continuity_id = c.continuity_id
                              AND instr(lower(tq.text), lower(?4)) > 0
                        ))
                 ORDER BY c.updated_at DESC
                 LIMIT ?5",
            )
            .map_err(|error| format!("cannot prepare continuity search: {error}"))?;
        let rows = stmt
            .query_map(
                params![project_id, workspace_id, conversation_id, query, limit],
                |row| {
                    Ok(ContinuityCandidate {
                        continuity_id: row.get(0)?,
                        title: row.get(1)?,
                        project_id: row.get(2)?,
                        status: row.get(3)?,
                        updated_at: row.get(4)?,
                    })
                },
            )
            .map_err(|error| format!("cannot query continuity search: {error}"))?;
        let mut basic = Vec::new();
        for row in rows {
            basic.push(
                row.map_err(|error| format!("cannot decode continuity search candidate: {error}"))?,
            );
        }
        drop(stmt);

        let mut workspace_stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT workspace_id
                 FROM continuity_bindings
                 WHERE continuity_id = ?1 AND workspace_id <> ''
                 ORDER BY bound_at DESC
                 LIMIT 8",
            )
            .map_err(|error| format!("cannot prepare continuity workspace lookup: {error}"))?;
        let mut recent_stmt = self
            .conn
            .prepare(
                "SELECT role, text
                 FROM continuity_turns
                 WHERE continuity_id = ?1
                 ORDER BY observed_at DESC
                 LIMIT 12",
            )
            .map_err(|error| format!("cannot prepare continuity excerpt lookup: {error}"))?;

        let mut result = Vec::with_capacity(basic.len());
        for candidate in basic {
            let workspace_rows = workspace_stmt
                .query_map([candidate.continuity_id.as_str()], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|error| format!("cannot query continuity workspaces: {error}"))?;
            let mut workspace_ids = Vec::new();
            for row in workspace_rows {
                workspace_ids.push(
                    row.map_err(|error| format!("cannot decode continuity workspace: {error}"))?,
                );
            }

            let recent_rows = recent_stmt
                .query_map([candidate.continuity_id.as_str()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| format!("cannot query continuity excerpts: {error}"))?;
            let mut recent_user_excerpt = None;
            let mut recent_assistant_excerpt = None;
            for row in recent_rows {
                let (role, text) =
                    row.map_err(|error| format!("cannot decode continuity excerpt: {error}"))?;
                if role == "user" && recent_user_excerpt.is_none() {
                    recent_user_excerpt = Some(bounded_continuity_excerpt(&text));
                } else if role == "assistant" && recent_assistant_excerpt.is_none() {
                    recent_assistant_excerpt = Some(bounded_continuity_excerpt(&text));
                }
                if recent_user_excerpt.is_some() && recent_assistant_excerpt.is_some() {
                    break;
                }
            }

            result.push(ContinuitySearchCandidate {
                continuity_id: candidate.continuity_id,
                title: candidate.title,
                project_id: candidate.project_id,
                status: candidate.status,
                updated_at: candidate.updated_at,
                workspace_ids,
                recent_user_excerpt,
                recent_assistant_excerpt,
            });
        }
        Ok(result)
    }

    pub fn continuity_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<String>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT b.continuity_id
                 FROM continuity_bindings b
                 JOIN continuity_chains c ON c.continuity_id = b.continuity_id
                 WHERE b.conversation_id = ?1 AND c.status = 'active'
                 ORDER BY c.updated_at DESC LIMIT 2",
            )
            .map_err(|error| format!("cannot prepare continuity binding lookup: {error}"))?;
        let rows = stmt
            .query_map([conversation_id], |row| row.get::<_, String>(0))
            .map_err(|error| format!("cannot query continuity binding: {error}"))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(|error| format!("cannot decode continuity binding: {error}"))?);
        }
        match ids.as_slice() {
            [] => Ok(None),
            [only] => Ok(Some(only.clone())),
            _ => Err("continuity_binding_ambiguous".to_owned()),
        }
    }
}

fn bounded_continuity_excerpt(text: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut chars = text.trim().chars();
    let excerpt = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{excerpt}…")
    } else {
        excerpt
    }
}

/// Human/ops-facing summary of an open store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateStoreDiagnostics {
    pub path: Option<String>,
    pub schema_version: i64,
    pub journal_mode: String,
    pub foreign_keys: i64,
    pub busy_timeout_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecSessionFence {
    pub session_id: String,
    pub pid: u32,
    pub process_group: Option<u32>,
    pub started_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedExecSessionRecord {
    pub session_id: String,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub expires_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeGenerationRecord {
    pub generation_id: String,
    pub runtime_path: String,
    pub sha256: String,
    pub source: String,
    pub state: String,
    pub installed_at: i64,
    pub activated_at: Option<i64>,
    pub deactivated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationTransitionRecord {
    pub timestamp_ms: i64,
    pub previous_generation: Option<String>,
    pub new_generation: Option<String>,
    pub previous_source_commit: Option<String>,
    pub new_source_commit: Option<String>,
    pub trigger: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRollbackRecord {
    pub rollback_id: String,
    pub source_kind: String,
    pub activated_generation_id: String,
    pub server_plist_backup: Option<String>,
    pub watchdog_plist_backup: Option<String>,
    pub previous_current_target: Option<String>,
    pub server_was_loaded: bool,
    pub watchdog_was_loaded: bool,
    pub created_at: i64,
    pub state: String,
}

impl StateStore {
    pub fn record_exec_running(
        &self,
        session_id: &str,
        pid: u32,
        process_group: Option<u32>,
        started_at_ms: u64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO exec_sessions (
                    session_id, pid, process_group, started_at, ended_at,
                    exit_code, signal, state, expires_at
                 ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL, 'running', NULL)
                 ON CONFLICT(session_id) DO UPDATE SET
                    pid = excluded.pid,
                    process_group = excluded.process_group,
                    started_at = excluded.started_at,
                    ended_at = NULL,
                    exit_code = NULL,
                    signal = NULL,
                    state = 'running',
                    expires_at = NULL",
                rusqlite::params![
                    session_id,
                    i64::from(pid),
                    process_group.map(i64::from),
                    i64::try_from(started_at_ms).unwrap_or(i64::MAX),
                ],
            )
            .map_err(|error| format!("cannot persist running exec session: {error}"))?;
        Ok(())
    }

    pub fn record_pane_exec_running(
        &self,
        session_id: &str,
        started_at_ms: u64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO exec_sessions (
                    session_id, pid, process_group, started_at, ended_at,
                    exit_code, signal, state, expires_at
                 ) VALUES (?1, NULL, NULL, ?2, NULL, NULL, NULL, 'running', NULL)
                 ON CONFLICT(session_id) DO UPDATE SET
                    pid = NULL,
                    process_group = NULL,
                    started_at = excluded.started_at,
                    ended_at = NULL,
                    exit_code = NULL,
                    signal = NULL,
                    state = 'running',
                    expires_at = NULL",
                rusqlite::params![session_id, i64::try_from(started_at_ms).unwrap_or(i64::MAX),],
            )
            .map_err(|error| format!("cannot persist running pane exec session: {error}"))?;
        Ok(())
    }

    pub fn settle_exec_session(
        &self,
        session_id: &str,
        state: &str,
        ended_at_ms: Option<u64>,
        exit_code: Option<i32>,
        signal: Option<&str>,
        expires_at_ms: u64,
    ) -> Result<(), String> {
        let changed = self
            .conn
            .execute(
                "UPDATE exec_sessions SET
                    ended_at = ?2,
                    exit_code = ?3,
                    signal = ?4,
                    state = ?5,
                    expires_at = ?6
                 WHERE session_id = ?1",
                rusqlite::params![
                    session_id,
                    ended_at_ms.map(|value| i64::try_from(value).unwrap_or(i64::MAX)),
                    exit_code,
                    signal,
                    state,
                    i64::try_from(expires_at_ms).unwrap_or(i64::MAX),
                ],
            )
            .map_err(|error| format!("cannot settle exec session: {error}"))?;
        if changed == 1 {
            Ok(())
        } else {
            Err(format!(
                "cannot settle exec session {session_id}: durable row is missing"
            ))
        }
    }

    pub fn recoverable_exec_sessions(&self, limit: usize) -> Result<Vec<ExecSessionFence>, String> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_id, pid, process_group, started_at
                 FROM exec_sessions
                 WHERE state = 'running' AND pid IS NOT NULL
                 ORDER BY started_at ASC
                 LIMIT ?1",
            )
            .map_err(|error| format!("cannot prepare exec recovery query: {error}"))?;
        let rows = stmt
            .query_map([limit], |row| {
                let pid = row.get::<_, i64>(1)?;
                let process_group = row.get::<_, Option<i64>>(2)?;
                let started_at = row.get::<_, i64>(3)?;
                Ok((row.get::<_, String>(0)?, pid, process_group, started_at))
            })
            .map_err(|error| format!("cannot query exec recovery rows: {error}"))?;
        let mut result = Vec::new();
        for row in rows {
            let (session_id, pid, process_group, started_at) =
                row.map_err(|error| format!("cannot decode exec recovery row: {error}"))?;
            let pid = u32::try_from(pid)
                .map_err(|_| format!("invalid durable exec pid for {session_id}: {pid}"))?;
            let process_group = process_group
                .map(|value| {
                    u32::try_from(value).map_err(|_| {
                        format!("invalid durable exec process group for {session_id}: {value}")
                    })
                })
                .transpose()?;
            let started_at_ms = u64::try_from(started_at).map_err(|_| {
                format!("invalid durable exec start time for {session_id}: {started_at}")
            })?;
            result.push(ExecSessionFence {
                session_id,
                pid,
                process_group,
                started_at_ms,
            });
        }
        Ok(result)
    }

    pub fn prune_exec_sessions(&self, now_ms: u64) -> Result<usize, String> {
        self.conn
            .execute(
                "DELETE FROM exec_sessions WHERE expires_at IS NOT NULL AND expires_at <= ?1",
                [i64::try_from(now_ms).unwrap_or(i64::MAX)],
            )
            .map_err(|error| format!("cannot prune expired exec sessions: {error}"))
    }

    pub fn get_closed_exec_session(
        &self,
        session_id: &str,
    ) -> Result<Option<ClosedExecSessionRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_id, started_at, ended_at, exit_code, signal, expires_at
                 FROM exec_sessions
                 WHERE session_id = ?1 AND state = 'closed'",
            )
            .map_err(|error| format!("cannot prepare closed exec session query: {error}"))?;
        let mut rows = stmt
            .query_map([session_id], |row| {
                let session_id: String = row.get(0)?;
                let started_at: i64 = row.get(1)?;
                let ended_at: Option<i64> = row.get(2)?;
                let exit_code: Option<i32> = row.get(3)?;
                let signal: Option<String> = row.get(4)?;
                let expires_at: Option<i64> = row.get(5)?;
                Ok((
                    session_id, started_at, ended_at, exit_code, signal, expires_at,
                ))
            })
            .map_err(|error| format!("cannot query closed exec session: {error}"))?;
        match rows.next() {
            Some(row) => {
                let (session_id, started_at, ended_at, exit_code, signal, expires_at) =
                    row.map_err(|error| format!("cannot decode closed exec session: {error}"))?;
                let started_at_ms = u64::try_from(started_at).unwrap_or(0);
                let ended_at_ms = ended_at.map(|v| u64::try_from(v).unwrap_or(0));
                let expires_at_ms = expires_at.map(|v| u64::try_from(v).unwrap_or(0));
                Ok(Some(ClosedExecSessionRecord {
                    session_id,
                    started_at_ms,
                    ended_at_ms,
                    exit_code,
                    signal,
                    expires_at_ms,
                }))
            }
            None => Ok(None),
        }
    }

    pub fn closed_exec_sessions(
        &self,
        now_ms: u64,
        limit: usize,
    ) -> Result<Vec<ClosedExecSessionRecord>, String> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_id, started_at, ended_at, exit_code, signal, expires_at
                 FROM exec_sessions
                 WHERE state = 'closed' AND (expires_at IS NULL OR expires_at > ?1)
                 ORDER BY started_at ASC
                 LIMIT ?2",
            )
            .map_err(|error| format!("cannot prepare closed exec sessions query: {error}"))?;
        let rows = stmt
            .query_map(rusqlite::params![now_i64, limit], |row| {
                let session_id: String = row.get(0)?;
                let started_at: i64 = row.get(1)?;
                let ended_at: Option<i64> = row.get(2)?;
                let exit_code: Option<i32> = row.get(3)?;
                let signal: Option<String> = row.get(4)?;
                let expires_at: Option<i64> = row.get(5)?;
                Ok((
                    session_id, started_at, ended_at, exit_code, signal, expires_at,
                ))
            })
            .map_err(|error| format!("cannot query closed exec sessions: {error}"))?;
        let mut result = Vec::new();
        for row in rows {
            let (session_id, started_at, ended_at, exit_code, signal, expires_at) =
                row.map_err(|error| format!("cannot decode closed exec sessions row: {error}"))?;
            let started_at_ms = u64::try_from(started_at).unwrap_or(0);
            let ended_at_ms = ended_at.map(|v| u64::try_from(v).unwrap_or(0));
            let expires_at_ms = expires_at.map(|v| u64::try_from(v).unwrap_or(0));
            result.push(ClosedExecSessionRecord {
                session_id,
                started_at_ms,
                ended_at_ms,
                exit_code,
                signal,
                expires_at_ms,
            });
        }
        Ok(result)
    }

    pub fn unexpired_exec_session_ids(
        &self,
        now_ms: u64,
    ) -> Result<std::collections::HashSet<String>, String> {
        let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_id FROM exec_sessions
                 WHERE expires_at IS NULL OR expires_at > ?1",
            )
            .map_err(|error| format!("cannot prepare unexpired exec sessions query: {error}"))?;
        let rows = stmt
            .query_map([now_i64], |row| row.get::<_, String>(0))
            .map_err(|error| format!("cannot query unexpired exec sessions: {error}"))?;
        let mut result = std::collections::HashSet::new();
        for row in rows {
            result.insert(row.map_err(|error| format!("cannot decode session id: {error}"))?);
        }
        Ok(result)
    }

    pub fn stage_runtime_generation(
        &self,
        generation_id: &str,
        runtime_path: &str,
        sha256: &str,
        source: &str,
        now_ms: i64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO runtime_generations (
                    generation_id, runtime_path, sha256, source, state,
                    installed_at, activated_at, deactivated_at
                 ) VALUES (?1, ?2, ?3, ?4, 'staged', ?5, NULL, NULL)
                 ON CONFLICT(generation_id) DO UPDATE SET
                    runtime_path = excluded.runtime_path,
                    sha256 = excluded.sha256,
                    source = excluded.source,
                    installed_at = MIN(runtime_generations.installed_at, excluded.installed_at)",
                params![generation_id, runtime_path, sha256, source, now_ms],
            )
            .map_err(|error| format!("cannot stage runtime generation: {error}"))?;
        Ok(())
    }

    pub fn activate_runtime_generation(
        &mut self,
        generation_id: &str,
        now_ms: i64,
    ) -> Result<(), String> {
        self.activate_runtime_generation_with_rollback(generation_id, None, now_ms)
    }

    pub fn activate_runtime_generation_with_rollback(
        &mut self,
        generation_id: &str,
        rollback_id: Option<&str>,
        now_ms: i64,
    ) -> Result<(), String> {
        self.activate_runtime_generation_inner(generation_id, rollback_id, now_ms, None)
    }

    pub fn activate_runtime_generation_with_transition(
        &mut self,
        generation_id: &str,
        rollback_id: Option<&str>,
        now_ms: i64,
        transition: &GenerationTransitionRecord,
    ) -> Result<(), String> {
        if transition.new_generation.as_deref() != Some(generation_id) {
            return Err(format!(
                "generation transition new_generation {:?} does not match activation {generation_id}",
                transition.new_generation
            ));
        }
        self.activate_runtime_generation_inner(generation_id, rollback_id, now_ms, Some(transition))
    }

    fn activate_runtime_generation_inner(
        &mut self,
        generation_id: &str,
        rollback_id: Option<&str>,
        now_ms: i64,
        transition: Option<&GenerationTransitionRecord>,
    ) -> Result<(), String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("cannot begin generation activation: {error}"))?;
        let exists = tx
            .query_row(
                "SELECT COUNT(*) FROM runtime_generations WHERE generation_id = ?1",
                [generation_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("cannot verify staged generation: {error}"))?;
        if exists != 1 {
            return Err(format!(
                "cannot activate unknown runtime generation {generation_id}"
            ));
        }
        tx.execute(
            "UPDATE runtime_generations
             SET state = 'previous', deactivated_at = ?1
             WHERE state = 'active' AND generation_id <> ?2",
            params![now_ms, generation_id],
        )
        .map_err(|error| format!("cannot demote active runtime generation: {error}"))?;
        let changed = tx
            .execute(
                "UPDATE runtime_generations
                 SET state = 'active', activated_at = COALESCE(activated_at, ?1),
                     deactivated_at = NULL
                 WHERE generation_id = ?2",
                params![now_ms, generation_id],
            )
            .map_err(|error| format!("cannot activate runtime generation: {error}"))?;
        if changed != 1 {
            return Err(format!(
                "runtime generation activation updated {changed} rows"
            ));
        }
        if let Some(rollback_id) = rollback_id {
            tx.execute(
                "UPDATE service_rollbacks SET state = 'superseded'
                 WHERE state = 'ready' AND rollback_id <> ?1",
                [rollback_id],
            )
            .map_err(|error| format!("cannot supersede previous service rollback: {error}"))?;
            let changed = tx
                .execute(
                    "UPDATE service_rollbacks SET state = 'ready'
                     WHERE rollback_id = ?1 AND state = 'prepared'",
                    [rollback_id],
                )
                .map_err(|error| format!("cannot mark service rollback ready: {error}"))?;
            if changed != 1 {
                return Err(format!(
                    "service rollback activation expected one prepared row, updated {changed}"
                ));
            }
        }
        if let Some(transition) = transition {
            let detail = serde_json::to_string(transition)
                .map_err(|error| format!("cannot encode generation transition: {error}"))?;
            if detail.len() > 512 {
                return Err("generation transition detail exceeds service event bound".to_owned());
            }
            tx.execute(
                "INSERT INTO service_events (action, outcome, generation_id, at, detail)
                 VALUES ('generation_transition', 'committed', ?1, ?2, ?3)",
                params![generation_id, transition.timestamp_ms, detail],
            )
            .map_err(|error| format!("cannot record generation transition: {error}"))?;
        }
        tx.commit()
            .map_err(|error| format!("cannot commit generation activation: {error}"))
    }

    pub fn latest_generation_transition(
        &self,
    ) -> Result<Option<GenerationTransitionRecord>, String> {
        let row = self
            .conn
            .query_row(
                "SELECT generation_id, at, detail
                 FROM service_events
                 WHERE action = 'generation_transition' AND outcome = 'committed'
                 ORDER BY event_id DESC LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("cannot read latest generation transition: {error}"))?;
        let Some((generation_id, at_ms, detail)) = row else {
            return Ok(None);
        };
        let transition: GenerationTransitionRecord = serde_json::from_str(&detail)
            .map_err(|error| format!("cannot decode latest generation transition: {error}"))?;
        if transition.new_generation != generation_id || transition.timestamp_ms != at_ms {
            return Err("generation transition event metadata is inconsistent".to_owned());
        }
        Ok(Some(transition))
    }

    pub fn active_runtime_generation(&self) -> Result<Option<RuntimeGenerationRecord>, String> {
        self.conn
            .query_row(
                "SELECT generation_id, runtime_path, sha256, source, state,
                        installed_at, activated_at, deactivated_at
                 FROM runtime_generations WHERE state = 'active'",
                [],
                |row| {
                    Ok(RuntimeGenerationRecord {
                        generation_id: row.get(0)?,
                        runtime_path: row.get(1)?,
                        sha256: row.get(2)?,
                        source: row.get(3)?,
                        state: row.get(4)?,
                        installed_at: row.get(5)?,
                        activated_at: row.get(6)?,
                        deactivated_at: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("cannot read active runtime generation: {error}"))
    }

    pub fn record_service_event(
        &self,
        action: &str,
        outcome: &str,
        generation_id: Option<&str>,
        at_ms: i64,
        detail: Option<&str>,
    ) -> Result<(), String> {
        let detail = detail.map(|value| {
            let mut text = value.to_owned();
            if text.len() > 512 {
                text.truncate(512);
            }
            text
        });
        self.conn
            .execute(
                "INSERT INTO service_events (action, outcome, generation_id, at, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![action, outcome, generation_id, at_ms, detail],
            )
            .map_err(|error| format!("cannot record service event: {error}"))?;
        Ok(())
    }

    pub fn record_generation_transition(
        &self,
        transition: &GenerationTransitionRecord,
    ) -> Result<(), String> {
        let detail = serde_json::to_string(transition)
            .map_err(|error| format!("cannot encode generation transition: {error}"))?;
        if detail.len() > 512 {
            return Err("generation transition detail exceeds service event bound".to_owned());
        }
        self.conn
            .execute(
                "INSERT INTO service_events (action, outcome, generation_id, at, detail)
                 VALUES ('generation_transition', 'committed', ?1, ?2, ?3)",
                params![
                    transition.new_generation.as_deref(),
                    transition.timestamp_ms,
                    detail
                ],
            )
            .map_err(|error| format!("cannot record generation transition: {error}"))?;
        Ok(())
    }

    pub fn prepare_service_rollback(&self, record: &ServiceRollbackRecord) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO service_rollbacks (
                    rollback_id, source_kind, activated_generation_id,
                    server_plist_backup, watchdog_plist_backup,
                    previous_current_target, server_was_loaded,
                    watchdog_was_loaded, created_at, state, consumed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'prepared', NULL)",
                params![
                    record.rollback_id,
                    record.source_kind,
                    record.activated_generation_id,
                    record.server_plist_backup,
                    record.watchdog_plist_backup,
                    record.previous_current_target,
                    i64::from(record.server_was_loaded),
                    i64::from(record.watchdog_was_loaded),
                    record.created_at,
                ],
            )
            .map_err(|error| format!("cannot prepare service rollback: {error}"))?;
        Ok(())
    }

    pub fn mark_service_rollback_ready(&mut self, rollback_id: &str) -> Result<(), String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("cannot begin rollback-ready transition: {error}"))?;
        tx.execute(
            "UPDATE service_rollbacks SET state = 'superseded'
             WHERE state = 'ready' AND rollback_id <> ?1",
            [rollback_id],
        )
        .map_err(|error| format!("cannot supersede previous service rollback: {error}"))?;
        let changed = tx
            .execute(
                "UPDATE service_rollbacks SET state = 'ready'
                 WHERE rollback_id = ?1 AND state = 'prepared'",
                [rollback_id],
            )
            .map_err(|error| format!("cannot mark service rollback ready: {error}"))?;
        if changed != 1 {
            return Err(format!(
                "service rollback ready transition expected one prepared row, updated {changed}"
            ));
        }
        tx.commit()
            .map_err(|error| format!("cannot commit rollback-ready transition: {error}"))
    }

    pub fn mark_prepared_service_rollback(
        &self,
        rollback_id: &str,
        state: &str,
        at_ms: i64,
    ) -> Result<(), String> {
        let changed = self
            .conn
            .execute(
                "UPDATE service_rollbacks
                 SET state = ?2, consumed_at = ?3
                 WHERE rollback_id = ?1 AND state IN ('prepared', 'ready')",
                params![rollback_id, state, at_ms],
            )
            .map_err(|error| format!("cannot settle prepared service rollback: {error}"))?;
        if changed == 1 {
            Ok(())
        } else {
            Err(format!(
                "prepared service rollback transition updated {changed} rows"
            ))
        }
    }

    pub fn begin_latest_service_rollback(
        &mut self,
    ) -> Result<Option<ServiceRollbackRecord>, String> {
        let Some(record) = self.latest_ready_service_rollback()? else {
            return Ok(None);
        };
        self.claim_service_rollback(&record.rollback_id).map(Some)
    }

    pub fn latest_ready_service_rollback(&self) -> Result<Option<ServiceRollbackRecord>, String> {
        self.conn
            .query_row(
                "SELECT rollback_id, source_kind, activated_generation_id,
                        server_plist_backup, watchdog_plist_backup,
                        previous_current_target, server_was_loaded,
                        watchdog_was_loaded, created_at, state
                 FROM service_rollbacks
                 WHERE state = 'ready'
                 ORDER BY created_at DESC LIMIT 1",
                [],
                decode_service_rollback,
            )
            .optional()
            .map_err(|error| format!("cannot read ready service rollback: {error}"))
    }

    pub fn protected_service_rollbacks(&self) -> Result<Vec<ServiceRollbackRecord>, String> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT rollback_id, source_kind, activated_generation_id,
                        server_plist_backup, watchdog_plist_backup,
                        previous_current_target, server_was_loaded,
                        watchdog_was_loaded, created_at, state
                 FROM service_rollbacks
                 WHERE state IN ('prepared', 'ready', 'consuming')
                 ORDER BY created_at DESC",
            )
            .map_err(|error| format!("cannot prepare protected service rollback read: {error}"))?;
        let rows = statement
            .query_map([], decode_service_rollback)
            .map_err(|error| format!("cannot read protected service rollbacks: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("cannot decode protected service rollback: {error}"))
    }

    pub fn claim_service_rollback(
        &mut self,
        rollback_id: &str,
    ) -> Result<ServiceRollbackRecord, String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("cannot begin service rollback claim: {error}"))?;
        let record = tx
            .query_row(
                "SELECT rollback_id, source_kind, activated_generation_id,
                        server_plist_backup, watchdog_plist_backup,
                        previous_current_target, server_was_loaded,
                        watchdog_was_loaded, created_at, state
                 FROM service_rollbacks
                 WHERE rollback_id = ?1 AND state = 'ready'",
                [rollback_id],
                decode_service_rollback,
            )
            .optional()
            .map_err(|error| format!("cannot read service rollback claim {rollback_id}: {error}"))?
            .ok_or_else(|| format!("service rollback {rollback_id} is no longer ready"))?;
        let changed = tx
            .execute(
                "UPDATE service_rollbacks SET state = 'consuming'
                 WHERE rollback_id = ?1 AND state = 'ready'",
                [rollback_id],
            )
            .map_err(|error| format!("cannot claim service rollback {rollback_id}: {error}"))?;
        if changed != 1 {
            return Err(format!(
                "service rollback claim lost concurrent race for {rollback_id}"
            ));
        }
        tx.commit().map_err(|error| {
            format!("cannot commit service rollback claim {rollback_id}: {error}")
        })?;
        Ok(ServiceRollbackRecord {
            state: "consuming".to_owned(),
            ..record
        })
    }

    pub fn service_rollback_by_id(
        &self,
        rollback_id: &str,
    ) -> Result<Option<ServiceRollbackRecord>, String> {
        self.conn
            .query_row(
                "SELECT rollback_id, source_kind, activated_generation_id,
                        server_plist_backup, watchdog_plist_backup,
                        previous_current_target, server_was_loaded,
                        watchdog_was_loaded, created_at, state
                 FROM service_rollbacks WHERE rollback_id = ?1",
                [rollback_id],
                decode_service_rollback,
            )
            .optional()
            .map_err(|error| format!("cannot read service rollback {rollback_id}: {error}"))
    }

    pub fn recover_service_rollback_after_guardian(
        &self,
        rollback_id: &str,
        mode: &str,
        at_ms: i64,
    ) -> Result<(), String> {
        let (from_states, target_state, consumed_at) = match mode {
            "install" => (
                &["prepared", "rollback_failed"][..],
                "auto_rolled_back",
                Some(at_ms),
            ),
            "rollback" => (&["consuming", "rollback_failed"][..], "ready", None),
            _ => return Err(format!("unknown guardian rollback recovery mode {mode}")),
        };
        let existing = self
            .service_rollback_by_id(rollback_id)?
            .ok_or_else(|| format!("guardian rollback {rollback_id} is missing"))?;
        if existing.state == target_state {
            return Ok(());
        }
        if !from_states.contains(&existing.state.as_str()) {
            return Err(format!(
                "guardian refuses rollback {rollback_id} transition from {} to {target_state}",
                existing.state
            ));
        }
        let changed = self
            .conn
            .execute(
                "UPDATE service_rollbacks SET state = ?2, consumed_at = ?3
                 WHERE rollback_id = ?1 AND state = ?4",
                params![rollback_id, target_state, consumed_at, existing.state],
            )
            .map_err(|error| format!("cannot settle guardian rollback {rollback_id}: {error}"))?;
        if changed == 1 {
            Ok(())
        } else {
            Err(format!(
                "guardian rollback transition lost concurrent race for {rollback_id}"
            ))
        }
    }

    pub fn finish_service_rollback(
        &self,
        rollback_id: &str,
        state: &str,
        at_ms: i64,
    ) -> Result<(), String> {
        let changed = self
            .conn
            .execute(
                "UPDATE service_rollbacks
                 SET state = ?2,
                     consumed_at = CASE WHEN ?2 = 'ready' THEN NULL ELSE ?3 END
                 WHERE rollback_id = ?1 AND state = 'consuming'",
                params![rollback_id, state, at_ms],
            )
            .map_err(|error| format!("cannot finish service rollback: {error}"))?;
        if changed == 1 {
            Ok(())
        } else {
            Err(format!("service rollback finish updated {changed} rows"))
        }
    }

    pub fn complete_service_rollback(
        &mut self,
        rollback_id: &str,
        activated_generation_id: &str,
        previous_generation_id: Option<&str>,
        at_ms: i64,
    ) -> Result<(), String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("cannot begin completed service rollback: {error}"))?;
        let active = tx
            .query_row(
                "SELECT generation_id FROM runtime_generations WHERE state = 'active'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("cannot read active generation during rollback: {error}"))?;
        if active.as_deref() != Some(activated_generation_id) {
            return Err(format!(
                "active generation changed before rollback completion: expected {activated_generation_id}, got {}",
                active.as_deref().unwrap_or("none")
            ));
        }
        tx.execute(
            "UPDATE runtime_generations
             SET state = 'rolled_back', deactivated_at = ?1
             WHERE generation_id = ?2 AND state = 'active'",
            params![at_ms, activated_generation_id],
        )
        .map_err(|error| format!("cannot retire rolled-back generation: {error}"))?;
        if let Some(previous_generation_id) = previous_generation_id {
            let changed = tx
                .execute(
                    "UPDATE runtime_generations
                     SET state = 'active', deactivated_at = NULL,
                         activated_at = COALESCE(activated_at, ?1)
                     WHERE generation_id = ?2",
                    params![at_ms, previous_generation_id],
                )
                .map_err(|error| format!("cannot reactivate previous generation: {error}"))?;
            if changed != 1 {
                return Err(format!(
                    "cannot reactivate previous generation {previous_generation_id}"
                ));
            }
            tx.execute(
                "UPDATE service_rollbacks
                 SET state = 'ready', consumed_at = NULL
                 WHERE rollback_id = (
                     SELECT rollback_id FROM service_rollbacks
                     WHERE activated_generation_id = ?1 AND state = 'superseded'
                     ORDER BY created_at DESC LIMIT 1
                 )",
                [previous_generation_id],
            )
            .map_err(|error| {
                format!(
                    "cannot reactivate rollback chain for previous generation {previous_generation_id}: {error}"
                )
            })?;
        }
        let changed = tx
            .execute(
                "UPDATE service_rollbacks
                 SET state = 'consumed', consumed_at = ?2
                 WHERE rollback_id = ?1 AND state = 'consuming'",
                params![rollback_id, at_ms],
            )
            .map_err(|error| format!("cannot consume service rollback: {error}"))?;
        if changed != 1 {
            return Err(format!(
                "service rollback consume expected one claimed row, updated {changed}"
            ));
        }
        tx.commit()
            .map_err(|error| format!("cannot commit completed service rollback: {error}"))
    }
}

fn decode_service_rollback(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServiceRollbackRecord> {
    Ok(ServiceRollbackRecord {
        rollback_id: row.get(0)?,
        source_kind: row.get(1)?,
        activated_generation_id: row.get(2)?,
        server_plist_backup: row.get(3)?,
        watchdog_plist_backup: row.get(4)?,
        previous_current_target: row.get(5)?,
        server_was_loaded: row.get::<_, i64>(6)? != 0,
        watchdog_was_loaded: row.get::<_, i64>(7)? != 0,
        created_at: row.get(8)?,
        state: row.get(9)?,
    })
}

// ---------------------------------------------------------------------------
// Transaction handle
// ---------------------------------------------------------------------------

/// A live SQLite transaction. Dropping it without commit rolls back.
pub struct Tx<'conn> {
    conn: &'conn rusqlite::Transaction<'conn>,
}

impl Tx<'_> {
    /// Execute a statement with no parameters.
    pub fn execute(&mut self, sql: &str) -> Result<usize, String> {
        self.conn
            .execute(sql, [])
            .map_err(|error| format!("tx execute failed: {error}"))
    }

    /// Execute a batch of SQL (used to apply migrations atomically).
    pub fn execute_batch(&mut self, sql: &str) -> Result<(), String> {
        self.conn
            .execute_batch(sql)
            .map_err(|error| format!("tx batch failed: {error}"))
    }

    /// Read a single integer scalar.
    pub fn scalar_i64(&self, sql: &str) -> Result<Option<i64>, String> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|error| format!("tx prepare failed: {error}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|error| format!("tx query failed: {error}"))?;
        match rows
            .next()
            .map_err(|error| format!("tx step failed: {error}"))?
        {
            Some(row) => row
                .get(0)
                .map(Some)
                .map_err(|error| format!("tx get failed: {error}")),
            None => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn open_connection(path: Option<&Path>) -> Result<Connection, String> {
    let conn = match path {
        Some(path) => Connection::open(path)
            .map_err(|error| format!("cannot open state store {}: {error}", path.display()))?,
        None => Connection::open_in_memory()
            .map_err(|error| format!("cannot open in-memory state store: {error}"))?,
    };
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| format!("cannot enable foreign_keys: {error}"))?;
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS as u64))
        .map_err(|error| format!("cannot set busy_timeout: {error}"))?;
    // WAL only for on-disk DBs; in-memory journals are inherently "memory".
    if path.is_some() {
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| format!("cannot enable WAL: {error}"))?;
    }
    Ok(conn)
}

/// Ensure the `meta` key/value table exists, then run pending migrations and
/// bump `schema_version`. Version higher than our maximum fails closed.
fn migrate(conn: &mut Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .map_err(|error| format!("cannot bootstrap meta table: {error}"))?;

    let current = i64_schema_version(conn)?;
    let target = SCHEMA_VERSION;
    if current > target {
        return Err(format!(
            "state store schema version {current} is newer than this binary supports \
             ({target}); refusing to run (fail-closed, no silent downgrade)"
        ));
    }

    for (index, migration) in MIGRATIONS.iter().enumerate() {
        let version = (index + 1) as i64;
        if current >= version {
            continue;
        }
        let tx = conn
            .transaction()
            .map_err(|error| format!("cannot begin migration transaction: {error}"))?;
        tx.execute_batch(migration)
            .map_err(|error| format!("migration v{version} failed: {error}"))?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![META_SCHEMA_VERSION, version.to_string()],
        )
        .map_err(|error| format!("cannot record schema version {version}: {error}"))?;
        tx.commit()
            .map_err(|error| format!("cannot commit migration v{version}: {error}"))?;
    }
    Ok(())
}

fn i64_schema_version(conn: &Connection) -> Result<i64, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT value FROM meta WHERE key = '{META_SCHEMA_VERSION}'"
        ))
        .map_err(|error| format!("cannot prepare meta read: {error}"))?;
    let version = stmt
        .query_row([], |row| row.get::<_, String>(0))
        .optional()
        .map_err(|error| format!("cannot read meta schema_version: {error}"))?;
    match version {
        None => Ok(0),
        Some(value) => value
            .parse::<i64>()
            .map_err(|_| format!("invalid meta schema_version {value:?}; refusing to migrate")),
    }
}

fn list_names(conn: &Connection, sql: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|error| format!("state store list prepare failed: {error}"))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("state store list query failed: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("state store list failed: {error}"))?;
    Ok(names)
}

fn reject_symlink(path: &Path, label: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "{label} path {} is a symlink; refusing to follow it",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot inspect {label} {}: {error}",
            path.display()
        )),
    }
}

fn db_file_name(db_name: &str) -> Result<String, String> {
    let path = Path::new(db_name);
    let is_single_name = !db_name.is_empty()
        && path.components().count() == 1
        && path.file_name().and_then(|value| value.to_str()) == Some(db_name);
    if !is_single_name {
        return Err("state db name must be one file name without path components".to_owned());
    }
    Ok(
        if db_name.ends_with(".db") || db_name.ends_with(".sqlite") {
            db_name.to_owned()
        } else {
            format!("{db_name}.db")
        },
    )
}

fn prepare_db_file(path: &Path) -> Result<(), String> {
    reject_symlink(path, "state store")?;
    if path.exists() {
        return Ok(());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            reject_symlink(path, "state store")
        }
        Err(error) => Err(format!(
            "cannot create state store {} securely: {error}",
            path.display()
        )),
    }
}

/// Create parent directories (if any). Only a directory created by Herdr is
/// tightened to `0o700`; never chmod an existing shared/system parent.
fn ensure_parent_dirs(path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let parent_existed = parent.exists();
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create state dir {}: {error}", parent.display()))?;
    #[cfg(unix)]
    if !parent_existed {
        secure_dir(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn secure_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure state dir {}: {error}", dir.display()))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot set state store mode on {}: {error}", path.display()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_db_path() -> PathBuf {
        env::temp_dir().join(format!(
            "herdr-mcp-state-store-{}-{}.sqlite",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn temp_dir() -> PathBuf {
        env::temp_dir().join(format!(
            "herdr-mcp-state-store-dir-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn new_db_creates_and_migrates_to_latest() {
        assert_eq!(SCHEMA_VERSION as usize, MIGRATIONS.len());
        let path = temp_db_path();
        let store = StateStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let tables = store.table_names().unwrap();
        for table in [
            "meta",
            "operations",
            "exec_sessions",
            "runtime_generations",
            "service_events",
            "service_rollbacks",
        ] {
            assert!(tables.contains(&table.to_owned()), "missing table {table}");
        }
        let indexes = store.index_names().unwrap();
        for index in [
            "idx_operations_idempotency",
            "idx_operations_kind_idempotency",
            "idx_operations_expires",
            "idx_exec_sessions_expires",
            "idx_runtime_generation_active",
            "idx_service_events_at",
            "idx_service_rollbacks_ready",
            "idx_service_rollbacks_created",
        ] {
            assert!(indexes.contains(&index.to_owned()), "missing index {index}");
        }
        let sensitive_exec_columns = store
            .scalar_i64(
                "SELECT COUNT(*) FROM pragma_table_info('exec_sessions') \
                 WHERE name IN ('command', 'cwd')",
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            sensitive_exec_columns, 0,
            "durable exec schema must not store command/cwd"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn operation_reservation_is_scoped_atomic_and_replayable() {
        let mut store = StateStore::open(":memory:").unwrap();
        assert_eq!(
            store
                .reserve_operation("herdr_prompt", "key-hash", "req-a", "op-a", 10, 1_000)
                .unwrap(),
            OperationReservation::Reserved
        );
        let OperationReservation::Existing(existing) = store
            .reserve_operation("herdr_prompt", "key-hash", "req-a", "op-a", 11, 1_000)
            .unwrap()
        else {
            panic!("expected existing reservation");
        };
        assert_eq!(existing.op_id, "op-a");
        assert_eq!(existing.request_hash, "req-a");
        assert_eq!(existing.state.as_deref(), Some("pending"));

        assert_eq!(
            store
                .reserve_operation("other_mutation", "key-hash", "req-b", "op-b", 12, 1_000)
                .unwrap(),
            OperationReservation::Reserved,
            "the same digest may be reused by a different operation kind"
        );

        store
            .complete_operation(
                "herdr_prompt",
                "key-hash",
                "req-a",
                r#"{"ok":true}"#,
                20,
                2_000,
            )
            .unwrap();
        let OperationReservation::Existing(completed) = store
            .reserve_operation("herdr_prompt", "key-hash", "req-a", "op-a", 21, 2_000)
            .unwrap()
        else {
            panic!("expected completed operation");
        };
        assert_eq!(completed.state.as_deref(), Some("complete"));
        assert_eq!(completed.phase.as_deref(), Some("settled"));
        assert_eq!(completed.result_json.as_deref(), Some(r#"{"ok":true}"#));
    }

    #[test]
    fn operation_replay_survives_database_reopen() {
        let path = temp_db_path();
        {
            let mut store = StateStore::open(&path).unwrap();
            assert_eq!(
                store
                    .reserve_operation(
                        "herdr_prompt",
                        "persisted-key",
                        "persisted-request",
                        "persisted-op",
                        100,
                        10_000,
                    )
                    .unwrap(),
                OperationReservation::Reserved
            );
            store
                .complete_operation(
                    "herdr_prompt",
                    "persisted-key",
                    "persisted-request",
                    r#"{"ok":true,"value":7}"#,
                    110,
                    10_000,
                )
                .unwrap();
        }
        {
            let mut reopened = StateStore::open(&path).unwrap();
            let OperationReservation::Existing(record) = reopened
                .reserve_operation(
                    "herdr_prompt",
                    "persisted-key",
                    "persisted-request",
                    "persisted-op",
                    120,
                    10_000,
                )
                .unwrap()
            else {
                panic!("expected persisted replay row");
            };
            assert_eq!(record.state.as_deref(), Some("complete"));
            assert_eq!(
                record.result_json.as_deref(),
                Some(r#"{"ok":true,"value":7}"#)
            );
        }
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(path.with_extension("sqlite-wal")).ok();
        std::fs::remove_file(path.with_extension("sqlite-shm")).ok();
    }

    #[test]
    fn runtime_generation_activation_is_single_active_and_service_evidence_is_bounded() {
        let mut store = StateStore::open(":memory:").unwrap();
        store
            .stage_runtime_generation("rust-a", "/runtime/a", "sha-a", "install", 10)
            .unwrap();
        store
            .stage_runtime_generation("rust-b", "/runtime/b", "sha-b", "update", 20)
            .unwrap();
        store.activate_runtime_generation("rust-a", 30).unwrap();
        assert_eq!(
            store.active_runtime_generation().unwrap().unwrap(),
            RuntimeGenerationRecord {
                generation_id: "rust-a".to_owned(),
                runtime_path: "/runtime/a".to_owned(),
                sha256: "sha-a".to_owned(),
                source: "install".to_owned(),
                state: "active".to_owned(),
                installed_at: 10,
                activated_at: Some(30),
                deactivated_at: None,
            }
        );
        store.activate_runtime_generation("rust-b", 40).unwrap();
        let active = store.active_runtime_generation().unwrap().unwrap();
        assert_eq!(active.generation_id, "rust-b");
        assert_eq!(
            store
                .scalar_text("SELECT state FROM runtime_generations WHERE generation_id = 'rust-a'")
                .unwrap()
                .as_deref(),
            Some("previous")
        );
        assert_eq!(
            store
                .scalar_i64(
                    "SELECT deactivated_at FROM runtime_generations WHERE generation_id = 'rust-a'"
                )
                .unwrap(),
            Some(40)
        );
        let long_detail = "x".repeat(900);
        store
            .record_service_event("restart", "ok", Some("rust-b"), 50, Some(&long_detail))
            .unwrap();
        assert_eq!(
            store
                .scalar_i64(
                    "SELECT length(detail) FROM service_events ORDER BY event_id DESC LIMIT 1"
                )
                .unwrap(),
            Some(512)
        );
    }

    #[test]
    fn generation_transition_is_committed_with_activation_and_round_trips_all_fields() {
        let mut store = StateStore::open(":memory:").unwrap();
        store
            .stage_runtime_generation("rust-old", "/runtime/old", "sha-old", "install", 10)
            .unwrap();
        store
            .stage_runtime_generation("rust-new", "/runtime/new", "sha-new", "dev-sync", 20)
            .unwrap();
        store.activate_runtime_generation("rust-old", 30).unwrap();
        let transition = GenerationTransitionRecord {
            timestamp_ms: 40,
            previous_generation: Some("rust-old".to_owned()),
            new_generation: Some("rust-new".to_owned()),
            previous_source_commit: Some("old-commit".to_owned()),
            new_source_commit: Some("new-commit".to_owned()),
            trigger: "dev_sync".to_owned(),
        };
        store
            .activate_runtime_generation_with_transition("rust-new", None, 40, &transition)
            .unwrap();
        assert_eq!(
            store
                .active_runtime_generation()
                .unwrap()
                .unwrap()
                .generation_id,
            "rust-new"
        );
        assert_eq!(
            store.latest_generation_transition().unwrap(),
            Some(transition)
        );
        assert_eq!(
            store
                .scalar_i64(
                    "SELECT COUNT(*) FROM service_events WHERE action = 'generation_transition'"
                )
                .unwrap(),
            Some(1)
        );
    }

    #[test]
    fn service_rollback_claim_is_single_use_and_completion_reactivates_previous_generation() {
        let mut store = StateStore::open(":memory:").unwrap();
        store
            .stage_runtime_generation("rust-a", "/runtime/a", "sha-a", "install", 10)
            .unwrap();
        store
            .stage_runtime_generation("rust-b", "/runtime/b", "sha-b", "update", 20)
            .unwrap();
        store.activate_runtime_generation("rust-a", 30).unwrap();
        store
            .prepare_service_rollback(&ServiceRollbackRecord {
                rollback_id: "rb-1".to_owned(),
                source_kind: "rust".to_owned(),
                activated_generation_id: "rust-b".to_owned(),
                server_plist_backup: Some("/backups/server.plist".to_owned()),
                watchdog_plist_backup: None,
                previous_current_target: Some("generations/rust-a".to_owned()),
                server_was_loaded: true,
                watchdog_was_loaded: false,
                created_at: 35,
                state: "prepared".to_owned(),
            })
            .unwrap();
        store
            .activate_runtime_generation_with_rollback("rust-b", Some("rb-1"), 40)
            .unwrap();
        assert_eq!(
            store
                .active_runtime_generation()
                .unwrap()
                .unwrap()
                .generation_id,
            "rust-b"
        );

        let claimed = store
            .begin_latest_service_rollback()
            .unwrap()
            .expect("ready rollback");
        assert_eq!(claimed.rollback_id, "rb-1");
        assert_eq!(claimed.state, "consuming");
        assert!(store.begin_latest_service_rollback().unwrap().is_none());

        store
            .complete_service_rollback("rb-1", "rust-b", Some("rust-a"), 50)
            .unwrap();
        assert_eq!(
            store
                .active_runtime_generation()
                .unwrap()
                .unwrap()
                .generation_id,
            "rust-a"
        );
        assert_eq!(
            store
                .scalar_text("SELECT state FROM runtime_generations WHERE generation_id = 'rust-b'")
                .unwrap()
                .as_deref(),
            Some("rolled_back")
        );
        assert_eq!(
            store
                .scalar_text("SELECT state FROM service_rollbacks WHERE rollback_id = 'rb-1'")
                .unwrap()
                .as_deref(),
            Some("consumed")
        );
    }

    #[test]
    fn layered_rust_rollback_reactivates_the_previous_generation_fallback() {
        let mut store = StateStore::open(":memory:").unwrap();
        store
            .stage_runtime_generation("rust-a", "/runtime/a", "sha-a", "node-adoption", 10)
            .unwrap();
        store
            .prepare_service_rollback(&ServiceRollbackRecord {
                rollback_id: "rb-node-a".to_owned(),
                source_kind: "node".to_owned(),
                activated_generation_id: "rust-a".to_owned(),
                server_plist_backup: Some("/backups/node.plist".to_owned()),
                watchdog_plist_backup: Some("/backups/watchdog.plist".to_owned()),
                previous_current_target: None,
                server_was_loaded: true,
                watchdog_was_loaded: true,
                created_at: 20,
                state: "prepared".to_owned(),
            })
            .unwrap();
        store
            .activate_runtime_generation_with_rollback("rust-a", Some("rb-node-a"), 30)
            .unwrap();

        store
            .stage_runtime_generation("rust-b", "/runtime/b", "sha-b", "service-install", 40)
            .unwrap();
        store
            .prepare_service_rollback(&ServiceRollbackRecord {
                rollback_id: "rb-rust-b".to_owned(),
                source_kind: "rust".to_owned(),
                activated_generation_id: "rust-b".to_owned(),
                server_plist_backup: Some("/backups/rust-a.plist".to_owned()),
                watchdog_plist_backup: None,
                previous_current_target: Some("generations/rust-a".to_owned()),
                server_was_loaded: true,
                watchdog_was_loaded: false,
                created_at: 50,
                state: "prepared".to_owned(),
            })
            .unwrap();
        store
            .activate_runtime_generation_with_rollback("rust-b", Some("rb-rust-b"), 60)
            .unwrap();
        assert_eq!(
            store
                .scalar_text("SELECT state FROM service_rollbacks WHERE rollback_id = 'rb-node-a'")
                .unwrap()
                .as_deref(),
            Some("superseded")
        );

        let rust_rollback = store.begin_latest_service_rollback().unwrap().unwrap();
        assert_eq!(rust_rollback.rollback_id, "rb-rust-b");
        store
            .complete_service_rollback("rb-rust-b", "rust-b", Some("rust-a"), 70)
            .unwrap();

        assert_eq!(
            store
                .scalar_text("SELECT state FROM service_rollbacks WHERE rollback_id = 'rb-node-a'")
                .unwrap()
                .as_deref(),
            Some("ready")
        );
        let node_rollback = store.begin_latest_service_rollback().unwrap().unwrap();
        assert_eq!(node_rollback.rollback_id, "rb-node-a");
        assert_eq!(node_rollback.source_kind, "node");
        assert_eq!(node_rollback.activated_generation_id, "rust-a");
    }

    #[test]
    fn failed_post_commit_rollback_can_release_claim_without_consuming_it() {
        let mut store = StateStore::open(":memory:").unwrap();
        store
            .stage_runtime_generation("rust-current", "/runtime/current", "sha", "install", 1)
            .unwrap();
        store
            .prepare_service_rollback(&ServiceRollbackRecord {
                rollback_id: "rb-retry".to_owned(),
                source_kind: "node".to_owned(),
                activated_generation_id: "rust-current".to_owned(),
                server_plist_backup: Some("/backups/node.plist".to_owned()),
                watchdog_plist_backup: Some("/backups/watchdog.plist".to_owned()),
                previous_current_target: None,
                server_was_loaded: true,
                watchdog_was_loaded: true,
                created_at: 2,
                state: "prepared".to_owned(),
            })
            .unwrap();
        store
            .activate_runtime_generation_with_rollback("rust-current", Some("rb-retry"), 3)
            .unwrap();
        let first = store.begin_latest_service_rollback().unwrap().unwrap();
        assert_eq!(first.state, "consuming");
        store
            .finish_service_rollback("rb-retry", "ready", 4)
            .unwrap();
        assert_eq!(
            store
                .scalar_i64(
                    "SELECT COUNT(*) FROM service_rollbacks
                     WHERE rollback_id = 'rb-retry' AND consumed_at IS NULL"
                )
                .unwrap(),
            Some(1)
        );
        let second = store.begin_latest_service_rollback().unwrap().unwrap();
        assert_eq!(second.rollback_id, "rb-retry");
    }

    #[test]
    fn rollback_can_be_peeked_before_exact_post_guardian_claim() {
        let mut store = StateStore::open(":memory:").unwrap();
        let record = |rollback_id: &str, created_at| ServiceRollbackRecord {
            rollback_id: rollback_id.to_owned(),
            source_kind: "rust".to_owned(),
            activated_generation_id: "rust-current".to_owned(),
            server_plist_backup: Some(format!("/backups/{rollback_id}.plist")),
            watchdog_plist_backup: None,
            previous_current_target: Some("generations/rust-known-good".to_owned()),
            server_was_loaded: true,
            watchdog_was_loaded: false,
            created_at,
            state: "prepared".to_owned(),
        };

        for (id, at) in [("rb-old", 1), ("rb-new", 2)] {
            store.prepare_service_rollback(&record(id, at)).unwrap();
            store.mark_service_rollback_ready(id).unwrap();
        }

        let peeked = store.latest_ready_service_rollback().unwrap().unwrap();
        assert_eq!(peeked.rollback_id, "rb-new");
        assert_eq!(peeked.state, "ready");
        assert_eq!(
            store
                .service_rollback_by_id("rb-new")
                .unwrap()
                .unwrap()
                .state,
            "ready",
            "read-only preflight must not consume the rollback before guardian arming"
        );

        let claimed = store.claim_service_rollback("rb-new").unwrap();
        assert_eq!(claimed.rollback_id, "rb-new");
        assert_eq!(claimed.state, "consuming");
        assert!(store.claim_service_rollback("rb-new").is_err());
        assert!(
            store.latest_ready_service_rollback().unwrap().is_none(),
            "claiming the sole current rollback must not resurrect an older superseded rollback"
        );
    }

    #[test]
    fn guardian_recovery_transitions_are_mode_scoped_and_idempotent() {
        let mut store = StateStore::open(":memory:").unwrap();
        let record =
            |rollback_id: &str, activated_generation_id: &str, created_at| ServiceRollbackRecord {
                rollback_id: rollback_id.to_owned(),
                source_kind: "rust".to_owned(),
                activated_generation_id: activated_generation_id.to_owned(),
                server_plist_backup: Some(format!("/backups/{rollback_id}.plist")),
                watchdog_plist_backup: None,
                previous_current_target: Some("generations/rust-known-good".to_owned()),
                server_was_loaded: true,
                watchdog_was_loaded: false,
                created_at,
                state: "prepared".to_owned(),
            };

        store
            .prepare_service_rollback(&record("rb-install", "rust-candidate", 1))
            .unwrap();
        assert_eq!(
            store
                .service_rollback_by_id("rb-install")
                .unwrap()
                .unwrap()
                .state,
            "prepared"
        );
        store
            .recover_service_rollback_after_guardian("rb-install", "install", 2)
            .unwrap();
        store
            .recover_service_rollback_after_guardian("rb-install", "install", 3)
            .unwrap();
        assert_eq!(
            store
                .service_rollback_by_id("rb-install")
                .unwrap()
                .unwrap()
                .state,
            "auto_rolled_back"
        );

        store
            .prepare_service_rollback(&record("rb-rollback", "rust-current", 4))
            .unwrap();
        store.mark_service_rollback_ready("rb-rollback").unwrap();
        let claimed = store.begin_latest_service_rollback().unwrap().unwrap();
        assert_eq!(claimed.rollback_id, "rb-rollback");
        store
            .recover_service_rollback_after_guardian("rb-rollback", "rollback", 5)
            .unwrap();
        assert_eq!(
            store
                .service_rollback_by_id("rb-rollback")
                .unwrap()
                .unwrap()
                .state,
            "ready"
        );
        store
            .recover_service_rollback_after_guardian("rb-rollback", "rollback", 6)
            .unwrap();

        store
            .prepare_service_rollback(&record("rb-committed", "rust-new", 7))
            .unwrap();
        store.mark_service_rollback_ready("rb-committed").unwrap();
        assert!(
            store
                .recover_service_rollback_after_guardian("rb-committed", "install", 8)
                .is_err(),
            "guardian must not roll back a release whose rollback row is already ready"
        );
    }

    #[test]
    fn node_service_rollback_consumes_record_and_leaves_no_active_rust_generation() {
        let mut store = StateStore::open(":memory:").unwrap();
        store
            .stage_runtime_generation(
                "rust-node-cutover",
                "/runtime/new",
                "sha",
                "node-adoption",
                1,
            )
            .unwrap();
        store
            .prepare_service_rollback(&ServiceRollbackRecord {
                rollback_id: "rb-node".to_owned(),
                source_kind: "node".to_owned(),
                activated_generation_id: "rust-node-cutover".to_owned(),
                server_plist_backup: Some("/backups/node.plist".to_owned()),
                watchdog_plist_backup: Some("/backups/watchdog.plist".to_owned()),
                previous_current_target: None,
                server_was_loaded: true,
                watchdog_was_loaded: true,
                created_at: 2,
                state: "prepared".to_owned(),
            })
            .unwrap();
        store
            .activate_runtime_generation_with_rollback("rust-node-cutover", Some("rb-node"), 3)
            .unwrap();
        let claimed = store.begin_latest_service_rollback().unwrap().unwrap();
        assert_eq!(claimed.source_kind, "node");
        store
            .complete_service_rollback("rb-node", "rust-node-cutover", None, 4)
            .unwrap();
        assert!(store.active_runtime_generation().unwrap().is_none());
        assert_eq!(
            store
                .scalar_text(
                    "SELECT state FROM runtime_generations WHERE generation_id = 'rust-node-cutover'"
                )
                .unwrap()
                .as_deref(),
            Some("rolled_back")
        );
        assert_eq!(
            store
                .scalar_text("SELECT state FROM service_rollbacks WHERE rollback_id = 'rb-node'")
                .unwrap()
                .as_deref(),
            Some("consumed")
        );
    }

    #[test]
    fn schema_v2_upgrades_to_generation_tables_without_losing_existing_rows() {
        let path = temp_db_path();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
                .unwrap();
            conn.execute_batch(MIGRATION_V1).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?1, '2')",
                [META_SCHEMA_VERSION],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO operations (op_id, kind, created_at, updated_at)
                 VALUES ('pre-v3-op', 'test', 1, 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO exec_sessions (session_id, pid, started_at, state)
                 VALUES ('pre-v3-exec', 123, 1, 'closed')",
                [],
            )
            .unwrap();
        }

        let store = StateStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(
            store
                .scalar_i64("SELECT COUNT(*) FROM operations WHERE op_id = 'pre-v3-op'")
                .unwrap(),
            Some(1)
        );
        assert_eq!(
            store
                .scalar_i64("SELECT COUNT(*) FROM exec_sessions WHERE session_id = 'pre-v3-exec'")
                .unwrap(),
            Some(1)
        );
        let tables = store.table_names().unwrap();
        assert!(tables.contains(&"runtime_generations".to_owned()));
        assert!(tables.contains(&"service_events".to_owned()));
        drop(store);
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(path.with_extension("sqlite-wal")).ok();
        std::fs::remove_file(path.with_extension("sqlite-shm")).ok();
    }

    #[test]
    fn reopen_is_idempotent() {
        let path = temp_db_path();
        let first = StateStore::open(&path).unwrap();
        assert_eq!(first.schema_version().unwrap(), SCHEMA_VERSION);
        drop(first);
        // Reopen must succeed and keep the same version (no re-migration error).
        let second = StateStore::open(&path).unwrap();
        assert_eq!(second.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(second.journal_mode().unwrap(), "wal");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let store = StateStore::open(":memory:").unwrap();
        assert_eq!(store.foreign_keys().unwrap(), 1);
        store
            .execute_batch(
                "CREATE TABLE op_tag (
                    tag_id INTEGER PRIMARY KEY,
                    op_id TEXT NOT NULL REFERENCES operations(op_id)
                );",
            )
            .unwrap();
        // A row referencing a missing op must be rejected.
        let result = store.execute("INSERT INTO op_tag (tag_id, op_id) VALUES (1, 'missing-op');");
        assert!(result.is_err(), "FK violation should be rejected");
        // A row referencing an existing op must succeed.
        store
            .execute(
                "INSERT INTO operations (op_id, kind, created_at, updated_at)
                 VALUES ('op-1', 'test', 1, 1);",
            )
            .unwrap();
        store
            .execute("INSERT INTO op_tag (tag_id, op_id) VALUES (1, 'op-1');")
            .unwrap();
    }

    #[test]
    fn wal_and_busy_timeout_are_initialized_on_disk() {
        let path = temp_db_path();
        let store = StateStore::open(&path).unwrap();
        assert_eq!(store.journal_mode().unwrap(), "wal");
        assert_eq!(store.busy_timeout().unwrap(), BUSY_TIMEOUT_MS);
        assert_eq!(store.foreign_keys().unwrap(), 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn in_memory_journal_mode_is_memory() {
        let store = StateStore::open(":memory:").unwrap();
        assert_eq!(store.journal_mode().unwrap(), "memory");
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn transaction_commits_work() {
        let mut store = StateStore::open(":memory:").unwrap();
        store
            .transaction(|tx| {
                tx.execute(
                    "INSERT INTO operations (op_id, kind, created_at, updated_at)
                     VALUES ('tx-1', 'test', 1, 1);",
                )?;
                Ok(())
            })
            .unwrap();
        let count = store
            .scalar_i64("SELECT COUNT(*) FROM operations WHERE op_id = 'tx-1'")
            .unwrap()
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn transaction_rolls_back_on_error() {
        let mut store = StateStore::open(":memory:").unwrap();
        let result: Result<i32, String> = store.transaction(|tx| {
            tx.execute(
                "INSERT INTO operations (op_id, kind, created_at, updated_at)
                 VALUES ('op-2', 'test', 1, 1);",
            )?;
            // Force rollback.
            Err("boom".to_owned())
        });
        assert!(result.is_err());
        let count = store
            .scalar_i64("SELECT COUNT(*) FROM operations WHERE op_id = 'op-2'")
            .unwrap()
            .unwrap();
        assert_eq!(count, 0, "rolled-back insert must not be visible");
    }

    #[test]
    fn rollback_via_drop_is_atomic() {
        let mut store = StateStore::open(":memory:").unwrap();
        // A failed ledger insert+second write must stay invisible after rollback.
        store
            .transaction(|tx| {
                tx.execute(
                    "INSERT INTO operations (op_id, kind, created_at, updated_at)
                     VALUES ('op-3', 'test', 1, 1);",
                )?;
                // Second statement is also part of the same tx; both undone.
                tx.execute(
                    "INSERT INTO operations (op_id, kind, created_at, updated_at)
                     VALUES ('op-4', 'test', 1, 1);",
                )?;
                Err::<i32, String>("rollback-now".to_owned())
            })
            .unwrap_err();
        let count = store
            .scalar_i64("SELECT COUNT(*) FROM operations WHERE op_id IN ('op-3','op-4')")
            .unwrap()
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn future_schema_version_fails_closed() {
        let path = temp_db_path();
        let store = StateStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        // Simulate a future/newer store by raising the recorded version.
        store
            .execute(&format!(
                "UPDATE meta SET value = '{}' WHERE key = '{META_SCHEMA_VERSION}'",
                SCHEMA_VERSION + 5
            ))
            .unwrap();
        // Reopening must refuse, never silently downgrade.
        let result = StateStore::open(&path);
        assert!(result.is_err());
        let message = result.err().unwrap();
        assert!(message.contains("newer than this binary supports"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn malformed_schema_version_fails_closed() {
        let path = temp_db_path();
        let store = StateStore::open(&path).unwrap();
        store
            .execute(&format!(
                "UPDATE meta SET value = 'not-a-version' WHERE key = '{META_SCHEMA_VERSION}'"
            ))
            .unwrap();
        drop(store);

        let result = StateStore::open(&path);
        assert!(result.is_err());
        let message = result.err().unwrap();
        assert!(message.contains("invalid meta schema_version"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn open_creates_parent_directory() {
        let dir = temp_dir().join("nested").join("deeper");
        let path = dir.join("state.sqlite");
        let store = StateStore::open(&path).unwrap();
        assert!(path.exists());
        assert!(store.schema_version().unwrap() >= 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "parent dir should be 0o700");
            let file_mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(file_mode & 0o777, 0o600, "db file should be 0o600");
        }
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn open_in_dir_creates_dir_and_secure_file() {
        let dir = temp_dir();
        let store = StateStore::open_in_dir(&dir, "state").unwrap();
        assert!(store.path().unwrap().ends_with("state.db"));
        assert!(store.schema_version().unwrap() >= 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_in_dir_rejects_db_name_path_escape() {
        let dir = temp_dir();
        for name in ["", ".", "..", "../outside.db", "nested/state.db"] {
            let error = StateStore::open_in_dir(&dir, name).unwrap_err();
            assert!(
                error.contains("one file name"),
                "unexpected error for {name:?}: {error}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn continuity_turns_are_idempotent_resumable_and_ambiguity_fails_closed() {
        let mut store = StateStore::open(":memory:").unwrap();
        assert_eq!(store.schema_version().unwrap(), 5);
        assert!(
            store
                .append_continuity_turn(ContinuityTurnInput {
                    continuity_id: "hc:alpha",
                    conversation_id: "conv-a",
                    workspace_id: Some("w19"),
                    project_id: Some("project-a"),
                    title: Some("Continuity alpha"),
                    message_id: "msg-user-1",
                    role: "user",
                    text: "continue the work",
                    fingerprint: Some("fp-user-1"),
                    observed_at: 100,
                })
                .unwrap()
        );
        assert!(
            !store
                .append_continuity_turn(ContinuityTurnInput {
                    continuity_id: "hc:alpha",
                    conversation_id: "conv-a",
                    workspace_id: Some("w19"),
                    project_id: Some("project-a"),
                    title: Some("Continuity alpha"),
                    message_id: "msg-user-1",
                    role: "user",
                    text: "duplicate payload is ignored",
                    fingerprint: Some("fp-user-1"),
                    observed_at: 101,
                })
                .unwrap()
        );
        assert!(
            store
                .append_continuity_turn(ContinuityTurnInput {
                    continuity_id: "hc:alpha",
                    conversation_id: "conv-a",
                    workspace_id: Some("w19"),
                    project_id: Some("project-a"),
                    title: None,
                    message_id: "msg-assistant-1",
                    role: "assistant",
                    text: "work completed",
                    fingerprint: None,
                    observed_at: 102,
                })
                .unwrap()
        );

        let resume = store.continuity_resume("hc:alpha", 32).unwrap().unwrap();
        assert_eq!(resume.continuity_id, "hc:alpha");
        assert_eq!(resume.turns.len(), 2);
        assert_eq!(resume.turns[0].message_id, "msg-user-1");
        assert_eq!(resume.turns[1].message_id, "msg-assistant-1");
        assert_eq!(
            store.continuity_for_conversation("conv-a").unwrap(),
            Some("hc:alpha".to_owned())
        );

        store
            .append_continuity_turn(ContinuityTurnInput {
                continuity_id: "hc:beta",
                conversation_id: "conv-a",
                workspace_id: Some("w20"),
                project_id: Some("project-a"),
                title: Some("Continuity beta"),
                message_id: "msg-user-2",
                role: "user",
                text: "other chain",
                fingerprint: None,
                observed_at: 103,
            })
            .unwrap();
        assert_eq!(
            store.continuity_for_conversation("conv-a").unwrap_err(),
            "continuity_binding_ambiguous"
        );
        assert_eq!(store.continuity_candidates(10).unwrap().len(), 2);
    }

    #[test]
    fn continuity_search_filters_identity_and_returns_bounded_confirmation_evidence() {
        let mut store = StateStore::open(":memory:").unwrap();
        let rows = [
            (
                "hc:alpha",
                "conv-a",
                "w19",
                "project-a",
                "Alpha release work",
                "msg-a-user",
                "user",
                "continue the v0.4.2 release qualification and continuity search",
                100,
            ),
            (
                "hc:alpha",
                "conv-a",
                "w19",
                "project-a",
                "Alpha release work",
                "msg-a-assistant",
                "assistant",
                "release gate is green; continuity search still needs confirmation coverage",
                101,
            ),
            (
                "hc:beta",
                "conv-b",
                "w20",
                "project-b",
                "Beta provider work",
                "msg-b-user",
                "user",
                "continue provider migration",
                200,
            ),
        ];
        for (
            continuity_id,
            conversation_id,
            workspace_id,
            project_id,
            title,
            message_id,
            role,
            text,
            observed_at,
        ) in rows
        {
            store
                .append_continuity_turn(ContinuityTurnInput {
                    continuity_id,
                    conversation_id,
                    workspace_id: Some(workspace_id),
                    project_id: Some(project_id),
                    title: Some(title),
                    message_id,
                    role,
                    text,
                    fingerprint: None,
                    observed_at,
                })
                .unwrap();
        }

        let by_workspace = store
            .continuity_search(ContinuitySearchInput {
                workspace_id: Some("w19"),
                limit: 5,
                ..ContinuitySearchInput::default()
            })
            .unwrap();
        assert_eq!(by_workspace.len(), 1);
        assert_eq!(by_workspace[0].continuity_id, "hc:alpha");
        assert_eq!(by_workspace[0].workspace_ids, vec!["w19"]);
        assert!(
            by_workspace[0]
                .recent_user_excerpt
                .as_deref()
                .unwrap()
                .contains("continuity search")
        );
        assert!(
            by_workspace[0]
                .recent_assistant_excerpt
                .as_deref()
                .unwrap()
                .contains("confirmation coverage")
        );

        let by_project = store
            .continuity_search(ContinuitySearchInput {
                project_id: Some("project-a"),
                limit: 5,
                ..ContinuitySearchInput::default()
            })
            .unwrap();
        assert_eq!(by_project.len(), 1);
        assert_eq!(by_project[0].continuity_id, "hc:alpha");

        let by_conversation = store
            .continuity_search(ContinuitySearchInput {
                conversation_id: Some("conv-b"),
                limit: 5,
                ..ContinuitySearchInput::default()
            })
            .unwrap();
        assert_eq!(by_conversation.len(), 1);
        assert_eq!(by_conversation[0].continuity_id, "hc:beta");

        let by_text = store
            .continuity_search(ContinuitySearchInput {
                query: Some("qualification"),
                limit: 5,
                ..ContinuitySearchInput::default()
            })
            .unwrap();
        assert_eq!(by_text.len(), 1);
        assert_eq!(by_text[0].continuity_id, "hc:alpha");

        let all = store
            .continuity_search(ContinuitySearchInput {
                limit: 5,
                ..ContinuitySearchInput::default()
            })
            .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].continuity_id, "hc:beta");
        assert_eq!(all[1].continuity_id, "hc:alpha");

        let long = "x".repeat(300);
        assert_eq!(bounded_continuity_excerpt(&long).chars().count(), 241);
    }

    #[cfg(unix)]
    #[test]
    fn open_in_dir_tightens_existing_dir_and_rejects_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let store = StateStore::open_in_dir(&dir, "state.db").unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(store);

        let real = dir.join("real.sqlite");
        std::fs::write(&real, b"").unwrap();
        let link = dir.join("linked.sqlite");
        symlink(&real, &link).unwrap();
        let error = StateStore::open(&link).unwrap_err();
        assert!(error.contains("symlink"));

        let real_dir = temp_dir();
        std::fs::create_dir_all(&real_dir).unwrap();
        let linked_dir = temp_dir();
        symlink(&real_dir, &linked_dir).unwrap();
        let error = StateStore::open_in_dir(&linked_dir, "state.db").unwrap_err();
        assert!(error.contains("symlink"));

        std::fs::remove_file(&linked_dir).ok();
        std::fs::remove_dir_all(&real_dir).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn closed_exec_sessions_query_settled_records() {
        let store = StateStore::open(":memory:").unwrap();
        store
            .record_exec_running("es_running", 100, Some(100), 1000)
            .unwrap();
        store
            .record_exec_running("es_closed_a", 101, Some(101), 1100)
            .unwrap();
        store
            .settle_exec_session("es_closed_a", "closed", Some(1150), Some(0), None, 5000)
            .unwrap();
        store
            .record_exec_running("es_closed_b", 102, Some(102), 1200)
            .unwrap();
        store
            .settle_exec_session(
                "es_closed_b",
                "closed",
                Some(1250),
                Some(1),
                Some("SIGTERM"),
                2000,
            )
            .unwrap();

        let single = store
            .get_closed_exec_session("es_closed_a")
            .unwrap()
            .unwrap();
        assert_eq!(single.session_id, "es_closed_a");
        assert_eq!(single.started_at_ms, 1100);
        assert_eq!(single.ended_at_ms, Some(1150));
        assert_eq!(single.exit_code, Some(0));
        assert_eq!(single.signal, None);
        assert_eq!(single.expires_at_ms, Some(5000));

        assert!(
            store
                .get_closed_exec_session("es_running")
                .unwrap()
                .is_none()
        );

        // At t=1500, both closed sessions are unexpired
        let unexpired = store.closed_exec_sessions(1500, 10).unwrap();
        assert_eq!(unexpired.len(), 2);
        assert_eq!(unexpired[0].session_id, "es_closed_a");
        assert_eq!(unexpired[1].session_id, "es_closed_b");

        // At t=3000, es_closed_b (expires_at=2000) is filtered out
        let filtered = store.closed_exec_sessions(3000, 10).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].session_id, "es_closed_a");
    }
}
