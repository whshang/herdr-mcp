//! Link transport telemetry for 1.2 S0.
//!
//! The Edge already tracks reconnect count, last reconnect duration and
//! connected-since per workstation; nothing local exposed them. The link daemon
//! maintains these counters in-process, publishes them to
//! `<config dir>/link-transport-telemetry.json`, and `link status` / `doctor`
//! read that file.
//!
//! Security: only a state label, timestamps, durations and counters are stored.
//! Never params, payload bytes, credentials or any other secret material.

use serde_json::{Value, json};
use std::path::Path;

/// Telemetry file published next to `runtime-control.json` in the config dir.
pub const TELEMETRY_FILE: &str = "link-transport-telemetry.json";
/// Document revision; bumped when the published field set changes.
pub const SCHEMA_VERSION: u64 = 1;

pub const STATE_CONNECTING: &str = "connecting";
pub const STATE_ONLINE: &str = "online";
pub const STATE_RECONNECTING: &str = "reconnecting";
pub const STATE_OFFLINE: &str = "offline";

/// One link process's transport counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTransportTelemetry {
    reconnect_count: u64,
    last_reconnect_duration_ms: Option<u64>,
    connected_since_ms: Option<i64>,
    transport_state: &'static str,
    // True once a handshake has completed at least once. `connecting` is
    // reserved for the very first attempt: after any successful handshake, a
    // retry is a *re*connect even while the link is not currently online.
    ever_online: bool,
    disconnected_at_ms: Option<i64>,
}

impl Default for LinkTransportTelemetry {
    fn default() -> Self {
        Self {
            reconnect_count: 0,
            last_reconnect_duration_ms: None,
            connected_since_ms: None,
            transport_state: STATE_CONNECTING,
            ever_online: false,
            disconnected_at_ms: None,
        }
    }
}

impl LinkTransportTelemetry {
    pub fn reconnect_count(&self) -> u64 {
        self.reconnect_count
    }

    pub fn last_reconnect_duration_ms(&self) -> Option<u64> {
        self.last_reconnect_duration_ms
    }

    pub fn connected_since_ms(&self) -> Option<i64> {
        self.connected_since_ms
    }

    pub fn transport_state(&self) -> &'static str {
        self.transport_state
    }

    /// The daemon is trying to establish (or re-establish) the link.
    ///
    /// `connecting` is reserved for the first attempt of this process. The
    /// production ordering is `Disconnected` (state → reconnecting) followed by
    /// `ScheduleReconnect` → this method, so deciding from "currently online"
    /// would always report `connecting` during a real reconnect window. The
    /// decision therefore comes from "have we ever been online", never from the
    /// momentary online flag.
    pub fn record_connecting(&mut self) {
        let first_attempt = !self.ever_online && self.disconnected_at_ms.is_none();
        self.transport_state = if first_attempt {
            STATE_CONNECTING
        } else {
            STATE_RECONNECTING
        };
    }

    /// A handshake completed: the link is online. When a disconnect was recorded
    /// since the previous online state this counts as one reconnect and records
    /// how long the outage lasted. `reconnect_count` therefore only ever grows.
    pub fn record_online(&mut self, now_ms: i64) {
        if let Some(disconnected_at) = self.disconnected_at_ms.take() {
            self.reconnect_count = self.reconnect_count.saturating_add(1);
            self.last_reconnect_duration_ms =
                Some(now_ms.saturating_sub(disconnected_at).max(0) as u64);
        }
        self.connected_since_ms = Some(now_ms);
        self.transport_state = STATE_ONLINE;
        self.ever_online = true;
    }

    /// The link lost its connection; the ladder keeps retrying.
    pub fn record_disconnected(&mut self, now_ms: i64) {
        if self.disconnected_at_ms.is_none() {
            self.disconnected_at_ms = Some(now_ms);
        }
        self.connected_since_ms = None;
        self.transport_state = STATE_RECONNECTING;
    }

    /// The daemon stopped or gave up; no retry is pending.
    pub fn record_offline(&mut self) {
        self.connected_since_ms = None;
        self.transport_state = STATE_OFFLINE;
    }

    pub fn to_json(&self, updated_at_ms: i64) -> Value {
        json!({
            "schema_version": SCHEMA_VERSION,
            "reconnect_count": self.reconnect_count,
            "last_reconnect_duration_ms": self.last_reconnect_duration_ms,
            "connected_since_ms": self.connected_since_ms,
            "transport_state": self.transport_state,
            "updated_at_ms": updated_at_ms,
        })
    }
}

/// Publish the counters atomically with owner-only permissions. A failed
/// publish never affects the transport loop; callers ignore the error.
pub fn publish(
    path: &Path,
    telemetry: &LinkTransportTelemetry,
    updated_at_ms: i64,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body =
        serde_json::to_vec(&telemetry.to_json(updated_at_ms)).unwrap_or_else(|_| b"{}".to_vec());
    // A per-publish temp name keeps repeated publications independent.
    let sequence = TEMP_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = path.with_extension(format!("tmp-{}-{sequence}", std::process::id()));
    std::fs::write(&temp, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    }
    replace_file(&temp, path)
}

static TEMP_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Replace `target` with `temp` atomically.
///
/// `std::fs::rename` cannot replace an existing destination on Windows, so the
/// second publish of a state file would fail there (and the transport loop
/// treats a publish failure as best-effort noise). On Windows use
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`, which is the documented atomic
/// replace for a same-volume file, instead of a delete-then-rename window.
fn replace_file(temp: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };

        let wide = |path: &Path| {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let from = wide(temp);
        let to = wide(target);
        let replaced = unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if replaced == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(temp, target)
    }
}

/// Read the last published counters. Returns `None` when the daemon has not
/// published yet or the file is unreadable/invalid.
pub fn read(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_are_monotonic_across_injected_reconnects() {
        let mut telemetry = LinkTransportTelemetry::default();
        assert_eq!(telemetry.transport_state(), STATE_CONNECTING);
        assert_eq!(telemetry.reconnect_count(), 0);

        // First connection: online, but no reconnect happened yet.
        telemetry.record_online(1_000);
        assert_eq!(telemetry.transport_state(), STATE_ONLINE);
        assert_eq!(telemetry.connected_since_ms(), Some(1_000));
        assert_eq!(telemetry.reconnect_count(), 0);
        assert_eq!(telemetry.last_reconnect_duration_ms(), None);

        // Injected outage 1: disconnected then online again after 250 ms.
        telemetry.record_disconnected(2_000);
        assert_eq!(telemetry.transport_state(), STATE_RECONNECTING);
        assert_eq!(telemetry.connected_since_ms(), None);
        telemetry.record_online(2_250);
        assert_eq!(telemetry.reconnect_count(), 1);
        assert_eq!(telemetry.last_reconnect_duration_ms(), Some(250));
        assert_eq!(telemetry.connected_since_ms(), Some(2_250));

        // Repeated disconnects without a new online do not double count.
        telemetry.record_disconnected(3_000);
        telemetry.record_disconnected(3_500);
        telemetry.record_online(4_000);
        assert_eq!(telemetry.reconnect_count(), 2);
        assert_eq!(telemetry.last_reconnect_duration_ms(), Some(1_000));

        // Clock skew never produces a negative duration.
        telemetry.record_disconnected(9_000);
        telemetry.record_online(8_000);
        assert_eq!(telemetry.last_reconnect_duration_ms(), Some(0));
        assert_eq!(telemetry.reconnect_count(), 3);

        telemetry.record_offline();
        assert_eq!(telemetry.transport_state(), STATE_OFFLINE);
        assert_eq!(
            telemetry.reconnect_count(),
            3,
            "offline never resets counters"
        );
    }

    /// P1 regression (#952 review): the production order is
    /// `Disconnected` → `ScheduleReconnect`. Deciding the state from "currently
    /// online" reported `connecting` for every real reconnect window.
    #[test]
    fn reconnect_window_reports_reconnecting_in_production_order() {
        let mut telemetry = LinkTransportTelemetry::default();
        // First attempt: nothing has ever been online.
        telemetry.record_connecting();
        assert_eq!(telemetry.transport_state(), STATE_CONNECTING);

        telemetry.record_online(1_000);
        assert_eq!(telemetry.transport_state(), STATE_ONLINE);

        // Exact io_loop order: Disconnected, then ScheduleReconnect.
        telemetry.record_disconnected(2_000);
        assert_eq!(telemetry.transport_state(), STATE_RECONNECTING);
        telemetry.record_connecting();
        assert_eq!(
            telemetry.transport_state(),
            STATE_RECONNECTING,
            "a reconnect window must never be reported as the first attempt"
        );

        // Repeated retries during one outage keep the reconnect label.
        telemetry.record_connecting();
        assert_eq!(telemetry.transport_state(), STATE_RECONNECTING);

        telemetry.record_online(3_000);
        assert_eq!(telemetry.transport_state(), STATE_ONLINE);
        assert_eq!(telemetry.reconnect_count(), 1);

        // A second outage behaves the same way.
        telemetry.record_disconnected(4_000);
        telemetry.record_connecting();
        assert_eq!(telemetry.transport_state(), STATE_RECONNECTING);
        telemetry.record_online(4_500);
        assert_eq!(telemetry.reconnect_count(), 2);
        assert_eq!(telemetry.transport_state(), STATE_ONLINE);
    }

    /// P1 regression (#952 review): Windows cannot `rename` over an existing
    /// destination, so the second publication of the state file must still
    /// succeed and be observable.
    #[test]
    fn repeated_publication_replaces_the_existing_document() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-link-telemetry-repeat-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join(TELEMETRY_FILE);

        let mut telemetry = LinkTransportTelemetry::default();
        telemetry.record_online(10);
        publish(&path, &telemetry, 10).unwrap();
        assert_eq!(read(&path).unwrap()["transport_state"], json!(STATE_ONLINE));

        telemetry.record_disconnected(20);
        publish(&path, &telemetry, 20).unwrap();
        let document = read(&path).unwrap();
        assert_eq!(document["transport_state"], json!(STATE_RECONNECTING));
        assert_eq!(document["connected_since_ms"], Value::Null);
        assert_eq!(document["updated_at_ms"], json!(20));

        telemetry.record_connecting();
        telemetry.record_online(35);
        publish(&path, &telemetry, 35).unwrap();
        let document = read(&path).unwrap();
        assert_eq!(document["transport_state"], json!(STATE_ONLINE));
        assert_eq!(document["reconnect_count"], json!(1));
        assert_eq!(document["last_reconnect_duration_ms"], json!(15));

        // No temp files are left behind next to the published document.
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("tmp-"))
            .count();
        assert_eq!(leftovers, 0, "every publication cleans up its temp file");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn published_document_is_owner_only_and_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-link-telemetry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join(TELEMETRY_FILE);
        let mut telemetry = LinkTransportTelemetry::default();
        telemetry.record_online(10);
        telemetry.record_disconnected(20);
        telemetry.record_online(25);
        publish(&path, &telemetry, 30).unwrap();

        let document = read(&path).unwrap();
        assert_eq!(document["schema_version"], json!(SCHEMA_VERSION));
        assert_eq!(document["reconnect_count"], json!(1));
        assert_eq!(document["last_reconnect_duration_ms"], json!(5));
        assert_eq!(document["connected_since_ms"], json!(25));
        assert_eq!(document["transport_state"], json!("online"));
        assert_eq!(document["updated_at_ms"], json!(30));
        assert!(document.get("params").is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "telemetry must not be world readable");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_or_invalid_telemetry_reads_as_absent() {
        let missing = std::env::temp_dir().join("herdr-link-telemetry-absent.json");
        assert!(read(&missing).is_none());
    }
}
