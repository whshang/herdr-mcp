//! Pure heartbeat/silence scheduling policy mirrored from `src/link/client.ts`.
//!
//! The future transport owns timers and socket state. This module only answers
//! whether a tick is eligible, how often the silence watchdog should run, and
//! whether the edge has been silent long enough to recycle a connection.

use super::lifecycle::ConnectionPhase;

pub fn heartbeat_eligible(phase: ConnectionPhase, stopped: bool, socket_open: bool) -> bool {
    phase == ConnectionPhase::Online && !stopped && socket_open
}

/// Exact Node formula:
/// `max(250, min(heartbeatMs, maxSilenceMs / 3))`.
pub fn silence_check_interval_ms(heartbeat_ms: f64, max_silence_ms: f64) -> f64 {
    heartbeat_ms.min(max_silence_ms / 3.0).max(250.0)
}

/// Node chooses `lastEdgeSeenMs ?? connectedAtMs ?? 0`.
pub fn silence_base_ms(last_edge_seen_ms: Option<i64>, connected_at_ms: Option<i64>) -> i64 {
    last_edge_seen_ms.or(connected_at_ms).unwrap_or(0)
}

/// The Node watchdog uses strict `>` rather than `>=` at the boundary.
pub fn silence_expired(
    now_ms: i64,
    last_edge_seen_ms: Option<i64>,
    connected_at_ms: Option<i64>,
    max_silence_ms: i64,
) -> bool {
    now_ms - silence_base_ms(last_edge_seen_ms, connected_at_ms) > max_silence_ms
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{heartbeat_eligible, silence_base_ms, silence_check_interval_ms, silence_expired};
    use crate::link::lifecycle::ConnectionPhase;

    fn phase_from_fixture(value: &Value) -> ConnectionPhase {
        match value.as_str().expect("phase string") {
            "idle" => ConnectionPhase::Idle,
            "connecting" => ConnectionPhase::Connecting,
            "handshake" => ConnectionPhase::Handshake,
            "online" => ConnectionPhase::Online,
            "reconnecting" => ConnectionPhase::Reconnecting,
            "closing" => ConnectionPhase::Closing,
            "closed" => ConnectionPhase::Closed,
            other => panic!("unknown phase {other}"),
        }
    }

    fn optional_i64(case: &Value, key: &str) -> Option<i64> {
        match case.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::Number(value)) => value.as_i64(),
            other => panic!("invalid {key}: {other:?}"),
        }
    }

    #[test]
    fn heartbeat_requires_online_not_stopped_and_open_socket() {
        assert!(heartbeat_eligible(ConnectionPhase::Online, false, true));
        assert!(!heartbeat_eligible(ConnectionPhase::Handshake, false, true));
        assert!(!heartbeat_eligible(ConnectionPhase::Online, true, true));
        assert!(!heartbeat_eligible(ConnectionPhase::Online, false, false));
    }

    #[test]
    fn silence_interval_matches_node_formula() {
        assert_eq!(silence_check_interval_ms(15_000.0, 90_000.0), 15_000.0);
        assert_eq!(silence_check_interval_ms(60_000.0, 60.0), 250.0);
        assert_eq!(silence_check_interval_ms(500.0, 3_003.0), 500.0);
        assert_eq!(silence_check_interval_ms(5_000.0, 3_003.0), 1_001.0);
    }

    #[test]
    fn silence_base_prefers_last_edge_then_connected_then_zero() {
        assert_eq!(silence_base_ms(Some(30), Some(20)), 30);
        assert_eq!(silence_base_ms(None, Some(20)), 20);
        assert_eq!(silence_base_ms(None, None), 0);
    }

    #[test]
    fn silence_expiry_is_strictly_greater_than_budget() {
        assert!(!silence_expired(110, Some(10), Some(0), 100));
        assert!(silence_expired(111, Some(10), Some(0), 100));
        assert!(!silence_expired(100, None, None, 100));
        assert!(silence_expired(101, None, None, 100));
    }

    /// 1.2/S6 follow-up: the shipped cadence is safe, the reverted one was not.
    /// `last_edge_seen` advances only on inbound application frames (the Edge
    /// status reply each heartbeat elicits); between two heartbeats it can age
    /// at most `heartbeat_ms`. 30s/90s keeps every gap inside the budget, while
    /// the former 300s heartbeat let the base age past it and forced a recycle.
    #[test]
    fn thirty_second_heartbeat_stays_inside_silence_but_three_hundred_did_not() {
        let silence = super::super::transport::LINK_DEFAULT_MAX_SILENCE_MS;
        let shipped = super::super::transport::LINK_DEFAULT_HEARTBEAT_MS;
        assert_eq!(shipped, 30_000);

        // Connected at t=0, then one Edge status reply per heartbeat: the base
        // never ages more than one heartbeat interval, so it never expires.
        for tick in 1..20 {
            let last_seen = shipped * tick;
            let now = last_seen + shipped;
            assert!(
                !silence_expired(now, Some(last_seen), Some(0), silence),
                "30s heartbeat must never expire at tick {tick} (gap {shipped}ms < {silence}ms)"
            );
        }

        // Regression guard for the 300s configuration: three silence windows
        // pass before the first reply lands, so the watchdog recycles.
        let previous = 300_000_i64;
        assert!(
            silence_expired(previous, Some(0), Some(0), silence),
            "300s heartbeat lets last_edge_seen age to {previous}ms > {silence}ms and recycles"
        );
    }

    #[test]
    fn shared_batch5_fixture_matches_heartbeat_and_silence_oracle() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/link-lifecycle-policy-batch5.json"
        ))
        .expect("shared link lifecycle fixture");
        let heartbeat = &fixture["heartbeat_silence"];

        for case in heartbeat["heartbeat_gate"]["conditions"]
            .as_array()
            .expect("heartbeat gate")
        {
            let got = heartbeat_eligible(
                phase_from_fixture(&case["phase"]),
                case["stopped"].as_bool().expect("stopped"),
                case["socket_open"].as_bool().expect("socket_open"),
            );
            assert_eq!(got, case["should_heartbeat"].as_bool().expect("expected"));
        }

        for case in heartbeat["silence_interval"]
            .as_array()
            .expect("silence interval")
        {
            assert_eq!(
                silence_check_interval_ms(
                    case["heartbeatMs"].as_f64().expect("heartbeatMs"),
                    case["maxSilenceMs"].as_f64().expect("maxSilenceMs"),
                ),
                case["expected"].as_f64().expect("expected interval")
            );
        }

        for case in heartbeat["silence_expiry"]
            .as_array()
            .expect("silence expiry")
        {
            assert_eq!(
                silence_expired(
                    case["now"].as_i64().expect("now"),
                    optional_i64(case, "lastEdgeSeenMs"),
                    optional_i64(case, "connectedAtMs"),
                    case["maxSilenceMs"].as_i64().expect("maxSilenceMs"),
                ),
                case["expired"].as_bool().expect("expired")
            );
        }
    }
}
