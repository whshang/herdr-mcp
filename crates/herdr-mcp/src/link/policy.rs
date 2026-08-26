//! Pure link refusal/close-code policy mirrored from `src/link/client.ts`.
//!
//! This module deliberately contains no socket I/O or credentials. It turns
//! canonical hello refusal codes and WebSocket close codes into a stable
//! reconnect-or-exit directive for the future Rust transport.

pub const WS_CLOSE_NORMAL: u16 = 1000;
pub const WS_CLOSE_GOING_AWAY: u16 = 1001;
pub const WS_CLOSE_POLICY: u16 = 1008;
pub const WS_CLOSE_SUPERSEDED: u16 = 4409;
pub const AUTH_REJECT_CLOSE_CODES: &[u16] = &[1008, 4401, 4403];
pub const DEFAULT_HELLO_ACK_REFUSAL_CODE: &str = "auth_rejected";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkExitKind {
    Stopped,
    AuthRejected,
    ContractRejected,
    Superseded,
    MaxReconnect,
    FatalError,
}

impl LinkExitKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::AuthRejected => "auth_rejected",
            Self::ContractRejected => "contract_rejected",
            Self::Superseded => "superseded",
            Self::MaxReconnect => "max_reconnect",
            Self::FatalError => "fatal_error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkDirective {
    Retry,
    Exit(LinkExitKind),
}

/// Exact parity with the Node `classifyFatalCode()` helper.
pub fn classify_fatal_code(code: &str) -> LinkExitKind {
    match code {
        "auth_rejected" | "auth_expired" | "session_invalid" => LinkExitKind::AuthRejected,
        "contract_mismatch" | "contract_rejected" | "protocol_incompatible" => {
            LinkExitKind::ContractRejected
        }
        _ => LinkExitKind::FatalError,
    }
}

/// Node `hello_ack.ok:false` defaults a missing code to `auth_rejected` before
/// classification. Only auth/contract refusal classes are fatal; unknown or
/// internal refusal codes are treated as a dropped attempt and reconnect.
pub fn classify_hello_ack_refusal(code: Option<&str>) -> LinkDirective {
    match classify_fatal_code(code.unwrap_or(DEFAULT_HELLO_ACK_REFUSAL_CODE)) {
        LinkExitKind::AuthRejected => LinkDirective::Exit(LinkExitKind::AuthRejected),
        LinkExitKind::ContractRejected => LinkDirective::Exit(LinkExitKind::ContractRejected),
        LinkExitKind::FatalError => LinkDirective::Retry,
        other => LinkDirective::Exit(other),
    }
}

/// Handshake timeout is a dropped attempt in the Node loop and therefore
/// reconnectable rather than fatal.
pub const fn classify_handshake_timeout() -> LinkDirective {
    LinkDirective::Retry
}

/// Policy applied after an established/open attempt closes.
pub fn classify_socket_close(code: u16) -> LinkDirective {
    if code == WS_CLOSE_SUPERSEDED {
        return LinkDirective::Exit(LinkExitKind::Superseded);
    }
    if AUTH_REJECT_CLOSE_CODES.contains(&code) {
        return LinkDirective::Exit(LinkExitKind::AuthRejected);
    }
    LinkDirective::Retry
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{
        AUTH_REJECT_CLOSE_CODES, DEFAULT_HELLO_ACK_REFUSAL_CODE, LinkDirective, LinkExitKind,
        WS_CLOSE_SUPERSEDED, classify_fatal_code, classify_handshake_timeout,
        classify_hello_ack_refusal, classify_socket_close,
    };

    #[test]
    fn fatal_code_helper_matches_node_mapping() {
        for code in ["auth_rejected", "auth_expired", "session_invalid"] {
            assert_eq!(classify_fatal_code(code), LinkExitKind::AuthRejected);
        }
        for code in [
            "contract_mismatch",
            "contract_rejected",
            "protocol_incompatible",
        ] {
            assert_eq!(classify_fatal_code(code), LinkExitKind::ContractRejected);
        }
        for code in ["internal_error", "unknown", ""] {
            assert_eq!(classify_fatal_code(code), LinkExitKind::FatalError);
        }
    }

    #[test]
    fn hello_ack_only_auth_or_contract_classes_stop_reconnect() {
        assert_eq!(
            classify_hello_ack_refusal(Some("auth_expired")),
            LinkDirective::Exit(LinkExitKind::AuthRejected)
        );
        assert_eq!(
            classify_hello_ack_refusal(Some("protocol_incompatible")),
            LinkDirective::Exit(LinkExitKind::ContractRejected)
        );
        assert_eq!(
            classify_hello_ack_refusal(Some("internal_error")),
            LinkDirective::Retry
        );
        assert_eq!(
            classify_hello_ack_refusal(Some("future_code")),
            LinkDirective::Retry
        );
        assert_eq!(
            classify_hello_ack_refusal(None),
            LinkDirective::Exit(LinkExitKind::AuthRejected)
        );
        assert_eq!(classify_handshake_timeout(), LinkDirective::Retry);
    }

    #[test]
    fn close_code_policy_matches_node_fencing_and_auth_rules() {
        assert_eq!(
            classify_socket_close(WS_CLOSE_SUPERSEDED),
            LinkDirective::Exit(LinkExitKind::Superseded)
        );
        for code in AUTH_REJECT_CLOSE_CODES {
            assert_eq!(
                classify_socket_close(*code),
                LinkDirective::Exit(LinkExitKind::AuthRejected)
            );
        }
        for code in [1000, 1001, 1006, 4000] {
            assert_eq!(classify_socket_close(code), LinkDirective::Retry);
        }
    }

    #[test]
    fn shared_batch5_fixture_matches_policy_oracle() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/link-lifecycle-policy-batch5.json"
        ))
        .expect("shared link lifecycle fixture");

        for case in fixture["hello_ack_classification"]
            .as_array()
            .expect("hello_ack_classification")
        {
            assert_eq!(case["oracle"].as_str(), Some("node_parity"));
            let code = case.get("code").and_then(Value::as_str);
            let effective_code = code.unwrap_or(DEFAULT_HELLO_ACK_REFUSAL_CODE);
            assert_eq!(
                classify_fatal_code(effective_code).as_str(),
                case["classify"].as_str().expect("classify")
            );

            let directive = classify_hello_ack_refusal(code);
            let expected_fatal = case["fatal"].as_bool().expect("fatal");
            let expected_reconnect = case["reconnect"].as_bool().expect("reconnect");
            assert_eq!(expected_reconnect, !expected_fatal);
            if expected_fatal {
                let LinkDirective::Exit(kind) = directive else {
                    panic!("expected fatal directive for {effective_code}");
                };
                assert_eq!(kind.as_str(), case["classify"].as_str().expect("classify"));
            } else {
                assert_eq!(directive, LinkDirective::Retry);
            }
        }

        assert_eq!(classify_handshake_timeout(), LinkDirective::Retry);

        for case in fixture["socket_close_policy"]
            .as_array()
            .expect("socket_close_policy")
        {
            assert_eq!(case["oracle"].as_str(), Some("node_parity"));
            let code = case["code"].as_u64().expect("close code") as u16;
            let directive = classify_socket_close(code);
            let expected_fatal = case["fatal"].as_bool().expect("fatal");
            let expected_reconnect = case["reconnect"].as_bool().expect("reconnect");
            assert_eq!(expected_reconnect, !expected_fatal);
            match (directive, expected_fatal) {
                (LinkDirective::Retry, false) => {}
                (LinkDirective::Exit(kind), true) => {
                    assert_eq!(Some(kind.as_str()), case["exit_kind"].as_str());
                }
                (other, expected) => {
                    panic!("close {code}: {other:?}, expected fatal={expected}")
                }
            }
        }
    }
}
