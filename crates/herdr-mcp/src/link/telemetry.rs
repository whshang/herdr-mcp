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
    online: bool,
    disconnected_at_ms: Option<i64>,
}

impl Default for LinkTransportTelemetry {
    fn default() -> Self {
        Self {
            reconnect_count: 0,
            last_reconnect_duration_ms: None,
            connected_since_ms: None,
            transport_state: STATE_CONNECTING,
            online: false,
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
    pub fn record_connecting(&mut self) {
        self.transport_state = if self.online {
            STATE_RECONNECTING
        } else {
            STATE_CONNECTING
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
        self.online = true;
    }

    /// The link lost its connection; the ladder keeps retrying.
    pub fn record_disconnected(&mut self, now_ms: i64) {
        if self.disconnected_at_ms.is_none() {
            self.disconnected_at_ms = Some(now_ms);
        }
        self.connected_since_ms = None;
        self.transport_state = STATE_RECONNECTING;
        self.online = false;
    }

    /// The daemon stopped or gave up; no retry is pending.
    pub fn record_offline(&mut self) {
        self.connected_since_ms = None;
        self.transport_state = STATE_OFFLINE;
        self.online = false;
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
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temp, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temp, path)
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
