use crate::paths::RuntimePaths;
use rusqlite::{Connection, OptionalExtension, params};
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const UPDATE_STORE_SCHEMA_VERSION: i64 = 1;
const META_SCHEMA_VERSION: &str = "schema_version";

const UPDATE_STORE_MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS update_jobs (
    job_id       TEXT PRIMARY KEY NOT NULL,
    version      TEXT NOT NULL,
    target       TEXT NOT NULL,
    asset_name   TEXT NOT NULL,
    sha256       TEXT NOT NULL,
    binary_path  TEXT NOT NULL,
    state        TEXT NOT NULL,
    detail       TEXT,
    worker_pid   INTEGER,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_update_jobs_created
    ON update_jobs(created_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_update_jobs_active
    ON update_jobs((1)) WHERE state IN ('queued', 'installing');
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateJobRecord {
    pub job_id: String,
    pub version: String,
    pub target: String,
    pub asset_name: String,
    pub sha256: String,
    pub binary_path: String,
    pub state: String,
    pub detail: Option<String>,
    pub worker_pid: Option<u32>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug)]
pub struct UpdateStore {
    conn: Connection,
}

impl UpdateStore {
    pub fn open(paths: &RuntimePaths) -> Result<Self, String> {
        ensure_real_dir(&paths.config_dir)?;
        let dir = paths.config_dir.join("update");
        ensure_real_dir(&dir)?;
        let path = dir.join("state.db");
        prepare_database_file(&path)?;
        let mut conn = Connection::open(&path)
            .map_err(|error| format!("cannot open update state database: {error}"))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|error| format!("cannot configure update state timeout: {error}"))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|error| format!("cannot configure update state database: {error}"))?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    #[cfg(test)]
    fn schema_version(&self) -> Result<i64, String> {
        read_schema_version(&self.conn)
    }

    pub fn create_update_job(&self, record: &UpdateJobRecord) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO update_jobs (
                    job_id, version, target, asset_name, sha256, binary_path,
                    state, detail, worker_pid, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    record.job_id,
                    record.version,
                    record.target,
                    record.asset_name,
                    record.sha256,
                    record.binary_path,
                    record.state,
                    bounded_detail(record.detail.as_deref()),
                    record.worker_pid.map(i64::from),
                    record.created_at,
                    record.updated_at,
                ],
            )
            .map_err(|error| format!("cannot create update job: {error}"))?;
        Ok(())
    }

    pub fn update_update_job(
        &self,
        job_id: &str,
        state: &str,
        detail: Option<&str>,
        worker_pid: Option<u32>,
        now_ms: i64,
    ) -> Result<(), String> {
        let changed = self
            .conn
            .execute(
                "UPDATE update_jobs
                 SET state = ?2, detail = ?3,
                     worker_pid = COALESCE(?4, worker_pid), updated_at = ?5
                 WHERE job_id = ?1",
                params![
                    job_id,
                    state,
                    bounded_detail(detail),
                    worker_pid.map(i64::from),
                    now_ms,
                ],
            )
            .map_err(|error| format!("cannot update update job: {error}"))?;
        if changed == 1 {
            Ok(())
        } else {
            Err(format!("update job transition updated {changed} rows"))
        }
    }

    pub fn set_update_worker_pid(
        &self,
        job_id: &str,
        worker_pid: u32,
        now_ms: i64,
    ) -> Result<(), String> {
        let changed = self
            .conn
            .execute(
                "UPDATE update_jobs SET worker_pid = ?2, updated_at = ?3 WHERE job_id = ?1",
                params![job_id, i64::from(worker_pid), now_ms],
            )
            .map_err(|error| format!("cannot record update worker pid: {error}"))?;
        if changed == 1 {
            Ok(())
        } else {
            Err(format!(
                "update worker pid transition updated {changed} rows"
            ))
        }
    }

    pub fn update_job(&self, job_id: &str) -> Result<Option<UpdateJobRecord>, String> {
        self.conn
            .query_row(
                "SELECT job_id, version, target, asset_name, sha256, binary_path,
                        state, detail, worker_pid, created_at, updated_at
                 FROM update_jobs WHERE job_id = ?1",
                [job_id],
                decode_update_job,
            )
            .optional()
            .map_err(|error| format!("cannot read update job: {error}"))
    }

    pub fn latest_update_job(&self) -> Result<Option<UpdateJobRecord>, String> {
        self.conn
            .query_row(
                "SELECT job_id, version, target, asset_name, sha256, binary_path,
                        state, detail, worker_pid, created_at, updated_at
                 FROM update_jobs ORDER BY created_at DESC LIMIT 1",
                [],
                decode_update_job,
            )
            .optional()
            .map_err(|error| format!("cannot read latest update job: {error}"))
    }

    pub fn active_update_job(&self) -> Result<Option<UpdateJobRecord>, String> {
        self.conn
            .query_row(
                "SELECT job_id, version, target, asset_name, sha256, binary_path,
                        state, detail, worker_pid, created_at, updated_at
                 FROM update_jobs
                 WHERE state IN ('queued', 'installing')
                 ORDER BY created_at DESC LIMIT 1",
                [],
                decode_update_job,
            )
            .optional()
            .map_err(|error| format!("cannot read active update job: {error}"))
    }
}

fn migrate(conn: &mut Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .map_err(|error| format!("cannot bootstrap update state meta table: {error}"))?;
    let current = read_schema_version(conn)?;
    if current > UPDATE_STORE_SCHEMA_VERSION {
        return Err(format!(
            "update state schema version {current} is newer than this binary supports ({UPDATE_STORE_SCHEMA_VERSION})"
        ));
    }
    if current == 0 {
        let tx = conn
            .transaction()
            .map_err(|error| format!("cannot begin update state migration: {error}"))?;
        tx.execute_batch(UPDATE_STORE_MIGRATION_V1)
            .map_err(|error| format!("update state migration v1 failed: {error}"))?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [META_SCHEMA_VERSION],
        )
        .map_err(|error| format!("cannot record update state schema version: {error}"))?;
        tx.commit()
            .map_err(|error| format!("cannot commit update state migration: {error}"))?;
    }
    Ok(())
}

fn read_schema_version(conn: &Connection) -> Result<i64, String> {
    let value = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [META_SCHEMA_VERSION],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("cannot read update state schema version: {error}"))?;
    match value {
        None => Ok(0),
        Some(value) => value
            .parse::<i64>()
            .map_err(|_| format!("invalid update state schema version {value:?}")),
    }
}

fn prepare_database_file(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err("update state database is not a real file".to_owned());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .map_err(|error| format!("cannot create update state database: {error}"))?;
            #[cfg(not(unix))]
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|error| format!("cannot create update state database: {error}"))?;
        }
        Err(error) => return Err(format!("cannot inspect update state database: {error}")),
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot secure update state database: {error}"))?;
    Ok(())
}

fn ensure_real_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err("update state path is not a real directory".to_owned());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|error| format!("cannot create update state directory: {error}"))?;
        }
        Err(error) => return Err(format!("cannot inspect update state directory: {error}")),
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure update state directory: {error}"))?;
    Ok(())
}

fn decode_update_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<UpdateJobRecord> {
    let worker_pid = row
        .get::<_, Option<i64>>(8)?
        .and_then(|value| u32::try_from(value).ok());
    Ok(UpdateJobRecord {
        job_id: row.get(0)?,
        version: row.get(1)?,
        target: row.get(2)?,
        asset_name: row.get(3)?,
        sha256: row.get(4)?,
        binary_path: row.get(5)?,
        state: row.get(6)?,
        detail: row.get(7)?,
        worker_pid,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn bounded_detail(value: Option<&str>) -> Option<String> {
    value.map(|value| {
        let mut text = value.to_owned();
        if text.len() > 512 {
            text.truncate(512);
        }
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn fixture() -> (std::path::PathBuf, RuntimePaths) {
        let root = std::env::temp_dir().join(format!(
            "herdr-mcp-updater-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let config_dir = root.join("config");
        let paths = RuntimePaths {
            config_file: config_dir.join("config.toml"),
            dev_state_dir: root.join("dev"),
            herdr_socket: Some(root.join("herdr.sock")),
            config_dir,
        };
        (root, paths)
    }

    fn job(id: &str, state: &str, created_at: i64) -> UpdateJobRecord {
        UpdateJobRecord {
            job_id: id.to_owned(),
            version: "9.9.9".to_owned(),
            target: "aarch64-apple-darwin".to_owned(),
            asset_name: "herdr-mcp-9.9.9-aarch64-apple-darwin".to_owned(),
            sha256: "a".repeat(64),
            binary_path: "/tmp/herdr-mcp-candidate".to_owned(),
            state: state.to_owned(),
            detail: Some("x".repeat(900)),
            worker_pid: None,
            created_at,
            updated_at: created_at,
        }
    }

    #[test]
    fn updater_state_is_isolated_durable_bounded_and_single_active() {
        let (root, paths) = fixture();
        {
            let store = UpdateStore::open(&paths).unwrap();
            assert_eq!(store.schema_version().unwrap(), 1);
            assert!(!paths.config_dir.join("state.db").exists());
            store
                .create_update_job(&job("upd-test-12345678", "queued", 10))
                .unwrap();
            assert!(
                store
                    .create_update_job(&job("upd-test-second", "queued", 11))
                    .is_err()
            );
            store
                .set_update_worker_pid("upd-test-12345678", 1234, 11)
                .unwrap();
            store
                .update_update_job(
                    "upd-test-12345678",
                    "installing",
                    Some(&"y".repeat(900)),
                    None,
                    12,
                )
                .unwrap();
            let record = store.latest_update_job().unwrap().unwrap();
            assert_eq!(record.state, "installing");
            assert_eq!(record.worker_pid, Some(1234));
            assert_eq!(record.detail.as_ref().unwrap().len(), 512);
            let url_columns = store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('update_jobs') WHERE name LIKE '%url%'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap();
            assert_eq!(url_columns, 0);
        }
        let reopened = UpdateStore::open(&paths).unwrap();
        assert_eq!(
            reopened
                .update_job("upd-test-12345678")
                .unwrap()
                .unwrap()
                .state,
            "installing"
        );
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn updater_state_future_schema_fails_closed() {
        let (root, paths) = fixture();
        {
            let store = UpdateStore::open(&paths).unwrap();
            store
                .conn
                .execute(
                    "UPDATE meta SET value = '2' WHERE key = ?1",
                    [META_SCHEMA_VERSION],
                )
                .unwrap();
        }
        assert!(UpdateStore::open(&paths).unwrap_err().contains("newer"));
        fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn updater_state_refuses_symlink_database() {
        use std::os::unix::fs::symlink;

        let (root, paths) = fixture();
        fs::create_dir_all(paths.config_dir.join("update")).unwrap();
        let outside = root.join("outside.db");
        fs::write(&outside, b"not sqlite").unwrap();
        symlink(&outside, paths.config_dir.join("update/state.db")).unwrap();
        assert!(UpdateStore::open(&paths).is_err());
        fs::remove_dir_all(root).ok();
    }
}
