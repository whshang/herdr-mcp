//! Read-only WebChat control-plane snapshot for doctor/status (issue #772).
//!
//! Two independent mechanisms gate WebChat work, and diagnosing an unbound
//! dispatch requires telling them apart:
//!
//! 1. The Edge `webchat_control` grant (header
//!    `x-herdr-edge-webchat-control-grants`, set via
//!    `herdr_mcp.connector.webchat_control.set`). Its absence makes binding
//!    fail with `grant_missing`. The grant itself is Edge-side and is not
//!    stored locally; the local state DB only records *evidence* that grants
//!    existed: connector-attributed dispatch rows
//!    (`browser_dispatches.authorization_*`) and caller session bindings
//!    (`browser_caller_sessions`). This module surfaces that evidence as
//!    grant state `present` / `absent` / `unknown`.
//! 2. The extension control-center consent flags
//!    (`browser_endpoints.webchat_control_allowed` etc. plus
//!    `consent_revision`). These gate browser control, not source-turn
//!    binding.
//!
//! Read-only contract: only `SELECT`s against the local state DB; if the DB or
//! the tables are missing the snapshot reports `unknown` and never fails
//! doctor. No tokens, credential values, or grant payloads are ever read or
//! printed — only opaque refs, connector ids, booleans, revisions, and
//! timestamps.

use crate::paths::RuntimePaths;
use serde_json::{Value, json};
use std::path::Path;

const MAX_ENDPOINTS: usize = 8;
const MAX_PRINCIPALS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantState {
    /// Local evidence proves at least one Edge webchat_control grant existed
    /// for the connector at some point.
    Present,
    /// The state DB is healthy but carries no connector-attributed grant
    /// evidence.
    Absent,
    /// The DB or its tables could not be read; no claim is made.
    Unknown,
}

impl GrantState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrantPrincipal {
    pub connector_id: String,
    pub grant_generation: Option<i64>,
    pub last_seen: Option<i64>,
    pub bound_caller_sessions: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EndpointConsent {
    pub endpoint_ref: String,
    pub browser_family: String,
    pub webchat_control_allowed: bool,
    pub tool_bridge_allowed: bool,
    pub tool_bridge_mutation_allowed: bool,
    pub consent_revision: i64,
    pub last_observed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebChatGrantSnapshot {
    pub state: GrantState,
    /// Error reason when `state == Unknown`.
    pub reason: Option<String>,
    pub principals: Vec<GrantPrincipal>,
    pub endpoints: Vec<EndpointConsent>,
}

impl WebChatGrantSnapshot {
    pub(crate) fn unknown(reason: String) -> Self {
        Self {
            state: GrantState::Unknown,
            reason: Some(reason),
            principals: Vec::new(),
            endpoints: Vec::new(),
        }
    }

    pub(crate) fn as_json(&self) -> Value {
        let principals = self
            .principals
            .iter()
            .map(|principal| {
                json!({
                    "connector_id": principal.connector_id,
                    "grant_generation": principal.grant_generation,
                    "last_seen": principal.last_seen,
                    "bound_caller_sessions": principal.bound_caller_sessions,
                })
            })
            .collect::<Vec<_>>();
        let endpoints = self
            .endpoints
            .iter()
            .map(|endpoint| {
                json!({
                    "endpoint_ref": endpoint.endpoint_ref,
                    "browser_family": endpoint.browser_family,
                    "consent": {
                        "webchat_control": endpoint.webchat_control_allowed,
                        "tool_bridge": endpoint.tool_bridge_allowed,
                        "tool_bridge_mutation": endpoint.tool_bridge_mutation_allowed,
                    },
                    "consent_revision": endpoint.consent_revision,
                    "last_observed_at": endpoint.last_observed_at,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "state": self.state.as_str(),
            "reason": self.reason,
            "principals": principals,
            "endpoints": endpoints,
        })
    }

    pub(crate) fn doctor_line(&self, hint: &str) -> String {
        match self.state {
            GrantState::Unknown => format!(
                "LAYER webchat-grants state=unknown reason={} {hint}",
                self.reason.as_deref().unwrap_or("unavailable")
            ),
            GrantState::Absent => {
                let mut line = format!("LAYER webchat-grants state=absent principals=0 {hint}");
                for endpoint in &self.endpoints {
                    line.push_str(&format!(
                        "\nLAYER webchat-consent endpoint_ref={} browser={} webchat_control={} tool_bridge={} tool_bridge_mutation={} consent_revision={} last_observed_at={}",
                        endpoint.endpoint_ref,
                        endpoint.browser_family,
                        endpoint.webchat_control_allowed,
                        endpoint.tool_bridge_allowed,
                        endpoint.tool_bridge_mutation_allowed,
                        endpoint.consent_revision,
                        endpoint.last_observed_at,
                    ));
                }
                line
            }
            GrantState::Present => {
                let mut line = format!(
                    "LAYER webchat-grants state=present principals={} {hint}",
                    self.principals.len(),
                );
                for principal in &self.principals {
                    line.push_str(&format!(
                        "\nLAYER webchat-grant principal={} grant_generation={} last_seen={} bound_caller_sessions={}",
                        principal.connector_id,
                        principal
                            .grant_generation
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "unknown".to_owned()),
                        principal
                            .last_seen
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "unknown".to_owned()),
                        principal.bound_caller_sessions,
                    ));
                }
                for endpoint in &self.endpoints {
                    line.push_str(&format!(
                        "\nLAYER webchat-consent endpoint_ref={} browser={} webchat_control={} tool_bridge={} tool_bridge_mutation={} consent_revision={} last_observed_at={}",
                        endpoint.endpoint_ref,
                        endpoint.browser_family,
                        endpoint.webchat_control_allowed,
                        endpoint.tool_bridge_allowed,
                        endpoint.tool_bridge_mutation_allowed,
                        endpoint.consent_revision,
                        endpoint.last_observed_at,
                    ));
                }
                line
            }
        }
    }
}

/// One-line summary used by `status --verbose` and the doctor layer rollup.
pub(crate) fn summary_line(
    snapshot: &WebChatGrantSnapshot,
    language: crate::locale::Locale,
) -> String {
    let label = language.text("WebChat grants", "WebChat 授权", "WebChat 認可");
    match snapshot.state {
        GrantState::Present => format!(
            "{label}: present · principals={} · missing Edge grant causes binding grant_missing",
            snapshot.principals.len(),
        ),
        GrantState::Absent => format!(
            "{label}: absent · {} · extension consent gates browser control",
            language.text(
                "binding would fail with grant_missing",
                "绑定将报 grant_missing",
                "バインディングは grant_missing で失敗する",
            ),
        ),
        GrantState::Unknown => format!(
            "{label}: unknown · {}",
            snapshot.reason.as_deref().unwrap_or("unavailable"),
        ),
    }
}

/// The distinguishing hint: a missing Edge grant fails source-turn binding
/// with `grant_missing`, while extension consent gates browser control.
pub(crate) fn binding_hint(language: crate::locale::Locale) -> String {
    language
        .text(
            "missing Edge grant causes binding grant_missing; extension consent gates browser control",
            "缺少 Edge grant 会使绑定报 grant_missing；扩展 consent 决定浏览器控制",
            "Edge grant の欠落はバインディングの grant_missing を招く。拡張 consent はブラウザー制御をゲートする",
        )
        .to_owned()
}

pub(crate) fn snapshot(paths: &RuntimePaths) -> WebChatGrantSnapshot {
    snapshot_at_db_path(&paths.config_dir.join("state.db"))
}

pub(crate) fn snapshot_at_db_path(db_path: &Path) -> WebChatGrantSnapshot {
    if !db_path.is_file() {
        return WebChatGrantSnapshot::unknown("state_db_missing".to_owned());
    }
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(conn) => conn,
        Err(error) => {
            return WebChatGrantSnapshot::unknown(format!("state_db_open_failed: {error}"));
        }
    };
    // Read existing tables only. A query against a table that predates the
    // runtime (or a future runtime whose schema differs) degrades to unknown.
    let principals = match read_grant_principals(&conn) {
        Ok(principals) => principals,
        Err(reason) => return WebChatGrantSnapshot::unknown(reason),
    };
    let endpoints = match read_endpoint_consent(&conn) {
        Ok(endpoints) => endpoints,
        Err(reason) => return WebChatGrantSnapshot::unknown(reason),
    };
    let state = if principals.is_empty() {
        GrantState::Absent
    } else {
        GrantState::Present
    };
    WebChatGrantSnapshot {
        state,
        reason: None,
        principals,
        endpoints,
    }
}

/// Grant evidence: connector-attributed dispatches. `connector_id` and
/// `principal_ref` are grant principals, not secrets; no token or payload
/// column is selected.
fn read_grant_principals(conn: &rusqlite::Connection) -> Result<Vec<GrantPrincipal>, String> {
    if !table_exists(conn, "browser_dispatches") || !table_exists(conn, "browser_caller_sessions") {
        return Err("grant_tables_missing".to_owned());
    }
    let mut stmt = conn
        .prepare(
            "SELECT authorization_connector_id,
                    MAX(authorization_grant_generation),
                    MAX(updated_at)
             FROM browser_dispatches
             WHERE authorization_connector_id IS NOT NULL
             GROUP BY authorization_connector_id
             ORDER BY MAX(updated_at) DESC
             LIMIT ?1",
        )
        .map_err(|error| format!("grant_query_failed: {error}"))?;
    let rows = stmt
        .query_map([MAX_PRINCIPALS as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .map_err(|error| format!("grant_query_failed: {error}"))?;
    let mut principals = Vec::new();
    for row in rows {
        let (connector_id, grant_generation, last_seen) =
            row.map_err(|error| format!("grant_query_failed: {error}"))?;
        let bound = conn
            .query_row(
                "SELECT COUNT(*) FROM browser_caller_sessions WHERE principal_ref = ?1",
                rusqlite::params![format!("connector:{connector_id}")],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("grant_query_failed: {error}"))
            .unwrap_or(0);
        principals.push(GrantPrincipal {
            connector_id,
            grant_generation,
            last_seen,
            bound_caller_sessions: bound,
        });
    }
    Ok(principals)
}

fn read_endpoint_consent(conn: &rusqlite::Connection) -> Result<Vec<EndpointConsent>, String> {
    if !table_exists(conn, "browser_endpoints") {
        return Err("endpoint_table_missing".to_owned());
    }
    let mut stmt = conn
        .prepare(
            "SELECT endpoint_ref, browser_family, webchat_control_allowed,
                    tool_bridge_allowed, tool_bridge_mutation_allowed,
                    consent_revision, last_observed_at
             FROM browser_endpoints
             ORDER BY last_observed_at DESC, endpoint_ref
             LIMIT ?1",
        )
        .map_err(|error| format!("consent_query_failed: {error}"))?;
    let rows = stmt
        .query_map([MAX_ENDPOINTS as i64], |row| {
            Ok(EndpointConsent {
                endpoint_ref: row.get(0)?,
                browser_family: row.get(1)?,
                webchat_control_allowed: row.get::<_, i64>(2)? != 0,
                tool_bridge_allowed: row.get::<_, i64>(3)? != 0,
                tool_bridge_mutation_allowed: row.get::<_, i64>(4)? != 0,
                consent_revision: row.get(5)?,
                last_observed_at: row.get(6)?,
            })
        })
        .map_err(|error| format!("consent_query_failed: {error}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("consent_query_failed: {error}"))
}

fn table_exists(conn: &rusqlite::Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        rusqlite::params![table],
        |_| Ok(()),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_store::{
        BrowserDispatchAuthorizationInput, BrowserDispatchReservation, BrowserDispatchReserveInput,
        BrowserEndpointConsentInput, BrowserEndpointRegistrationInput,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_db_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-mcp-webchat-grants-{}-{}.sqlite",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn sha256_text(text: &str) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(text.as_bytes()))
    }

    fn register_endpoint(store: &mut crate::state_store::StateStore) -> String {
        let endpoint = store
            .register_browser_endpoint(BrowserEndpointRegistrationInput {
                device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                profile_seed: "webchat-grants-profile-seed-01",
                browser_family: "chrome",
                extension_version: "0.1.229",
                observed_at: 1,
            })
            .unwrap();
        endpoint.endpoint_ref
    }

    fn reserve_authorized_dispatch(
        store: &mut crate::state_store::StateStore,
        endpoint_ref: &str,
        connector_id: &str,
    ) {
        store
            .observe_browser_provider(crate::state_store::BrowserProviderObservationInput {
                endpoint_ref,
                provider: "chatgpt",
                adapter_protocol_version: 1,
                observation_generation: 1,
                capabilities_json: r#"{"operations":["composer.submit"]}"#,
                observed_at: 2,
            })
            .unwrap();
        let account = store
            .observe_browser_resource(crate::state_store::BrowserResourceObservationInput {
                endpoint_ref,
                provider: "chatgpt",
                kind: "account",
                parent_ref: None,
                native_identity: "native-identity-webchat-grants-account",
                display_label: None,
                observation_generation: 1,
                observed_at: 2,
            })
            .unwrap();
        let session = store
            .observe_browser_resource(crate::state_store::BrowserResourceObservationInput {
                endpoint_ref,
                provider: "chatgpt",
                kind: "session",
                parent_ref: Some(&account.resource_ref),
                native_identity: "native-identity-webchat-grants-session",
                display_label: None,
                observation_generation: 1,
                observed_at: 3,
            })
            .unwrap();
        let principal_ref = format!("connector:{connector_id}");
        let required_apps: [&str; 0] = [];
        let request_digest = sha256_text("webchat-grants-request");
        let message_digest = sha256_text("webchat-grants-message");
        let key_digest = sha256_text("webchat-grants-key");
        let BrowserDispatchReservation::Reserved(_) = store
            .reserve_browser_dispatch(BrowserDispatchReserveInput {
                endpoint_ref,
                provider: "chatgpt",
                operation: "browser_dispatch.submit",
                target_session_ref: &session.resource_ref,
                request_digest: &request_digest,
                message_digest: &message_digest,
                message_canonical_digest: None,
                reasoning_effort: None,
                required_apps: &required_apps,
                expected_generation: 1,
                idempotency_key_digest: &key_digest,
                parent_dispatch_id: None,
                authorization: Some(BrowserDispatchAuthorizationInput {
                    principal_ref: &principal_ref,
                    connector_id,
                    grant_generation: 3,
                }),
                work_chain_id: None,
                lane_id: None,
                created_at: 10,
            })
            .unwrap()
        else {
            panic!("expected new reservation");
        };
    }

    #[test]
    fn grant_present_reports_principals_and_consent() {
        let path = temp_db_path();
        let mut store = crate::state_store::StateStore::open(&path).unwrap();
        let endpoint_ref = register_endpoint(&mut store);
        store
            .set_browser_endpoint_consent(BrowserEndpointConsentInput {
                endpoint_ref: &endpoint_ref,
                expected_revision: 0,
                webchat_control_allowed: true,
                tool_bridge_allowed: false,
                tool_bridge_mutation_allowed: false,
                observed_at: 20,
            })
            .unwrap();
        reserve_authorized_dispatch(&mut store, &endpoint_ref, "conn_webchatGrant01");
        drop(store);

        let snapshot = snapshot_at_db_path(&path);
        assert_eq!(snapshot.state, GrantState::Present);
        assert_eq!(snapshot.reason, None);
        assert_eq!(snapshot.principals.len(), 1);
        let principal = &snapshot.principals[0];
        assert_eq!(principal.connector_id, "conn_webchatGrant01");
        assert_eq!(principal.grant_generation, Some(3));
        assert_eq!(principal.last_seen, Some(10));
        assert_eq!(principal.bound_caller_sessions, 0);
        assert_eq!(snapshot.endpoints.len(), 1);
        let endpoint = &snapshot.endpoints[0];
        // The per-endpoint WebChat switch was retired in 1.1; consent writes
        // only move Tool Bridge flags. The row still reports its stored flag.
        assert!(!endpoint.webchat_control_allowed);
        assert!(!endpoint.tool_bridge_allowed);
        assert_eq!(endpoint.consent_revision, 1);

        let json = snapshot.as_json();
        assert_eq!(json["state"], "present");
        assert_eq!(json["principals"][0]["connector_id"], "conn_webchatGrant01");
        assert_eq!(json["endpoints"][0]["consent"]["webchat_control"], false);
        assert_eq!(json["endpoints"][0]["consent_revision"], 1);

        let line = snapshot.doctor_line("hint");
        assert!(line.contains("state=present"));
        assert!(line.contains("conn_webchatGrant01"));
        assert!(line.contains("webchat_control=false"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn grant_absent_when_db_has_no_connector_evidence() {
        let path = temp_db_path();
        let mut store = crate::state_store::StateStore::open(&path).unwrap();
        let endpoint_ref = register_endpoint(&mut store);
        store
            .set_browser_endpoint_consent(BrowserEndpointConsentInput {
                endpoint_ref: &endpoint_ref,
                expected_revision: 0,
                webchat_control_allowed: false,
                tool_bridge_allowed: false,
                tool_bridge_mutation_allowed: false,
                observed_at: 20,
            })
            .unwrap();
        drop(store);

        let snapshot = snapshot_at_db_path(&path);
        assert_eq!(snapshot.state, GrantState::Absent);
        assert!(snapshot.principals.is_empty());
        assert_eq!(snapshot.endpoints.len(), 1);
        assert!(!snapshot.endpoints[0].webchat_control_allowed);
        assert_eq!(snapshot.as_json()["state"], "absent");
        assert!(snapshot.doctor_line("hint").contains("state=absent"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_when_db_missing_or_table_absent() {
        let missing = temp_db_path();
        let snapshot = snapshot_at_db_path(&missing);
        assert_eq!(snapshot.state, GrantState::Unknown);
        assert_eq!(snapshot.reason.as_deref(), Some("state_db_missing"));
        assert_eq!(snapshot.as_json()["state"], "unknown");
        assert!(snapshot.doctor_line("hint").contains("state=unknown"));

        // A DB file without the grant tables (e.g. some other sqlite file)
        // degrades to unknown instead of failing.
        let foreign = temp_db_path();
        let conn = rusqlite::Connection::open(&foreign).unwrap();
        conn.execute_batch("CREATE TABLE unrelated (id INTEGER);")
            .unwrap();
        drop(conn);
        let snapshot = snapshot_at_db_path(&foreign);
        assert_eq!(snapshot.state, GrantState::Unknown);
        assert_eq!(snapshot.reason.as_deref(), Some("grant_tables_missing"));
        let _ = std::fs::remove_file(&foreign);
    }

    #[test]
    fn output_never_contains_secret_material() {
        // Bind a caller session so the snapshot has maximum surface, then
        // assert nothing secret-like leaks. All printed values are opaque
        // refs, connector ids, booleans, integers, and timestamps.
        let path = temp_db_path();
        let mut store = crate::state_store::StateStore::open(&path).unwrap();
        let endpoint_ref = register_endpoint(&mut store);
        reserve_authorized_dispatch(&mut store, &endpoint_ref, "conn_auditReveal01");
        drop(store);

        let snapshot = snapshot_at_db_path(&path);
        let json_text = snapshot.as_json().to_string();
        let line = snapshot.doctor_line("hint");
        for rendered in [json_text, line] {
            assert!(!rendered.contains("token"));
            assert!(!rendered.contains("secret"));
            assert!(!rendered.contains("credential"));
            assert!(!rendered.contains("bearer"));
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn binding_hint_distinguishes_grant_from_consent() {
        let hint = binding_hint(crate::locale::Locale::En);
        assert!(hint.contains("grant_missing"));
        assert!(hint.contains("consent"));
    }
}
