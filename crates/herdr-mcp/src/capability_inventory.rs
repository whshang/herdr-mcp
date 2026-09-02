use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub const INVENTORY_SCHEMA_VERSION: i64 = 1;
const MAX_INVENTORY_RECORDS: usize = 256;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeLevel {
    Version,
    Deep,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Evidence<T> {
    pub value: T,
    pub source: String,
    pub authority: String,
    pub observed_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentCapabilityRecord {
    pub schema_version: i64,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_source_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub herdr_startable: Option<Evidence<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_available: Option<Evidence<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_for_start: Option<Evidence<bool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_version: Option<Evidence<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<Evidence<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Evidence<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<Evidence<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_code_edit: Option<Evidence<bool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_shell: Option<Evidence<bool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<Evidence<bool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tier: Option<Evidence<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_tier: Option<Evidence<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_tier: Option<Evidence<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_tier: Option<Evidence<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive_only: Option<Evidence<bool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_run_headless: Option<Evidence<bool>>,
    pub probe_level: ProbeLevel,
    pub probe_adapter_version: u32,
    pub fingerprint: String,
    pub observed_at_ms: i64,
}

#[derive(Debug)]
pub struct CapabilityInventoryStore {
    path: PathBuf,
    conn: Connection,
}

impl CapabilityInventoryStore {
    pub fn has_scan_cache(config_dir: &Path) -> bool {
        let dir = config_dir.join("capability");
        if reject_symlink(&dir, "capability state directory").is_err() {
            return false;
        }
        let path = dir.join("inventory.db");
        if reject_symlink(&path, "capability inventory database").is_err() || !path.is_file() {
            return false;
        }
        Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()
            .and_then(|conn| {
                conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .ok()
            })
            .is_some_and(|version| version > 0 && version <= INVENTORY_SCHEMA_VERSION)
    }

    pub fn open(config_dir: &Path) -> Result<Self, String> {
        let dir = config_dir.join("capability");
        reject_symlink(&dir, "capability state directory")?;
        std::fs::create_dir_all(&dir).map_err(|error| {
            format!(
                "cannot create capability state directory {}: {error}",
                dir.display()
            )
        })?;
        #[cfg(unix)]
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                format!(
                    "cannot secure capability state directory {}: {error}",
                    dir.display()
                )
            },
        )?;

        let path = dir.join("inventory.db");
        reject_symlink(&path, "capability inventory database")?;
        if !path.exists() {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            options.mode(0o600);
            match options.open(&path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!(
                        "cannot create capability inventory {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        reject_symlink(&path, "capability inventory database")?;
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| {
                format!(
                    "cannot secure capability inventory {}: {error}",
                    path.display()
                )
            },
        )?;

        let conn = Connection::open(&path).map_err(|error| {
            format!(
                "cannot open capability inventory {}: {error}",
                path.display()
            )
        })?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| format!("cannot set capability inventory busy timeout: {error}"))?;
        let store = Self { path, conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), String> {
        let current = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .map_err(|error| format!("cannot read capability inventory schema: {error}"))?;
        if current > INVENTORY_SCHEMA_VERSION {
            return Err(format!(
                "capability inventory schema {current} is newer than supported {INVENTORY_SCHEMA_VERSION}"
            ));
        }
        if current == 0 {
            self.conn
                .execute_batch(
                    r#"
                    BEGIN IMMEDIATE;
                    CREATE TABLE IF NOT EXISTS inventory (
                        agent          TEXT PRIMARY KEY NOT NULL,
                        fingerprint    TEXT NOT NULL,
                        probe_level    TEXT NOT NULL,
                        observed_at_ms INTEGER NOT NULL,
                        record_json    TEXT NOT NULL
                    );
                    CREATE INDEX IF NOT EXISTS idx_capability_inventory_observed
                        ON inventory(observed_at_ms DESC);
                    PRAGMA user_version = 1;
                    COMMIT;
                    "#,
                )
                .map_err(|error| format!("cannot migrate capability inventory: {error}"))?;
        }
        Ok(())
    }

    pub fn get(&self, agent: &str) -> Result<Option<AgentCapabilityRecord>, String> {
        let json = self
            .conn
            .query_row(
                "SELECT record_json FROM inventory WHERE agent = ?1",
                params![agent],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("cannot query capability inventory: {error}"))?;
        json.map(|json| decode_record(&json)).transpose()
    }

    pub fn replace_all(&mut self, records: &[AgentCapabilityRecord]) -> Result<(), String> {
        if records.len() > MAX_INVENTORY_RECORDS {
            return Err(format!(
                "capability inventory has {} records; maximum is {MAX_INVENTORY_RECORDS}",
                records.len()
            ));
        }
        for record in records {
            if record.schema_version != INVENTORY_SCHEMA_VERSION {
                return Err(format!(
                    "capability record schema {} for '{}' does not match supported {INVENTORY_SCHEMA_VERSION}",
                    record.schema_version, record.agent
                ));
            }
        }
        let transaction = self
            .conn
            .transaction()
            .map_err(|error| format!("cannot begin capability inventory transaction: {error}"))?;
        transaction
            .execute("DELETE FROM inventory", [])
            .map_err(|error| format!("cannot clear capability inventory: {error}"))?;
        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO inventory(agent, fingerprint, probe_level, observed_at_ms, record_json) VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(|error| format!("cannot prepare capability inventory insert: {error}"))?;
            for record in records {
                let json = serde_json::to_string(record).map_err(|error| {
                    format!("cannot encode capability inventory record: {error}")
                })?;
                statement
                    .execute(params![
                        &record.agent,
                        &record.fingerprint,
                        match record.probe_level {
                            ProbeLevel::Version => "version",
                            ProbeLevel::Deep => "deep",
                        },
                        record.observed_at_ms,
                        json,
                    ])
                    .map_err(|error| format!("cannot insert capability inventory: {error}"))?;
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("cannot commit capability inventory: {error}"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_existing(config_dir: &Path) -> Result<Vec<AgentCapabilityRecord>, String> {
        let dir = config_dir.join("capability");
        reject_symlink(&dir, "capability state directory")?;
        let path = dir.join("inventory.db");
        reject_symlink(&path, "capability inventory database")?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| {
            format!(
                "cannot open capability inventory read-only {}: {error}",
                path.display()
            )
        })?;
        let current = conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .map_err(|error| format!("cannot read capability inventory schema: {error}"))?;
        if current > INVENTORY_SCHEMA_VERSION {
            return Err(format!(
                "capability inventory schema {current} is newer than supported {INVENTORY_SCHEMA_VERSION}"
            ));
        }
        if current == 0 {
            return Ok(Vec::new());
        }
        let mut statement = conn
            .prepare("SELECT record_json FROM inventory ORDER BY agent")
            .map_err(|error| format!("cannot prepare capability inventory query: {error}"))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| format!("cannot query capability inventory: {error}"))?;
        let mut records = Vec::new();
        for row in rows {
            if records.len() >= MAX_INVENTORY_RECORDS {
                return Err(format!(
                    "capability inventory exceeds maximum {MAX_INVENTORY_RECORDS} records"
                ));
            }
            let json =
                row.map_err(|error| format!("cannot read capability inventory row: {error}"))?;
            records.push(decode_record(&json)?);
        }
        Ok(records)
    }
}

fn decode_record(json: &str) -> Result<AgentCapabilityRecord, String> {
    let record: AgentCapabilityRecord = serde_json::from_str(json)
        .map_err(|error| format!("cannot decode capability inventory record: {error}"))?;
    if record.schema_version != INVENTORY_SCHEMA_VERSION {
        return Err(format!(
            "capability record schema {} for '{}' does not match supported {INVENTORY_SCHEMA_VERSION}",
            record.schema_version, record.agent
        ));
    }
    Ok(record)
}

fn reject_symlink(path: &Path, label: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("refusing symlink {label}: {}", path.display()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot inspect {label} {}: {error}",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "herdr-capability-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn record(agent: &str) -> AgentCapabilityRecord {
        AgentCapabilityRecord {
            schema_version: INVENTORY_SCHEMA_VERSION,
            agent: agent.to_owned(),
            manifest_version: Some("1".to_owned()),
            manifest_source: Some("bundled".to_owned()),
            manifest_source_kind: Some("bundled".to_owned()),
            binary_path: Some(format!("/bin/{agent}")),
            herdr_startable: None,
            executable_available: None,
            available_for_start: None,
            binary_version: None,
            provider: None,
            model: None,
            profile: None,
            supports_code_edit: None,
            supports_shell: None,
            supports_vision: None,
            reasoning_tier: None,
            latency_tier: None,
            cost_tier: None,
            context_tier: None,
            interactive_only: None,
            can_run_headless: None,
            probe_level: ProbeLevel::Version,
            probe_adapter_version: 1,
            fingerprint: format!("sha256:{agent}"),
            observed_at_ms: 1,
        }
    }

    #[cfg(unix)]
    #[test]
    fn inventory_refuses_symlinked_state_directory_for_read_and_write() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("symlink");
        let outside = temp_dir("outside");
        std::fs::create_dir_all(&dir).unwrap();
        let mut outside_store = CapabilityInventoryStore::open(&outside).unwrap();
        outside_store.replace_all(&[record("pi")]).unwrap();
        drop(outside_store);
        symlink(outside.join("capability"), dir.join("capability")).unwrap();

        let write_error = CapabilityInventoryStore::open(&dir).unwrap_err();
        assert!(write_error.contains("refusing symlink capability state directory"));
        let read_error = CapabilityInventoryStore::load_existing(&dir).unwrap_err();
        assert!(read_error.contains("refusing symlink capability state directory"));

        let _ = std::fs::remove_file(dir.join("capability"));
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn inventory_refuses_newer_schema_on_read_or_write() {
        let dir = temp_dir("newer-schema");
        let store = CapabilityInventoryStore::open(&dir).unwrap();
        store
            .conn
            .execute_batch("PRAGMA user_version = 2;")
            .unwrap();
        drop(store);
        let read_error = CapabilityInventoryStore::load_existing(&dir).unwrap_err();
        assert!(read_error.contains("newer than supported"));
        let write_error = CapabilityInventoryStore::open(&dir).unwrap_err();
        assert!(write_error.contains("newer than supported"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn inventory_bounds_record_count_and_record_schema() {
        let dir = temp_dir("bounded");
        let mut store = CapabilityInventoryStore::open(&dir).unwrap();
        let oversized = (0..=MAX_INVENTORY_RECORDS)
            .map(|index| record(&format!("agent-{index}")))
            .collect::<Vec<_>>();
        let error = store.replace_all(&oversized).unwrap_err();
        assert!(error.contains("maximum"));

        let mut future = record("pi");
        future.schema_version = INVENTORY_SCHEMA_VERSION + 1;
        let error = store.replace_all(&[future]).unwrap_err();
        assert!(error.contains("does not match supported"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn inventory_rejects_tampered_record_schema_on_read() {
        let dir = temp_dir("record-schema");
        let mut store = CapabilityInventoryStore::open(&dir).unwrap();
        store.replace_all(&[record("pi")]).unwrap();
        let mut future = record("pi");
        future.schema_version = INVENTORY_SCHEMA_VERSION + 1;
        let json = serde_json::to_string(&future).unwrap();
        store
            .conn
            .execute(
                "UPDATE inventory SET record_json = ?1 WHERE agent = 'pi'",
                params![json],
            )
            .unwrap();
        assert!(
            store
                .get("pi")
                .unwrap_err()
                .contains("does not match supported")
        );
        drop(store);
        assert!(
            CapabilityInventoryStore::load_existing(&dir)
                .unwrap_err()
                .contains("does not match supported")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn inventory_store_round_trips_and_replaces_atomically() {
        let dir = temp_dir("roundtrip");
        let mut store = CapabilityInventoryStore::open(&dir).unwrap();
        assert!(store.path().ends_with("capability/inventory.db"));
        store.replace_all(&[record("pi"), record("codex")]).unwrap();
        assert_eq!(
            CapabilityInventoryStore::load_existing(&dir).unwrap().len(),
            2
        );
        assert_eq!(store.get("pi").unwrap().unwrap().agent, "pi");
        store.replace_all(&[record("grok")]).unwrap();
        assert!(store.get("pi").unwrap().is_none());
        assert_eq!(
            CapabilityInventoryStore::load_existing(&dir).unwrap(),
            vec![record("grok")]
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
