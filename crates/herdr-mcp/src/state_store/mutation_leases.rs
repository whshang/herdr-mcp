//! Durable single-writer mutation leases with monotonic fencing tokens and
//! unpredictable capability tokens (#920 / #923 / #332).
//!
//! Uses a forward-additive auxiliary table (`mutation_leases`) so `SCHEMA_VERSION`
//! remains unchanged and rollback to pinned v1.1.x binaries stays 100% compatible.

use super::StateStore;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

pub const DEFAULT_MUTATION_LEASE_LANE_KEY: &str = "*";
pub const MIN_MUTATION_LEASE_TTL_SECONDS: u64 = 10;
pub const MAX_MUTATION_LEASE_TTL_SECONDS: u64 = 3600;
pub const DEFAULT_MUTATION_LEASE_TTL_SECONDS: u64 = 300;
pub const MAX_MUTATION_LEASE_ID_BYTES: usize = 256;
pub const MAX_MUTATION_LEASE_TOKEN_BYTES: usize = 256;
pub const MAX_MUTATION_LEASE_PATH_BYTES: usize = 4096;

const MUTATION_LEASES_AUXILIARY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mutation_leases (
    project_root       TEXT NOT NULL,
    lane_key           TEXT NOT NULL DEFAULT '*',
    holder_id          TEXT NOT NULL,
    lease_epoch        INTEGER NOT NULL,
    workspace_id       TEXT,
    pane_id            TEXT,
    branch             TEXT,
    acquired_at        INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL,
    expires_at         INTEGER NOT NULL,
    released_at        INTEGER,
    lease_token_sha256 TEXT,
    device_id          TEXT,
    intent             TEXT,
    readers_json       TEXT,
    PRIMARY KEY (project_root, lane_key),
    CHECK (lease_epoch >= 1)
);
CREATE INDEX IF NOT EXISTS idx_mutation_leases_active
    ON mutation_leases(project_root, expires_at)
    WHERE released_at IS NULL;
"#;

pub(super) fn ensure_mutation_leases_auxiliary_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(MUTATION_LEASES_AUXILIARY_SCHEMA)
        .map_err(|error| format!("cannot ensure mutation leases auxiliary schema: {error}"))?;
    for column in ["lease_token_sha256", "device_id", "intent", "readers_json"] {
        let has_column: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('mutation_leases') WHERE name = ?1",
                [column],
                |row| row.get(0),
            )
            .map_err(|error| format!("cannot inspect mutation_leases auxiliary schema: {error}"))?;
        if has_column == 0 {
            let sql = format!("ALTER TABLE mutation_leases ADD COLUMN {column} TEXT");
            match conn.execute(&sql, []) {
                Ok(_) => {}
                Err(error) if error.to_string().contains("duplicate column name") => {}
                Err(error) => {
                    return Err(format!(
                        "cannot extend mutation_leases auxiliary schema: {error}"
                    ));
                }
            }
        }
    }
    // Older schema-16 writers do not know device_id and update an existing
    // row in place when they advance the epoch. Clear the old attribution so
    // a later new runtime never reports a previous owner's device as theirs.
    // New writers restore their explicitly supplied device in the same update.
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS mutation_leases_clear_old_device_on_epoch
         AFTER UPDATE OF lease_epoch ON mutation_leases
         WHEN NEW.lease_epoch != OLD.lease_epoch AND NEW.device_id IS OLD.device_id
         BEGIN
           UPDATE mutation_leases SET device_id = NULL
            WHERE project_root = NEW.project_root AND lane_key = NEW.lane_key;
         END;",
    )
    .map_err(|error| format!("cannot ensure mutation lease device safety trigger: {error}"))?;
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS mutation_leases_clear_old_readers_on_epoch
         AFTER UPDATE OF lease_epoch ON mutation_leases
         WHEN NEW.lease_epoch != OLD.lease_epoch AND NEW.intent IS OLD.intent
              AND NEW.readers_json IS OLD.readers_json
         BEGIN
           UPDATE mutation_leases SET intent = NULL, readers_json = NULL
            WHERE project_root = NEW.project_root AND lane_key = NEW.lane_key;
         END;",
    )
    .map_err(|error| format!("cannot ensure mutation lease intent safety trigger: {error}"))?;
    Ok(())
}

pub fn sha256_hex(input: &str) -> String {
    format!("{:x}", Sha256::digest(input.as_bytes()))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&a, &b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn generate_lease_token() -> Result<(String, String), String> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw)
        .map_err(|error| format!("cannot generate unpredictable mutation lease token: {error}"))?;
    let mut token = String::with_capacity(68);
    token.push_str("mlt_");
    for byte in raw {
        let _ = write!(&mut token, "{byte:02x}");
    }
    let digest = sha256_hex(&token);
    Ok((token, digest))
}

pub fn verify_lease_token_digest(stored_sha256: Option<&str>, candidate_token: &str) -> bool {
    let Some(expected) = stored_sha256
        .map(str::trim)
        .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
    else {
        return false;
    };
    let trimmed = candidate_token.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_MUTATION_LEASE_TOKEN_BYTES
        || trimmed.chars().any(char::is_control)
    {
        return false;
    }
    let actual = sha256_hex(trimmed);
    constant_time_eq(actual.as_bytes(), expected.to_ascii_lowercase().as_bytes())
}

/// Canonicalize a project root or target filesystem path for lease scoping.
///
/// Matches `fs_security::resolve_input` + `normalize_lexical` by:
/// 1. Resolving relative paths against `std::env::current_dir()`,
/// 2. Lexically folding `.` and `..` segments (`Component::ParentDir => pop()`),
/// 3. Resolving existing symlinks (such as `/var` -> `/private/var` on macOS)
///    by canonicalizing the path or its deepest existing ancestor and joining
///    the remaining normal path components.
pub fn canonicalize_lease_path(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("project_root must not be empty".to_owned());
    }
    if trimmed.len() > MAX_MUTATION_LEASE_PATH_BYTES {
        return Err(format!(
            "project_root exceeds maximum length of {MAX_MUTATION_LEASE_PATH_BYTES} bytes"
        ));
    }
    if trimmed.contains('\0') {
        return Err("project_root must not contain NUL bytes".to_owned());
    }
    let raw_path = PathBuf::from(trimmed);
    let resolved = if raw_path.is_absolute() {
        raw_path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(raw_path))
            .map_err(|error| format!("cannot resolve relative lease path: {error}"))?
    };
    let mut normalized = PathBuf::new();
    for component in resolved.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    if !normalized.is_absolute() {
        return Err("project_root must resolve to an absolute path".to_owned());
    }
    if let Ok(canon) = std::fs::canonicalize(&normalized) {
        return Ok(canon.to_string_lossy().into_owned());
    }
    let mut tail = Vec::new();
    let mut cursor: &Path = &normalized;
    while let Some(parent) = cursor.parent() {
        if let Some(name) = cursor.file_name() {
            tail.push(name.to_os_string());
        }
        if let Ok(canon_parent) = std::fs::canonicalize(parent) {
            let mut rebuilt = canon_parent;
            for part in tail.into_iter().rev() {
                rebuilt.push(part);
            }
            return Ok(rebuilt.to_string_lossy().into_owned());
        }
        if parent == cursor {
            break;
        }
        cursor = parent;
    }
    Ok(normalized.to_string_lossy().into_owned())
}

fn validate_lane_key(raw: Option<&str>) -> Result<String, String> {
    let key = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_MUTATION_LEASE_LANE_KEY);
    if key.len() > MAX_MUTATION_LEASE_ID_BYTES {
        return Err(format!(
            "lane_key exceeds maximum length of {MAX_MUTATION_LEASE_ID_BYTES} bytes"
        ));
    }
    if key.chars().any(char::is_control) {
        return Err("lane_key must not contain control characters".to_owned());
    }
    Ok(key.to_owned())
}

fn validate_holder_id(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("holder_id must not be empty".to_owned());
    }
    if trimmed.len() > MAX_MUTATION_LEASE_ID_BYTES {
        return Err(format!(
            "holder_id exceeds maximum length of {MAX_MUTATION_LEASE_ID_BYTES} bytes"
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err("holder_id must not contain control characters".to_owned());
    }
    Ok(trimmed.to_owned())
}

fn validate_optional_metadata(field: &str, raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > MAX_MUTATION_LEASE_ID_BYTES {
        return Err(format!(
            "{field} exceeds maximum length of {MAX_MUTATION_LEASE_ID_BYTES} bytes"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{field} must not contain control characters"));
    }
    Ok(Some(value.to_owned()))
}

pub fn validate_ttl_seconds(raw: Option<u64>) -> Result<u64, String> {
    let ttl = raw.unwrap_or(DEFAULT_MUTATION_LEASE_TTL_SECONDS);
    if !(MIN_MUTATION_LEASE_TTL_SECONDS..=MAX_MUTATION_LEASE_TTL_SECONDS).contains(&ttl) {
        return Err(format!(
            "ttl_seconds must be between {MIN_MUTATION_LEASE_TTL_SECONDS} and {MAX_MUTATION_LEASE_TTL_SECONDS}"
        ));
    }
    Ok(ttl)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationLeaseRecord {
    pub project_root: String,
    pub lane_key: String,
    pub holder_id: String,
    pub lease_epoch: i64,
    pub device_id: Option<String>,
    pub workspace_id: Option<String>,
    pub pane_id: Option<String>,
    pub branch: Option<String>,
    pub acquired_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
    pub intent: String,
    pub readers: Vec<MutationLeaseReader>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationLeaseReader {
    pub holder_id: String,
    pub device_id: Option<String>,
    pub workspace_id: Option<String>,
    pub pane_id: Option<String>,
    pub branch: Option<String>,
    pub acquired_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredReader {
    #[serde(flatten)]
    reader: MutationLeaseReader,
    token_sha256: String,
    updated_at: i64,
    epoch: i64,
}

#[derive(Debug, Clone)]
struct StoredMutationLeaseRow {
    record: MutationLeaseRecord,
    released_at: Option<i64>,
    lease_token_sha256: Option<String>,
    readers: Vec<StoredReader>,
}

impl StoredMutationLeaseRow {
    fn is_active(&self, now_ms: i64) -> bool {
        self.released_at.is_none() && self.record.expires_at > now_ms
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MutationLeaseAcquireInput<'a> {
    pub project_root: &'a str,
    pub lane_key: Option<&'a str>,
    pub intent: Option<&'a str>,
    pub holder_id: &'a str,
    pub lease_token: Option<&'a str>,
    pub ttl_seconds: Option<u64>,
    pub workspace_id: Option<&'a str>,
    pub pane_id: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationLeaseAcquireOutcome {
    Acquired {
        lease: MutationLeaseRecord,
        lease_token: String,
        reacquired: bool,
    },
    Conflict {
        reason: &'static str,
        active_lease: MutationLeaseRecord,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct MutationLeaseRenewInput<'a> {
    pub project_root: &'a str,
    pub lane_key: Option<&'a str>,
    pub holder_id: &'a str,
    pub lease_epoch: i64,
    pub lease_token: Option<&'a str>,
    pub ttl_seconds: Option<u64>,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationLeaseRenewOutcome {
    Renewed {
        lease: MutationLeaseRecord,
        lease_token: String,
    },
    Conflict {
        reason: &'static str,
        active_lease: Option<MutationLeaseRecord>,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct MutationLeaseReleaseInput<'a> {
    pub project_root: &'a str,
    pub lane_key: Option<&'a str>,
    pub holder_id: &'a str,
    pub lease_epoch: i64,
    pub lease_token: Option<&'a str>,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationLeaseReleaseOutcome {
    Released {
        lease: MutationLeaseRecord,
        released_at: i64,
        idempotent_replay: bool,
    },
    Conflict {
        reason: &'static str,
        active_lease: Option<MutationLeaseRecord>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationLeaseEnforcementVerdict {
    Allowed {
        matched_lease: Option<MutationLeaseRecord>,
    },
    Conflict {
        reason: &'static str,
        active_lease: MutationLeaseRecord,
    },
}

fn row_to_stored_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMutationLeaseRow> {
    let epoch: i64 = row.get(3)?;
    let intent: Option<String> = row.get(13)?;
    let intent = if intent.as_deref() == Some("read") {
        "read"
    } else {
        "write"
    };
    let json: Option<String> = row.get(14)?;
    let readers: Vec<StoredReader> = if intent == "read" {
        serde_json::from_str(json.as_deref().unwrap_or("[]")).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                14,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?
    } else {
        Vec::new()
    };
    let readers: Vec<_> = readers
        .into_iter()
        .filter(|entry| {
            entry.epoch == epoch
                && validate_holder_id(&entry.reader.holder_id).as_deref()
                    == Ok(entry.reader.holder_id.as_str())
                && [
                    (&entry.reader.device_id, "device_id"),
                    (&entry.reader.workspace_id, "workspace_id"),
                    (&entry.reader.pane_id, "pane_id"),
                    (&entry.reader.branch, "branch"),
                ]
                .into_iter()
                .all(|(value, name)| {
                    validate_optional_metadata(name, value.as_deref())
                        .ok()
                        .as_ref()
                        == Some(value)
                })
                && entry.token_sha256.len() == 64
                && entry.token_sha256.chars().all(|c| c.is_ascii_hexdigit())
        })
        .take(64)
        .collect();
    Ok(StoredMutationLeaseRow {
        record: MutationLeaseRecord {
            project_root: row.get(0)?,
            lane_key: row.get(1)?,
            holder_id: row.get(2)?,
            lease_epoch: row.get(3)?,
            workspace_id: row.get(4)?,
            pane_id: row.get(5)?,
            branch: row.get(6)?,
            acquired_at: row.get(7)?,
            updated_at: row.get(8)?,
            expires_at: row.get(9)?,
            device_id: row.get(10)?,
            intent: intent.to_owned(),
            readers: readers.iter().map(|reader| reader.reader.clone()).collect(),
        },
        released_at: row.get(11)?,
        lease_token_sha256: row.get(12)?,
        readers,
    })
}

fn load_stored_lease(
    conn: &Connection,
    project_root: &str,
    lane_key: &str,
) -> Result<Option<StoredMutationLeaseRow>, String> {
    conn.query_row(
        "SELECT project_root, lane_key, holder_id, lease_epoch, workspace_id, pane_id, branch,
                acquired_at, updated_at, expires_at, device_id, released_at, lease_token_sha256, intent, readers_json
           FROM mutation_leases
          WHERE project_root = ?1 AND lane_key = ?2",
        params![project_root, lane_key],
        row_to_stored_lease,
    )
    .optional()
    .map_err(|error| format!("cannot query mutation_leases: {error}"))
}

impl StateStore {
    #[cfg_attr(not(test), allow(dead_code))] // all callers are test modules; production uses acquire_mutation_lease_for_device
    pub fn acquire_mutation_lease(
        &mut self,
        input: MutationLeaseAcquireInput<'_>,
    ) -> Result<MutationLeaseAcquireOutcome, String> {
        self.acquire_mutation_lease_for_device(input, None)
    }

    pub fn acquire_mutation_lease_for_device(
        &mut self,
        input: MutationLeaseAcquireInput<'_>,
        device_id: Option<&str>,
    ) -> Result<MutationLeaseAcquireOutcome, String> {
        let project_root = canonicalize_lease_path(input.project_root)?;
        let lane_key = validate_lane_key(input.lane_key)?;
        let holder_id = validate_holder_id(input.holder_id)?;
        let intent = match input.intent.unwrap_or("write") {
            "read" => "read",
            "write" => "write",
            _ => return Err("intent must be 'read' or 'write'".to_owned()),
        };
        let device_id = validate_optional_metadata("device_id", device_id)?;
        let ttl_seconds = validate_ttl_seconds(input.ttl_seconds)?;
        let workspace_id = validate_optional_metadata("workspace_id", input.workspace_id)?;
        let pane_id = validate_optional_metadata("pane_id", input.pane_id)?;
        let branch = validate_optional_metadata("branch", input.branch)?;
        let ttl_ms = (ttl_seconds as i64).saturating_mul(1000);
        let expires_at = input.now_ms.saturating_add(ttl_ms);

        let tx = self
            .conn
            .transaction()
            .map_err(|error| format!("cannot begin mutation lease transaction: {error}"))?;

        let existing = load_stored_lease(&tx, &project_root, &lane_key)?;
        if intent == "read" {
            if let Some(stored) = &existing
                && stored.is_active(input.now_ms)
                && stored.record.intent != "read"
            {
                return Ok(MutationLeaseAcquireOutcome::Conflict {
                    reason: "lease_held_by_other",
                    active_lease: stored.record.clone(),
                });
            }
            let active = existing
                .as_ref()
                .is_some_and(|row| row.is_active(input.now_ms));
            let epoch = existing.as_ref().map_or(1, |row| {
                if active {
                    row.record.lease_epoch
                } else {
                    row.record.lease_epoch.saturating_add(1)
                }
            });
            let mut readers = if active {
                existing.as_ref().unwrap().readers.clone()
            } else {
                Vec::new()
            };
            readers.retain(|reader| reader.reader.expires_at > input.now_ms);
            let position = readers
                .iter()
                .position(|reader| reader.reader.holder_id == holder_id);
            if position.is_none() && readers.len() >= 64 {
                return Ok(MutationLeaseAcquireOutcome::Conflict {
                    reason: "lease_readers_limit",
                    active_lease: existing.unwrap().record,
                });
            }
            let (token, digest) = generate_lease_token()?;
            let reader = StoredReader {
                reader: MutationLeaseReader {
                    holder_id: holder_id.clone(),
                    device_id: device_id.clone(),
                    workspace_id: workspace_id.clone(),
                    pane_id: pane_id.clone(),
                    branch: branch.clone(),
                    acquired_at: position
                        .map_or(input.now_ms, |index| readers[index].reader.acquired_at),
                    expires_at,
                },
                token_sha256: digest,
                updated_at: input.now_ms,
                epoch,
            };
            if let Some(index) = position {
                readers[index] = reader;
            } else {
                readers.push(reader);
            }
            let json = serde_json::to_string(&readers)
                .map_err(|error| format!("cannot encode lease readers: {error}"))?;
            let max_expiry = readers
                .iter()
                .map(|reader| reader.reader.expires_at)
                .max()
                .unwrap();
            if active {
                tx.execute("UPDATE mutation_leases SET readers_json = ?1, updated_at = ?2, expires_at = ?3 WHERE project_root = ?4 AND lane_key = ?5", params![json, input.now_ms, max_expiry, project_root, lane_key])
                    .map_err(|error| format!("cannot append lease reader: {error}"))?;
            } else if existing.is_some() {
                tx.execute("UPDATE mutation_leases SET holder_id = ?1, lease_epoch = ?2, device_id = ?3, workspace_id = ?4, pane_id = ?5, branch = ?6, acquired_at = ?7, updated_at = ?7, expires_at = ?8, released_at = NULL, lease_token_sha256 = NULL, intent = 'read', readers_json = ?9 WHERE project_root = ?10 AND lane_key = ?11", params![holder_id, epoch, device_id, workspace_id, pane_id, branch, input.now_ms, max_expiry, json, project_root, lane_key])
                    .map_err(|error| format!("cannot advance read lease epoch: {error}"))?;
                // Reattest fields when an old-writer safety trigger sees equal values.
                tx.execute("UPDATE mutation_leases SET device_id = ?1, intent = 'read', readers_json = ?2 WHERE project_root = ?3 AND lane_key = ?4", params![device_id, json, project_root, lane_key]).map_err(|error| format!("cannot bind read lease: {error}"))?;
            } else {
                tx.execute("INSERT INTO mutation_leases (project_root,lane_key,holder_id,lease_epoch,device_id,workspace_id,pane_id,branch,acquired_at,updated_at,expires_at,released_at,lease_token_sha256,intent,readers_json) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,NULL,NULL,'read',?11)", params![project_root,lane_key,holder_id,epoch,device_id,workspace_id,pane_id,branch,input.now_ms,max_expiry,json])
                    .map_err(|error| format!("cannot insert read lease: {error}"))?;
            }
            let record = load_stored_lease(&tx, &project_root, &lane_key)?
                .unwrap()
                .record;
            tx.commit()
                .map_err(|error| format!("cannot commit mutation lease acquire: {error}"))?;
            return Ok(MutationLeaseAcquireOutcome::Acquired {
                lease: record,
                lease_token: token,
                reacquired: position.is_some(),
            });
        }
        let outcome = match existing {
            None => {
                let (lease_token, lease_token_sha256) = generate_lease_token()?;
                let record = MutationLeaseRecord {
                    project_root: project_root.clone(),
                    lane_key: lane_key.clone(),
                    holder_id,
                    lease_epoch: 1,
                    device_id: device_id.clone(),
                    workspace_id,
                    pane_id,
                    branch,
                    acquired_at: input.now_ms,
                    updated_at: input.now_ms,
                    expires_at,
                    intent: "write".to_owned(),
                    readers: Vec::new(),
                };
                tx.execute(
                    "INSERT INTO mutation_leases (
                        project_root, lane_key, holder_id, lease_epoch, device_id,
                        workspace_id, pane_id, branch,
                        acquired_at, updated_at, expires_at, released_at, lease_token_sha256, intent, readers_json
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?12, 'write', NULL)",
                    params![
                        record.project_root,
                        record.lane_key,
                        record.holder_id,
                        record.lease_epoch,
                        record.device_id,
                        record.workspace_id,
                        record.pane_id,
                        record.branch,
                        record.acquired_at,
                        record.updated_at,
                        record.expires_at,
                        lease_token_sha256,
                    ],
                )
                .map_err(|error| format!("cannot insert mutation lease: {error}"))?;
                MutationLeaseAcquireOutcome::Acquired {
                    lease: record,
                    lease_token,
                    reacquired: false,
                }
            }
            Some(stored) if stored.is_active(input.now_ms) => {
                if stored.record.intent == "read" {
                    return Ok(MutationLeaseAcquireOutcome::Conflict {
                        reason: "lease_held_by_other",
                        active_lease: stored.record,
                    });
                }
                if stored.record.holder_id != holder_id {
                    MutationLeaseAcquireOutcome::Conflict {
                        reason: "lease_held_by_other",
                        active_lease: stored.record,
                    }
                } else {
                    let Some(caller_token) =
                        input.lease_token.map(str::trim).filter(|s| !s.is_empty())
                    else {
                        return Ok(MutationLeaseAcquireOutcome::Conflict {
                            reason: "lease_token_required",
                            active_lease: stored.record,
                        });
                    };
                    if !verify_lease_token_digest(
                        stored.lease_token_sha256.as_deref(),
                        caller_token,
                    ) {
                        return Ok(MutationLeaseAcquireOutcome::Conflict {
                            reason: "lease_token_invalid",
                            active_lease: stored.record,
                        });
                    }
                    let updated = MutationLeaseRecord {
                        project_root: project_root.clone(),
                        lane_key: lane_key.clone(),
                        holder_id,
                        lease_epoch: stored.record.lease_epoch,
                        device_id: stored.record.device_id,
                        workspace_id: workspace_id.or(stored.record.workspace_id),
                        pane_id: pane_id.or(stored.record.pane_id),
                        branch: branch.or(stored.record.branch),
                        acquired_at: stored.record.acquired_at,
                        updated_at: input.now_ms,
                        expires_at,
                        intent: "write".to_owned(),
                        readers: Vec::new(),
                    };
                    tx.execute(
                        "UPDATE mutation_leases
                            SET device_id = ?1,
                                workspace_id = ?2,
                                pane_id = ?3,
                                branch = ?4,
                                updated_at = ?5,
                                expires_at = ?6,
                                released_at = NULL
                          WHERE project_root = ?7 AND lane_key = ?8",
                        params![
                            updated.device_id,
                            updated.workspace_id,
                            updated.pane_id,
                            updated.branch,
                            updated.updated_at,
                            updated.expires_at,
                            updated.project_root,
                            updated.lane_key,
                        ],
                    )
                    .map_err(|error| format!("cannot reacquire mutation lease: {error}"))?;
                    MutationLeaseAcquireOutcome::Acquired {
                        lease: updated,
                        lease_token: caller_token.to_owned(),
                        reacquired: true,
                    }
                }
            }
            Some(stored) => {
                let (lease_token, lease_token_sha256) = generate_lease_token()?;
                let next_epoch = stored.record.lease_epoch.saturating_add(1);
                let record = MutationLeaseRecord {
                    project_root: project_root.clone(),
                    lane_key: lane_key.clone(),
                    holder_id,
                    lease_epoch: next_epoch,
                    device_id: device_id.clone(),
                    workspace_id,
                    pane_id,
                    branch,
                    acquired_at: input.now_ms,
                    updated_at: input.now_ms,
                    expires_at,
                    intent: "write".to_owned(),
                    readers: Vec::new(),
                };
                tx.execute(
                    "UPDATE mutation_leases
                        SET holder_id = ?1,
                            lease_epoch = ?2,
                            device_id = ?3,
                            workspace_id = ?4,
                            pane_id = ?5,
                            branch = ?6,
                            acquired_at = ?7,
                            updated_at = ?8,
                            expires_at = ?9,
                            lease_token_sha256 = ?10,
                            intent = 'write', readers_json = NULL,
                            released_at = NULL
                      WHERE project_root = ?11 AND lane_key = ?12",
                    params![
                        record.holder_id,
                        record.lease_epoch,
                        record.device_id,
                        record.workspace_id,
                        record.pane_id,
                        record.branch,
                        record.acquired_at,
                        record.updated_at,
                        record.expires_at,
                        lease_token_sha256,
                        record.project_root,
                        record.lane_key,
                    ],
                )
                .map_err(|error| format!("cannot advance mutation lease epoch: {error}"))?;
                tx.execute("UPDATE mutation_leases SET intent = 'write', readers_json = NULL WHERE project_root = ?1 AND lane_key = ?2", params![record.project_root, record.lane_key]).map_err(|error| format!("cannot bind write lease intent: {error}"))?;
                // If the same device acquires the next epoch, the legacy-writer
                // safety trigger clears it. This write explicitly reattests the
                // current device inside the same transaction.
                if record.device_id.is_some() {
                    tx.execute(
                        "UPDATE mutation_leases SET device_id = ?1 WHERE project_root = ?2 AND lane_key = ?3",
                        params![record.device_id, record.project_root, record.lane_key],
                    )
                    .map_err(|error| format!("cannot bind mutation lease device: {error}"))?;
                }
                MutationLeaseAcquireOutcome::Acquired {
                    lease: record,
                    lease_token,
                    reacquired: false,
                }
            }
        };

        tx.commit()
            .map_err(|error| format!("cannot commit mutation lease acquire: {error}"))?;
        Ok(outcome)
    }

    pub fn renew_mutation_lease(
        &mut self,
        input: MutationLeaseRenewInput<'_>,
    ) -> Result<MutationLeaseRenewOutcome, String> {
        let project_root = canonicalize_lease_path(input.project_root)?;
        let lane_key = validate_lane_key(input.lane_key)?;
        let holder_id = validate_holder_id(input.holder_id)?;
        if input.lease_epoch < 1 {
            return Err("lease_epoch must be a positive integer".to_owned());
        }
        let ttl_seconds = validate_ttl_seconds(input.ttl_seconds)?;
        let ttl_ms = (ttl_seconds as i64).saturating_mul(1000);
        let expires_at = input.now_ms.saturating_add(ttl_ms);

        let tx = self
            .conn
            .transaction()
            .map_err(|error| format!("cannot begin mutation lease renew transaction: {error}"))?;

        let existing = load_stored_lease(&tx, &project_root, &lane_key)?;
        if let Some(stored) = &existing
            && stored.is_active(input.now_ms)
            && stored.record.intent == "read"
        {
            let conflict = |reason| MutationLeaseRenewOutcome::Conflict {
                reason,
                active_lease: Some(stored.record.clone()),
            };
            if stored.record.lease_epoch != input.lease_epoch {
                return Ok(conflict("lease_epoch_mismatch"));
            }
            let Some(index) = stored.readers.iter().position(|reader| {
                reader.reader.holder_id == holder_id && reader.reader.expires_at > input.now_ms
            }) else {
                return Ok(conflict("lease_held_by_other"));
            };
            let Some(token) = input
                .lease_token
                .map(str::trim)
                .filter(|token| !token.is_empty())
            else {
                return Ok(conflict("lease_token_required"));
            };
            if !verify_lease_token_digest(Some(&stored.readers[index].token_sha256), token) {
                return Ok(conflict("lease_token_invalid"));
            }
            let mut readers = stored.readers.clone();
            readers.retain(|reader| reader.reader.expires_at > input.now_ms);
            let reader = readers
                .iter_mut()
                .find(|reader| reader.reader.holder_id == holder_id)
                .unwrap();
            reader.reader.expires_at = expires_at;
            reader.updated_at = input.now_ms;
            let max_expiry = readers
                .iter()
                .map(|reader| reader.reader.expires_at)
                .max()
                .unwrap();
            let json = serde_json::to_string(&readers)
                .map_err(|error| format!("cannot encode lease readers: {error}"))?;
            tx.execute("UPDATE mutation_leases SET readers_json = ?1, updated_at = ?2, expires_at = ?3 WHERE project_root = ?4 AND lane_key = ?5", params![json, input.now_ms, max_expiry, project_root, lane_key]).map_err(|error| format!("cannot renew lease reader: {error}"))?;
            let lease = load_stored_lease(&tx, &project_root, &lane_key)?
                .unwrap()
                .record;
            tx.commit()
                .map_err(|error| format!("cannot commit mutation lease renew: {error}"))?;
            return Ok(MutationLeaseRenewOutcome::Renewed {
                lease,
                lease_token: token.to_owned(),
            });
        }
        let outcome = match existing {
            None => MutationLeaseRenewOutcome::Conflict {
                reason: "lease_not_held",
                active_lease: None,
            },
            Some(stored) if !stored.is_active(input.now_ms) => {
                let reason = if stored.released_at.is_some() {
                    "lease_not_held"
                } else {
                    "lease_expired"
                };
                MutationLeaseRenewOutcome::Conflict {
                    reason,
                    active_lease: None,
                }
            }
            Some(stored) => {
                if stored.record.holder_id != holder_id {
                    MutationLeaseRenewOutcome::Conflict {
                        reason: "lease_held_by_other",
                        active_lease: Some(stored.record),
                    }
                } else if stored.record.lease_epoch != input.lease_epoch {
                    MutationLeaseRenewOutcome::Conflict {
                        reason: "lease_epoch_mismatch",
                        active_lease: Some(stored.record),
                    }
                } else {
                    let Some(caller_token) =
                        input.lease_token.map(str::trim).filter(|s| !s.is_empty())
                    else {
                        return Ok(MutationLeaseRenewOutcome::Conflict {
                            reason: "lease_token_required",
                            active_lease: Some(stored.record),
                        });
                    };
                    if !verify_lease_token_digest(
                        stored.lease_token_sha256.as_deref(),
                        caller_token,
                    ) {
                        return Ok(MutationLeaseRenewOutcome::Conflict {
                            reason: "lease_token_invalid",
                            active_lease: Some(stored.record),
                        });
                    }
                    let mut updated = stored.record;
                    updated.updated_at = input.now_ms;
                    updated.expires_at = expires_at;
                    tx.execute(
                        "UPDATE mutation_leases
                            SET updated_at = ?1,
                                expires_at = ?2
                          WHERE project_root = ?3 AND lane_key = ?4
                            AND holder_id = ?5 AND lease_epoch = ?6
                            AND released_at IS NULL",
                        params![
                            updated.updated_at,
                            updated.expires_at,
                            updated.project_root,
                            updated.lane_key,
                            updated.holder_id,
                            updated.lease_epoch,
                        ],
                    )
                    .map_err(|error| format!("cannot renew mutation lease: {error}"))?;
                    MutationLeaseRenewOutcome::Renewed {
                        lease: updated,
                        lease_token: caller_token.to_owned(),
                    }
                }
            }
        };

        tx.commit()
            .map_err(|error| format!("cannot commit mutation lease renew: {error}"))?;
        Ok(outcome)
    }

    pub fn release_mutation_lease(
        &mut self,
        input: MutationLeaseReleaseInput<'_>,
    ) -> Result<MutationLeaseReleaseOutcome, String> {
        let project_root = canonicalize_lease_path(input.project_root)?;
        let lane_key = validate_lane_key(input.lane_key)?;
        let holder_id = validate_holder_id(input.holder_id)?;
        if input.lease_epoch < 1 {
            return Err("lease_epoch must be a positive integer".to_owned());
        }

        let tx = self
            .conn
            .transaction()
            .map_err(|error| format!("cannot begin mutation lease release transaction: {error}"))?;

        let existing = load_stored_lease(&tx, &project_root, &lane_key)?;
        if let Some(stored) = &existing
            && stored.is_active(input.now_ms)
            && stored.record.intent == "read"
        {
            let conflict = |reason| MutationLeaseReleaseOutcome::Conflict {
                reason,
                active_lease: Some(stored.record.clone()),
            };
            if stored.record.lease_epoch != input.lease_epoch {
                return Ok(conflict("lease_epoch_mismatch"));
            }
            let Some(index) = stored.readers.iter().position(|reader| {
                reader.reader.holder_id == holder_id && reader.reader.expires_at > input.now_ms
            }) else {
                return Ok(conflict("lease_held_by_other"));
            };
            let Some(token) = input
                .lease_token
                .map(str::trim)
                .filter(|token| !token.is_empty())
            else {
                return Ok(conflict("lease_token_required"));
            };
            if !verify_lease_token_digest(Some(&stored.readers[index].token_sha256), token) {
                return Ok(conflict("lease_token_invalid"));
            }
            let mut readers = stored.readers.clone();
            readers.retain(|reader| {
                reader.reader.holder_id != holder_id && reader.reader.expires_at > input.now_ms
            });
            let max_expiry = readers
                .iter()
                .map(|reader| reader.reader.expires_at)
                .max()
                .unwrap_or(input.now_ms);
            let released_at = if readers.is_empty() {
                Some(input.now_ms)
            } else {
                None
            };
            let json = serde_json::to_string(&readers)
                .map_err(|error| format!("cannot encode lease readers: {error}"))?;
            tx.execute("UPDATE mutation_leases SET readers_json = ?1, updated_at = ?2, expires_at = ?3, released_at = ?4 WHERE project_root = ?5 AND lane_key = ?6", params![json, input.now_ms, max_expiry, released_at, project_root, lane_key]).map_err(|error| format!("cannot release lease reader: {error}"))?;
            let lease = load_stored_lease(&tx, &project_root, &lane_key)?
                .unwrap()
                .record;
            tx.commit()
                .map_err(|error| format!("cannot commit mutation lease release: {error}"))?;
            return Ok(MutationLeaseReleaseOutcome::Released {
                lease,
                released_at: input.now_ms,
                idempotent_replay: false,
            });
        }
        let outcome = match existing {
            None => MutationLeaseReleaseOutcome::Conflict {
                reason: "lease_not_held",
                active_lease: None,
            },
            Some(stored) if stored.is_active(input.now_ms) => {
                if stored.record.holder_id != holder_id {
                    MutationLeaseReleaseOutcome::Conflict {
                        reason: "lease_held_by_other",
                        active_lease: Some(stored.record),
                    }
                } else if stored.record.lease_epoch != input.lease_epoch {
                    MutationLeaseReleaseOutcome::Conflict {
                        reason: "lease_epoch_mismatch",
                        active_lease: Some(stored.record),
                    }
                } else {
                    let Some(caller_token) =
                        input.lease_token.map(str::trim).filter(|s| !s.is_empty())
                    else {
                        return Ok(MutationLeaseReleaseOutcome::Conflict {
                            reason: "lease_token_required",
                            active_lease: Some(stored.record),
                        });
                    };
                    if !verify_lease_token_digest(
                        stored.lease_token_sha256.as_deref(),
                        caller_token,
                    ) {
                        return Ok(MutationLeaseReleaseOutcome::Conflict {
                            reason: "lease_token_invalid",
                            active_lease: Some(stored.record),
                        });
                    }
                    let mut updated = stored.record;
                    updated.updated_at = input.now_ms;
                    tx.execute(
                        "UPDATE mutation_leases
                            SET updated_at = ?1,
                                released_at = ?1
                          WHERE project_root = ?2 AND lane_key = ?3
                            AND holder_id = ?4 AND lease_epoch = ?5
                            AND released_at IS NULL",
                        params![
                            input.now_ms,
                            updated.project_root,
                            updated.lane_key,
                            updated.holder_id,
                            updated.lease_epoch,
                        ],
                    )
                    .map_err(|error| format!("cannot release mutation lease: {error}"))?;
                    MutationLeaseReleaseOutcome::Released {
                        lease: updated,
                        released_at: input.now_ms,
                        idempotent_replay: false,
                    }
                }
            }
            Some(stored) => {
                let token_valid = input
                    .lease_token
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .is_some_and(|tok| {
                        verify_lease_token_digest(stored.lease_token_sha256.as_deref(), tok)
                    });
                if let Some(released_at) = stored.released_at
                    && stored.record.holder_id == holder_id
                    && stored.record.lease_epoch == input.lease_epoch
                    && token_valid
                {
                    MutationLeaseReleaseOutcome::Released {
                        lease: stored.record,
                        released_at,
                        idempotent_replay: true,
                    }
                } else {
                    let reason = if stored.record.holder_id != holder_id {
                        if stored.released_at.is_some() {
                            "lease_not_held"
                        } else {
                            "lease_expired"
                        }
                    } else if stored.record.lease_epoch != input.lease_epoch {
                        "lease_epoch_mismatch"
                    } else if input
                        .lease_token
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .is_none()
                    {
                        "lease_token_required"
                    } else if !token_valid {
                        "lease_token_invalid"
                    } else if stored.released_at.is_some() {
                        "lease_not_held"
                    } else {
                        "lease_expired"
                    };
                    MutationLeaseReleaseOutcome::Conflict {
                        reason,
                        active_lease: None,
                    }
                }
            }
        };

        tx.commit()
            .map_err(|error| format!("cannot commit mutation lease release: {error}"))?;
        Ok(outcome)
    }

    pub fn list_active_mutation_leases(
        &self,
        project_root_filter: Option<&str>,
        lane_key_filter: Option<&str>,
        now_ms: i64,
    ) -> Result<Vec<MutationLeaseRecord>, String> {
        let canonical_root = match project_root_filter
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(raw) => Some(canonicalize_lease_path(raw)?),
            None => None,
        };
        let lane_filter = match lane_key_filter
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(raw) => Some(validate_lane_key(Some(raw))?),
            None => None,
        };

        let mut stmt = self
            .conn
            .prepare(
                "SELECT project_root, lane_key, holder_id, lease_epoch, workspace_id, pane_id, branch,
                        acquired_at, updated_at, expires_at, device_id, released_at, lease_token_sha256, intent, readers_json
                   FROM mutation_leases
                  WHERE released_at IS NULL
                    AND expires_at > ?1
                    AND (?2 IS NULL OR project_root = ?2)
                    AND (?3 IS NULL OR lane_key = ?3)
                  ORDER BY project_root ASC, lane_key ASC",
            )
            .map_err(|error| format!("cannot prepare list_active_mutation_leases: {error}"))?;
        let rows = stmt
            .query_map(
                params![now_ms, canonical_root.as_deref(), lane_filter.as_deref()],
                row_to_stored_lease,
            )
            .map_err(|error| format!("cannot query active mutation leases: {error}"))?;
        let mut out = Vec::new();
        for row in rows {
            let mut record = row
                .map_err(|error| format!("cannot decode mutation lease row: {error}"))?
                .record;
            record.readers.retain(|reader| reader.expires_at > now_ms);
            out.push(record);
        }
        Ok(out)
    }

    /// Check whether a target path falls under an active root lease (`lane_key == "*"`).
    ///
    /// When no active root lease covers `target_path`, returns `Allowed { matched_lease: None }`
    /// so unleased project roots experience zero friction.
    /// When at least one active root lease exists and `target_path` cannot be canonicalized,
    /// fails closed with `Conflict { reason: "lease_target_path_invalid", .. }`.
    pub fn check_root_mutation_lease_for_path(
        &self,
        target_path: &str,
        caller_holder_id: Option<&str>,
        caller_lease_epoch: Option<i64>,
        caller_lease_token: Option<&str>,
        _caller_session_bound: bool,
        now_ms: i64,
    ) -> Result<MutationLeaseEnforcementVerdict, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT project_root, lane_key, holder_id, lease_epoch, workspace_id, pane_id, branch,
                        acquired_at, updated_at, expires_at, device_id, released_at, lease_token_sha256, intent, readers_json
                   FROM mutation_leases
                  WHERE lane_key = '*'
                    AND released_at IS NULL
                    AND expires_at > ?1
                  ORDER BY length(project_root) DESC, project_root ASC",
            )
            .map_err(|error| format!("cannot prepare root mutation lease check: {error}"))?;
        let rows = stmt
            .query_map(params![now_ms], row_to_stored_lease)
            .map_err(|error| format!("cannot query root mutation leases: {error}"))?;

        let mut active_rows = Vec::new();
        for row in rows {
            active_rows
                .push(row.map_err(|error| format!("cannot decode root mutation lease: {error}"))?);
        }

        if active_rows.is_empty() {
            return Ok(MutationLeaseEnforcementVerdict::Allowed {
                matched_lease: None,
            });
        }

        let canonical_target = match canonicalize_lease_path(target_path) {
            Ok(path) => path,
            Err(_) => {
                // Fail-closed: never allow an unnormalizable path to bypass an active root lease.
                return Ok(MutationLeaseEnforcementVerdict::Conflict {
                    reason: "lease_target_path_invalid",
                    active_lease: active_rows.into_iter().next().unwrap().record,
                });
            }
        };
        let target_as_path = Path::new(&canonical_target);

        let matching = active_rows
            .into_iter()
            .filter(|stored| target_as_path.starts_with(Path::new(&stored.record.project_root)))
            .collect::<Vec<_>>();
        let stored = if let Some(write_lease) = matching
            .iter()
            .find(|stored| stored.record.intent != "read")
        {
            Some(write_lease.clone())
        } else {
            matching.into_iter().next()
        };

        let Some(stored) = stored else {
            return Ok(MutationLeaseEnforcementVerdict::Allowed {
                matched_lease: None,
            });
        };
        let expected_digest = stored.lease_token_sha256.clone();
        let active_lease = stored.record;
        if active_lease.intent == "read" {
            return Ok(MutationLeaseEnforcementVerdict::Conflict {
                reason: "lease_held_by_other",
                active_lease,
            });
        }

        let Some(holder_id) = caller_holder_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(MutationLeaseEnforcementVerdict::Conflict {
                reason: "lease_credentials_required",
                active_lease,
            });
        };

        if holder_id != active_lease.holder_id {
            return Ok(MutationLeaseEnforcementVerdict::Conflict {
                reason: "lease_held_by_other",
                active_lease,
            });
        }

        match caller_lease_epoch {
            Some(epoch) if epoch == active_lease.lease_epoch => {}
            Some(_) => {
                return Ok(MutationLeaseEnforcementVerdict::Conflict {
                    reason: "lease_epoch_mismatch",
                    active_lease,
                });
            }
            None => {
                return Ok(MutationLeaseEnforcementVerdict::Conflict {
                    reason: "lease_epoch_required",
                    active_lease,
                });
            }
        }

        let Some(token) = caller_lease_token
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(MutationLeaseEnforcementVerdict::Conflict {
                reason: "lease_token_required",
                active_lease,
            });
        };

        if !verify_lease_token_digest(expected_digest.as_deref(), token) {
            return Ok(MutationLeaseEnforcementVerdict::Conflict {
                reason: "lease_token_invalid",
                active_lease,
            });
        }

        Ok(MutationLeaseEnforcementVerdict::Allowed {
            matched_lease: Some(active_lease),
        })
    }
}

#[cfg(test)]
mod device_migration_tests {
    use super::*;

    #[test]
    fn legacy_rows_remain_null_and_old_writer_cannot_retain_stale_device() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mutation_leases (
                project_root TEXT NOT NULL, lane_key TEXT NOT NULL DEFAULT '*',
                holder_id TEXT NOT NULL, lease_epoch INTEGER NOT NULL,
                workspace_id TEXT, pane_id TEXT, branch TEXT,
                acquired_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL, released_at INTEGER,
                lease_token_sha256 TEXT, PRIMARY KEY(project_root, lane_key)
             );
             INSERT INTO mutation_leases(project_root,lane_key,holder_id,lease_epoch,
                 acquired_at,updated_at,expires_at)
             VALUES('/old','*','old-holder',1,1,1,10000);",
        )
        .unwrap();
        ensure_mutation_leases_auxiliary_schema(&conn).unwrap();
        ensure_mutation_leases_auxiliary_schema(&conn).unwrap();
        let row = load_stored_lease(&conn, "/old", "*").unwrap().unwrap();
        assert_eq!(row.record.device_id, None);
        assert_eq!(row.record.holder_id, "old-holder");
        // A rollback-era writer advances the lease without knowing device_id.
        conn.execute(
            "UPDATE mutation_leases SET device_id = 'dev_old' WHERE project_root = '/old'",
            [],
        )
        .unwrap();
        conn.execute("UPDATE mutation_leases SET holder_id = 'new-holder', lease_epoch = 2 WHERE project_root = '/old'", []).unwrap();
        let row = load_stored_lease(&conn, "/old", "*").unwrap().unwrap();
        assert_eq!(row.record.device_id, None);
        assert_eq!(row.record.holder_id, "new-holder");
        assert_eq!(row.record.lease_epoch, 2);
        // A schema-16 reader explicitly selecting the old columns still works.
        let holder: String = conn
            .query_row(
                "SELECT holder_id FROM mutation_leases WHERE project_root = '/old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(holder, "new-holder");
    }
}
