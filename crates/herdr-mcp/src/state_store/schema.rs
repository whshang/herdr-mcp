//! Schema, migration, connection, and file-security owner for the durable local
//! state store.
//!
//! This module is the single SQLite authority behind [`super::StateStore`]: it
//! owns the ordered migration list, the additive browser-archive auxiliary
//! schema, connection setup, the fail-closed migration runner, and the
//! file/permission helpers that keep the on-disk database private.
//!
//! `SCHEMA_VERSION` and `BUSY_TIMEOUT_MS` intentionally stay in the parent
//! module (`state_store.rs`) because release tooling and CI parse that file as
//! the source location of the durable state schema version; this module reads
//! them through `super::`.

use super::{BUSY_TIMEOUT_MS, SCHEMA_VERSION};
use rusqlite::{Connection, OptionalExtension};
use std::fs::OpenOptions;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// Meta-table key holding the applied schema version. Stored as a string.
pub(super) const META_SCHEMA_VERSION: &str = "schema_version";

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
pub(super) const MIGRATION_V1: &str = r#"
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
pub(super) const MIGRATION_V2: &str = r#"
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

/// Migration 6: project-level Work Memory on the existing Continuity Journal.
/// Full raw turns/evidence remain local; FTS5 is an index over the same DB.
const MIGRATION_V6: &str = r#"
ALTER TABLE continuity_chains ADD COLUMN project_ref TEXT;
ALTER TABLE continuity_chains ADD COLUMN repo_id TEXT;
ALTER TABLE continuity_chains ADD COLUMN work_chain_id TEXT;
ALTER TABLE continuity_chains ADD COLUMN checkpoint_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE continuity_chains ADD COLUMN retention_policy TEXT NOT NULL DEFAULT 'retain_all';

CREATE UNIQUE INDEX IF NOT EXISTS idx_continuity_chains_work_partition
    ON continuity_chains(project_ref, repo_id, work_chain_id)
    WHERE project_ref IS NOT NULL AND repo_id IS NOT NULL AND work_chain_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS continuity_provider_bindings (
    continuity_id TEXT NOT NULL,
    provider      TEXT NOT NULL,
    account_ref   TEXT NOT NULL DEFAULT '',
    space_ref     TEXT NOT NULL DEFAULT '',
    session_ref   TEXT NOT NULL,
    bound_at      INTEGER NOT NULL,
    PRIMARY KEY (continuity_id, provider, account_ref, space_ref, session_ref),
    FOREIGN KEY (continuity_id) REFERENCES continuity_chains(continuity_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_continuity_provider_session
    ON continuity_provider_bindings(provider, account_ref, space_ref, session_ref);

ALTER TABLE continuity_turns ADD COLUMN provider TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE continuity_turns ADD COLUMN account_ref TEXT NOT NULL DEFAULT '';
ALTER TABLE continuity_turns ADD COLUMN space_ref TEXT NOT NULL DEFAULT '';
ALTER TABLE continuity_turns ADD COLUMN provider_session_ref TEXT NOT NULL DEFAULT '';
ALTER TABLE continuity_turns ADD COLUMN provider_message_ref TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_continuity_turns_provider_message
    ON continuity_turns(
        continuity_id, provider, account_ref, space_ref,
        provider_session_ref, provider_message_ref
    ) WHERE provider_message_ref IS NOT NULL;

CREATE TABLE IF NOT EXISTS continuity_evidence (
    continuity_id TEXT NOT NULL,
    evidence_id   TEXT NOT NULL,
    kind          TEXT NOT NULL,
    content       TEXT NOT NULL,
    sha256        TEXT NOT NULL,
    provider      TEXT,
    account_ref   TEXT,
    space_ref     TEXT,
    session_ref   TEXT,
    portable_repo_id TEXT,
    portable_commit_sha TEXT,
    portable_path TEXT,
    portable_line_start INTEGER,
    portable_line_end INTEGER,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (continuity_id, evidence_id),
    FOREIGN KEY (continuity_id) REFERENCES continuity_chains(continuity_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_continuity_evidence_order
    ON continuity_evidence(continuity_id, created_at DESC);

ALTER TABLE continuity_checkpoints ADD COLUMN checkpoint_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE continuity_checkpoints ADD COLUMN checkpoint_json TEXT;
ALTER TABLE continuity_checkpoints ADD COLUMN checkpoint_sha256 TEXT;
ALTER TABLE continuity_checkpoints ADD COLUMN verified INTEGER NOT NULL DEFAULT 0;
ALTER TABLE continuity_checkpoints ADD COLUMN through_evidence_id TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_continuity_checkpoints_revision
    ON continuity_checkpoints(continuity_id, checkpoint_revision)
    WHERE checkpoint_revision > 0;

CREATE VIRTUAL TABLE IF NOT EXISTS continuity_memory_fts USING fts5(
    continuity_id UNINDEXED,
    source_kind UNINDEXED,
    source_id UNINDEXED,
    text
);
INSERT INTO continuity_memory_fts(continuity_id, source_kind, source_id, text)
    SELECT continuity_id, 'turn', message_id, text FROM continuity_turns;
"#;

/// Migration 7: provider-neutral Browser Endpoint and Resource Registry.
///
/// Browser identity stays local to the existing state store. The raw browser
/// profile seed and raw provider-native resource ids are intentionally absent:
/// callers pass them only to trusted local registration APIs, which persist
/// opaque refs/digests. Browser resource reservation/failover is a later
/// milestone and therefore has no table in this migration.
const MIGRATION_V7: &str = r#"
CREATE TABLE IF NOT EXISTS browser_endpoints (
    endpoint_ref                    TEXT PRIMARY KEY NOT NULL,
    device_id                       TEXT NOT NULL,
    browser_family                  TEXT NOT NULL,
    extension_version               TEXT NOT NULL,
    webchat_control_allowed         INTEGER NOT NULL DEFAULT 0,
    tool_bridge_allowed             INTEGER NOT NULL DEFAULT 0,
    tool_bridge_mutation_allowed    INTEGER NOT NULL DEFAULT 0,
    consent_revision                INTEGER NOT NULL DEFAULT 0,
    first_observed_at               INTEGER NOT NULL,
    last_observed_at                INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_browser_endpoints_device
    ON browser_endpoints(device_id, last_observed_at DESC);

CREATE TABLE IF NOT EXISTS browser_provider_state (
    endpoint_ref                TEXT NOT NULL,
    provider                    TEXT NOT NULL,
    adapter_protocol_version    INTEGER NOT NULL,
    observation_generation      INTEGER NOT NULL,
    capabilities_json           TEXT NOT NULL,
    observed_at                 INTEGER NOT NULL,
    PRIMARY KEY (endpoint_ref, provider),
    FOREIGN KEY (endpoint_ref) REFERENCES browser_endpoints(endpoint_ref) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS browser_resources (
    resource_ref                TEXT PRIMARY KEY NOT NULL,
    endpoint_ref                TEXT NOT NULL,
    provider                    TEXT NOT NULL,
    kind                        TEXT NOT NULL,
    parent_ref                  TEXT NOT NULL DEFAULT '',
    native_identity_sha256      TEXT NOT NULL,
    display_label               TEXT,
    observation_generation      INTEGER NOT NULL,
    first_observed_at           INTEGER NOT NULL,
    last_observed_at            INTEGER NOT NULL,
    FOREIGN KEY (endpoint_ref) REFERENCES browser_endpoints(endpoint_ref) ON DELETE CASCADE,
    UNIQUE(endpoint_ref, provider, kind, parent_ref, native_identity_sha256)
);
CREATE INDEX IF NOT EXISTS idx_browser_resources_lookup
    ON browser_resources(endpoint_ref, provider, kind, parent_ref, last_observed_at DESC);
"#;

/// Migration 8: minimum durable browser-dispatch identity and idempotency.
///
/// The row stores only provider-neutral refs, semantic intent, and caller-
/// supplied digests. Delivery history and late settlement belong to beta.1.
const MIGRATION_V8: &str = r#"
CREATE TABLE IF NOT EXISTS browser_dispatches (
    dispatch_id           TEXT PRIMARY KEY NOT NULL,
    endpoint_ref           TEXT NOT NULL,
    provider               TEXT NOT NULL,
    operation              TEXT NOT NULL,
    target_session_ref     TEXT NOT NULL,
    request_digest         TEXT NOT NULL,
    message_digest         TEXT NOT NULL,
    reasoning_effort       TEXT,
    required_apps_json     TEXT NOT NULL,
    expected_generation    INTEGER NOT NULL,
    idempotency_key_digest TEXT NOT NULL,
    delivery_state         TEXT NOT NULL,
    generation_owner       INTEGER,
    work_chain_id          TEXT,
    lane_id                TEXT,
    created_at             INTEGER NOT NULL,
    updated_at             INTEGER NOT NULL,
    FOREIGN KEY (endpoint_ref) REFERENCES browser_endpoints(endpoint_ref) ON DELETE CASCADE,
    FOREIGN KEY (target_session_ref) REFERENCES browser_resources(resource_ref),
    CHECK (expected_generation > 0),
    CHECK (generation_owner IS NULL OR generation_owner = expected_generation),
    CHECK (delivery_state IN (
        'not_applied', 'applied', 'uncertain', 'rejected',
        'browser_offline', 'resource_unavailable', 'stopped'
    ))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_browser_dispatches_idempotency
    ON browser_dispatches(endpoint_ref, provider, operation, idempotency_key_digest);
"#;

/// Migration 9: local-only browser lifecycle locators and pending session
/// reservations for beta.2 WebChat orchestration.
///
/// Browser Registry resource identity remains unchanged: canonical URLs are
/// explicitly disposable local locators, not provider identity. A pending
/// session reservation exists only until the provider exposes a stable native
/// session identity after the first real assignment is submitted.
const MIGRATION_V9: &str = r#"
CREATE TABLE IF NOT EXISTS browser_resource_locators (
    resource_ref            TEXT PRIMARY KEY NOT NULL,
    canonical_url           TEXT NOT NULL,
    observation_generation  INTEGER NOT NULL,
    observed_at             INTEGER NOT NULL,
    FOREIGN KEY (resource_ref) REFERENCES browser_resources(resource_ref) ON DELETE CASCADE,
    CHECK (observation_generation > 0)
);

CREATE TABLE IF NOT EXISTS browser_session_reservations (
    reservation_ref         TEXT PRIMARY KEY NOT NULL,
    endpoint_ref            TEXT NOT NULL,
    provider                TEXT NOT NULL,
    account_ref             TEXT NOT NULL,
    space_ref               TEXT,
    display_label           TEXT NOT NULL,
    expected_generation     INTEGER NOT NULL,
    idempotency_key_digest  TEXT NOT NULL,
    state                   TEXT NOT NULL,
    session_ref             TEXT,
    created_at              INTEGER NOT NULL,
    updated_at              INTEGER NOT NULL,
    expires_at              INTEGER NOT NULL,
    FOREIGN KEY (endpoint_ref) REFERENCES browser_endpoints(endpoint_ref) ON DELETE CASCADE,
    FOREIGN KEY (account_ref) REFERENCES browser_resources(resource_ref),
    FOREIGN KEY (space_ref) REFERENCES browser_resources(resource_ref),
    FOREIGN KEY (session_ref) REFERENCES browser_resources(resource_ref),
    CHECK (expected_generation > 0),
    CHECK (expires_at > created_at),
    CHECK (state IN ('pending', 'materialized', 'cancelled', 'expired')),
    CHECK ((state = 'materialized' AND session_ref IS NOT NULL)
        OR (state != 'materialized' AND session_ref IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_browser_session_reservations_idempotency
    ON browser_session_reservations(endpoint_ref, provider, idempotency_key_digest);
"#;

/// Migration 10: durable parent linkage for control operations against a
/// previously submitted browser dispatch. This lets a stop attempt survive a
/// lost response without overwriting the original submit state while the stop
/// outcome is still uncertain.
const MIGRATION_V10: &str = r#"
ALTER TABLE browser_dispatches ADD COLUMN parent_dispatch_id TEXT
    REFERENCES browser_dispatches(dispatch_id);
CREATE INDEX IF NOT EXISTS idx_browser_dispatches_parent
    ON browser_dispatches(parent_dispatch_id, operation, updated_at DESC);
"#;

/// Migration 11: bind one pending browser-session reservation to the exact
/// session-create request so an idempotency key cannot be replayed with a
/// different first assignment after browser delivery becomes uncertain.
const MIGRATION_V11: &str = r#"
ALTER TABLE browser_session_reservations ADD COLUMN request_digest TEXT;
ALTER TABLE browser_session_reservations ADD COLUMN delivery_state TEXT NOT NULL DEFAULT 'not_applied';
"#;

/// Migration 12: durable browser dispatch result settlement linkage.
///
/// The assistant worker result keeps living in Work Memory; only the exact
/// settled-assistant identity and the durable Work Memory turn/evidence
/// linkage are added to the existing dispatch row so `browser_dispatch.status`
/// can project settlement without a second result database. The reservation
/// keeps one nullable accepted-user-message ref so a crash between provider
/// acceptance and synthesized dispatch creation cannot lose the exact submit
/// identity that result settlement must match.
const MIGRATION_V12: &str = r#"
ALTER TABLE browser_dispatches ADD COLUMN accepted_user_message_ref TEXT;
ALTER TABLE browser_dispatches ADD COLUMN result_assistant_message_ref TEXT;
ALTER TABLE browser_dispatches ADD COLUMN result_turn_message_id TEXT;
ALTER TABLE browser_dispatches ADD COLUMN result_evidence_id TEXT;
ALTER TABLE browser_dispatches ADD COLUMN result_settled_at INTEGER;
ALTER TABLE browser_session_reservations ADD COLUMN accepted_user_message_ref TEXT;
CREATE INDEX IF NOT EXISTS idx_browser_dispatches_result_lookup
    ON browser_dispatches(provider, target_session_ref, accepted_user_message_ref)
    WHERE accepted_user_message_ref IS NOT NULL;
"#;

/// Migration 13: bind each durable browser dispatch attempt to the exact
/// Connector grant identity admitted by Edge. Legacy/operator dispatches keep
/// these nullable; new Connector traffic supplies all three fields together.
const MIGRATION_V13: &str = r#"
ALTER TABLE browser_dispatches ADD COLUMN authorization_principal_ref TEXT;
ALTER TABLE browser_dispatches ADD COLUMN authorization_connector_id TEXT;
ALTER TABLE browser_dispatches ADD COLUMN authorization_grant_generation INTEGER;
CREATE INDEX IF NOT EXISTS idx_browser_dispatches_authorization
    ON browser_dispatches(authorization_connector_id, authorization_grant_generation, updated_at DESC)
    WHERE authorization_connector_id IS NOT NULL;
"#;

/// Migration 14: durable current-conversation source-turn identity.
///
/// A browser session keeps exactly one row: the most recent user turn observed
/// by the trusted Extension IPC. Only the SHA-256 of the trimmed user text is
/// stored; the plaintext turn is never persisted. `observed_at` drives the 24h
/// retention window, and a late-arriving older observation must not overwrite a
/// newer one (`observe_browser_source_turn` enforces that in SQL).
const MIGRATION_V14: &str = r#"
CREATE TABLE IF NOT EXISTS browser_source_turns (
    session_ref         TEXT PRIMARY KEY NOT NULL,
    expected_generation INTEGER NOT NULL,
    user_text_sha256    TEXT NOT NULL,
    observed_at         INTEGER NOT NULL,
    CHECK (expected_generation > 0),
    CHECK (observed_at >= 0)
);
CREATE INDEX IF NOT EXISTS idx_browser_source_turns_hash
    ON browser_source_turns(user_text_sha256);
"#;

/// Migration 15: bind ChatGPT's opaque MCP caller-session identity to the
/// exact browser session after the two surfaces have been correlated once.
/// Raw host identifiers are never persisted; only their SHA-256 digest is
/// stored, scoped by Connector principal and provider.
const MIGRATION_V15: &str = r#"
CREATE TABLE IF NOT EXISTS browser_caller_sessions (
    principal_ref         TEXT NOT NULL,
    provider              TEXT NOT NULL,
    caller_session_sha256 TEXT NOT NULL,
    session_ref           TEXT NOT NULL,
    first_bound_at        INTEGER NOT NULL,
    last_bound_at         INTEGER NOT NULL,
    PRIMARY KEY (principal_ref, provider, caller_session_sha256),
    FOREIGN KEY (session_ref) REFERENCES browser_resources(resource_ref) ON DELETE CASCADE,
    CHECK (first_bound_at >= 0),
    CHECK (last_bound_at >= first_bound_at)
);
CREATE INDEX IF NOT EXISTS idx_browser_caller_sessions_resource
    ON browser_caller_sessions(session_ref);
"#;

/// Forward-additive durable deferred ChatGPT self-archive lifecycle.
///
/// This table is intentionally *not* a numbered schema migration. It is an
/// additive auxiliary table that references existing schema-15 tables without
/// altering them, so a pinned schema-15 PROD binary can still open the same DB
/// after DEV has exercised this feature. That rollback property is required by
/// `herdr-mcp dev rollback`; the older binary safely ignores this table.
const BROWSER_ARCHIVE_AUXILIARY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS browser_session_archive_intents (
    archive_ref              TEXT PRIMARY KEY NOT NULL,
    endpoint_ref             TEXT NOT NULL,
    provider                 TEXT NOT NULL,
    session_ref              TEXT NOT NULL,
    source_user_text_sha256  TEXT NOT NULL,
    request_digest           TEXT NOT NULL,
    idempotency_key_digest   TEXT NOT NULL,
    state                    TEXT NOT NULL,
    requested_generation     INTEGER NOT NULL,
    execution_generation     INTEGER,
    claim_attempt            INTEGER NOT NULL DEFAULT 0,
    created_at               INTEGER NOT NULL,
    updated_at               INTEGER NOT NULL,
    claimed_at               INTEGER,
    completed_at             INTEGER,
    FOREIGN KEY (endpoint_ref) REFERENCES browser_endpoints(endpoint_ref) ON DELETE CASCADE,
    FOREIGN KEY (session_ref) REFERENCES browser_resources(resource_ref) ON DELETE CASCADE,
    CHECK (requested_generation > 0),
    CHECK (execution_generation IS NULL OR execution_generation > 0),
    CHECK (claim_attempt >= 0),
    CHECK (state IN ('pending', 'claimed', 'applied', 'uncertain'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_browser_session_archive_intents_idempotency
    ON browser_session_archive_intents(endpoint_ref, provider, idempotency_key_digest);
CREATE UNIQUE INDEX IF NOT EXISTS idx_browser_session_archive_intents_turn
    ON browser_session_archive_intents(session_ref, source_user_text_sha256);
CREATE INDEX IF NOT EXISTS idx_browser_session_archive_intents_pending
    ON browser_session_archive_intents(session_ref, state, updated_at DESC);
"#;

/// Ordered, append-only migration list. Index `i` (0-based) upgrades from
/// version `i` to version `i + 1`.
pub(super) const MIGRATIONS: &[&str] = &[
    MIGRATION_V1,
    MIGRATION_V2,
    MIGRATION_V3,
    MIGRATION_V4,
    MIGRATION_V5,
    MIGRATION_V6,
    MIGRATION_V7,
    MIGRATION_V8,
    MIGRATION_V9,
    MIGRATION_V10,
    MIGRATION_V11,
    MIGRATION_V12,
    MIGRATION_V13,
    MIGRATION_V14,
    MIGRATION_V15,
];

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

pub(super) fn open_connection(path: Option<&Path>) -> Result<Connection, String> {
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
pub(super) fn migrate(conn: &mut Connection) -> Result<(), String> {
    migrate_to(conn, SCHEMA_VERSION, MIGRATIONS)
}

pub(super) fn ensure_browser_archive_auxiliary_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(BROWSER_ARCHIVE_AUXILIARY_SCHEMA)
        .map_err(|error| format!("cannot ensure browser archive auxiliary schema: {error}"))?;
    let has_claim_attempt = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('browser_session_archive_intents') WHERE name = 'claim_attempt'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("cannot inspect browser archive auxiliary schema: {error}"))?;
    if has_claim_attempt == 0 {
        conn.execute(
            "ALTER TABLE browser_session_archive_intents ADD COLUMN claim_attempt INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|error| format!("cannot extend browser archive auxiliary schema: {error}"))?;
    }
    Ok(())
}

pub(super) fn migrate_to(
    conn: &mut Connection,
    target: i64,
    migrations: &[&str],
) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .map_err(|error| format!("cannot bootstrap meta table: {error}"))?;

    let current = i64_schema_version(conn)?;
    if current > target {
        return Err(format!(
            "state store schema version {current} is newer than this binary supports \
             ({target}); refusing to run (fail-closed, no silent downgrade); rollback \
             requires a schema-{target} database backup or a schema-{current}-capable binary"
        ));
    }

    for (index, migration) in migrations.iter().enumerate() {
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

pub(super) fn i64_schema_version(conn: &Connection) -> Result<i64, String> {
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

pub(super) fn reject_symlink(path: &Path, label: &str) -> Result<(), String> {
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

pub(super) fn db_file_name(db_name: &str) -> Result<String, String> {
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

pub(super) fn prepare_db_file(path: &Path) -> Result<(), String> {
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
pub(super) fn ensure_parent_dirs(path: &Path) -> Result<(), String> {
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
pub(super) fn secure_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure state dir {}: {error}", dir.display()))
}

#[cfg(unix)]
pub(super) fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot set state store mode on {}: {error}", path.display()))
}
