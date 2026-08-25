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
//! The store only guarantees a well-formed, versioned SQLite file plus a small
//! transactional API. Consumers wire real records in later migrations.

// Foundation layer with no live consumer yet: nothing in the binary wires it up
// until a specific feature (operation ledger / exec-session persistence) is
// built on top. It is fully exercised by unit tests, so dead_code here is
// expected and intentional, not a leak.
#![allow(dead_code)]

use rusqlite::{Connection, OptionalExtension};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// How long a writer waits on a locked database before erroring.
pub const BUSY_TIMEOUT_MS: i64 = 5_000;

/// Max migration version the current binary understands.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

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

/// Ordered, append-only migration list. Index `i` (0-based) upgrades from
/// version `i` to version `i + 1`.
const MIGRATIONS: &[&str] = &[MIGRATION_V1];

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
        let path = temp_db_path();
        let store = StateStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let tables = store.table_names().unwrap();
        for table in ["meta", "operations", "exec_sessions"] {
            assert!(tables.contains(&table.to_owned()), "missing table {table}");
        }
        let indexes = store.index_names().unwrap();
        for index in [
            "idx_operations_idempotency",
            "idx_operations_expires",
            "idx_exec_sessions_expires",
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
}
