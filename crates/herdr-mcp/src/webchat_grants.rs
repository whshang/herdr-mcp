//! Read-only WebChat control-plane snapshot for doctor/status (issues #772,
//! #791).
//!
//! One mechanism gates WebChat source-turn binding: the **Edge**
//! `webchat_control` grant (header `x-herdr-edge-webchat-control-grants`, set
//! via `herdr_mcp.connector.webchat_control.set` / `herdr-mcp connector
//! webchat-control`). Its absence makes binding fail with `grant_missing`.
//!
//! The grant lives on Edge. This runtime has no read path for it — `POST
//! /connectors/webchat-control` is write-only and the `GET /connectors`
//! inventory carries no grant rows — so doctor stays offline and credential
//! free. What the local state DB holds is only *evidence* that a grant was
//! once used: connector-attributed dispatch rows
//! (`browser_dispatches.authorization_*`) and caller session bindings
//! (`browser_caller_sessions`). The snapshot therefore reports
//! `present` (local evidence proves a grant existed), `absent_locally` (no
//! local evidence — Edge may still hold a grant this tool cannot see), or
//! `unknown` (the DB or its tables cannot be read). Only `present` is ever
//! rendered as evidence; the other states make the Edge authority explicit and
//! print the owner-device grant command instead of guessing.
//!
//! The per-endpoint `browser_endpoints.webchat_control_allowed` column is a
//! **retired** local switch (`e44688a0`): registered WebChat endpoints are
//! controllable by default. It is reported as `webchat_control=legacy_ignored`
//! and never presented as a gate, so a stale `0`/`consent_revision=0` row can
//! no longer be mistaken for the reason a binding fails. The Tool Bridge flags
//! are separate live gates for a different feature and stay visible.
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
const MAX_GRANT_ROWS: usize = 32;

/// The owner-device command that creates the Edge grant this runtime cannot
/// read back (#791). Printed whenever no local evidence exists, because the
/// retired extension WebChat Control switch is not a valid next step.
pub(crate) const CONNECTOR_GRANT_COMMAND: &str = "herdr-mcp connector webchat-control allow <connector-id> <device-id> <endpoint_ref> <provider> --confirm";
/// Device-wide variant of [`CONNECTOR_GRANT_COMMAND`] for every endpoint of a
/// provider on one device. This is a secondary option, not the default: it
/// needs an Edge that accepts `scope: "device"` (#713). A Worker without that
/// support rejects the body with `device_grant_requires_edge_support` and
/// writes nothing, so the per-endpoint command above always leads.
pub(crate) const DEVICE_WIDE_GRANT_COMMAND: &str = "herdr-mcp connector webchat-control allow-device <connector-id-or-platform-account> <device-id> --confirm";
/// Why the device-wide form is offered last. Named next to the command in both
/// text and JSON so an operator on a pre-#713 Edge does not read the failure as
/// a grant problem.
pub(crate) const DEVICE_WIDE_GRANT_REQUIRES: &str = "#713 / Edge device-scope (scope=device) support; otherwise device_grant_requires_edge_support and no grant is written";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantState {
    /// Local evidence proves at least one Edge webchat_control grant existed
    /// for the connector at some point.
    Present,
    /// The state DB is healthy but carries no connector-attributed grant
    /// evidence. This is *not* an Edge-side absence: grants live on Edge and
    /// this runtime cannot read them back.
    AbsentLocally,
    /// The DB or its tables could not be read; no claim is made.
    Unknown,
}

impl GrantState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::AbsentLocally => "absent_locally",
            Self::Unknown => "unknown",
        }
    }

    /// Where the reported evidence came from. Grants themselves are always
    /// Edge-authoritative; this describes the local projection only.
    fn source(self) -> &'static str {
        match self {
            Self::Present | Self::AbsentLocally => "local_evidence",
            Self::Unknown => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrantScope {
    pub endpoint_ref: String,
    pub provider: String,
}

/// One grant-evidence row: a connector's attributed dispatches observed under
/// a concrete endpoint/provider scope (the tuple `webchat_control_grant_covers`
/// checks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrantPrincipal {
    pub connector_id: String,
    pub grant_generation: Option<i64>,
    pub last_seen: Option<i64>,
    pub bound_caller_sessions: i64,
    pub scopes: Vec<GrantScope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EndpointConsent {
    pub endpoint_ref: String,
    pub browser_family: String,
    /// Retired local WebChat switch (`e44688a0`). Reported only as
    /// `legacy_ignored`; never a gate.
    pub legacy_webchat_control_allowed: bool,
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

    /// Compact lines for the concise doctor / status views. Always starts
    /// with the `state=` summary; when a state DB exists, includes every
    /// principal and per-endpoint line (bounded by the snapshot limits), so
    /// grant evidence and `consent_revision` are visible without
    /// `--verbose`.
    pub(crate) fn concise_lines(&self, language: crate::locale::Locale) -> Vec<String> {
        let mut lines = vec![summary_line(self, language)];
        match self.state {
            GrantState::Unknown => {}
            GrantState::AbsentLocally => {
                for endpoint in &self.endpoints {
                    lines.push(endpoint_line(endpoint));
                }
            }
            GrantState::Present => {
                for principal in &self.principals {
                    lines.push(principal_line(principal));
                    for scope in &principal.scopes {
                        lines.push(format!(
                            "LAYER webchat-grant-scope principal={} endpoint_ref={} provider={}",
                            principal.connector_id, scope.endpoint_ref, scope.provider,
                        ));
                    }
                }
                for endpoint in &self.endpoints {
                    lines.push(endpoint_line(endpoint));
                }
            }
        }
        lines
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
                    "scopes": principal.scopes.iter().map(|scope| json!({
                        "endpoint_ref": scope.endpoint_ref,
                        "provider": scope.provider,
                    })).collect::<Vec<_>>(),
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
                        // Retired local switch: never a gate, reported as such.
                        "webchat_control": "legacy_ignored",
                        "tool_bridge": endpoint.tool_bridge_allowed,
                        "tool_bridge_mutation": endpoint.tool_bridge_mutation_allowed,
                    },
                    "legacy_webchat_control_allowed": endpoint.legacy_webchat_control_allowed,
                    "consent_revision": endpoint.consent_revision,
                    "last_observed_at": endpoint.last_observed_at,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "state": self.state.as_str(),
            // Grants live on Edge; this snapshot is only local evidence and
            // can therefore only ever be non-authoritative.
            "authoritative": false,
            "source": self.state.source(),
            "reason": self.reason,
            "next_step": self.next_step_json(),
            "principals": principals,
            "endpoints": endpoints,
        })
    }

    /// Machine-readable next step. Only emitted when the local DB holds no
    /// grant evidence, where the operator action is an Edge connector grant
    /// from the owner device.
    fn next_step_json(&self) -> Value {
        match self.state {
            GrantState::AbsentLocally => json!({
                "action": "grant_on_owner_device",
                "command": CONNECTOR_GRANT_COMMAND,
                // Secondary: needs an Edge that accepts `scope: "device"` (#713).
                "device_wide_command": DEVICE_WIDE_GRANT_COMMAND,
                "device_wide_requires": DEVICE_WIDE_GRANT_REQUIRES,
            }),
            GrantState::Present | GrantState::Unknown => Value::Null,
        }
    }

    /// The state-dependent next step. A missing Edge grant fails source-turn
    /// binding with `grant_missing`; no local evidence is not an Edge absence,
    /// so it never points at the retired extension WebChat Control switch.
    pub(crate) fn hint(&self, language: crate::locale::Locale) -> String {
        match self.state {
            GrantState::Present => language
                .text(
                    "local dispatch evidence only; Edge grants stay authoritative — if binding still fails with grant_missing, re-check the grant on Edge",
                    "仅为本地派发证据；Edge 授权才是权威 — 若绑定仍报 grant_missing，请在 Edge 上复核授权",
                    "ローカルの dispatch 証拠のみ。権威は Edge の grant — それでもバインディングが grant_missing で失敗する場合は Edge 側で再確認する",
                )
                .to_owned(),
            GrantState::AbsentLocally | GrantState::Unknown => {
                let lead = if self.state == GrantState::AbsentLocally {
                    language.text(
                        "not authoritative: WebChat grants live on Edge and no local evidence exists; grant from the owner device:",
                        "非权威：WebChat 授权在 Edge 上，本地没有证据；请在属主设备上授权：",
                        "非権威：WebChat の grant は Edge 側にあり、ローカルに証拠はない。オーナーデバイスで許可する:",
                    )
                } else {
                    language.text(
                        "grant state not proven here; WebChat grants live on Edge — verify or grant from the owner device:",
                        "本地无法证明授权状态；WebChat 授权在 Edge 上 — 请在属主设备上核对或授权：",
                        "ローカルでは grant 状態を証明できない。WebChat の grant は Edge 側 — オーナーデバイスで確認または許可する:",
                    )
                };
                format!(
                    "{lead} {CONNECTOR_GRANT_COMMAND} ({} {DEVICE_WIDE_GRANT_COMMAND} — {DEVICE_WIDE_GRANT_REQUIRES})",
                    language.text(
                        "device-wide alternative, not the default:",
                        "整设备范围的替代命令（非默认）：",
                        "デバイス全体の代替（既定ではない）:",
                    ),
                )
            }
        }
    }

    pub(crate) fn doctor_line(&self, language: crate::locale::Locale) -> String {
        let hint = self.hint(language);
        let mut line = match self.state {
            GrantState::Unknown => format!(
                "LAYER webchat-grants state=unknown authoritative=false source={} reason={} {hint}",
                self.state.source(),
                self.reason.as_deref().unwrap_or("unavailable")
            ),
            GrantState::AbsentLocally => format!(
                "LAYER webchat-grants state=absent_locally principals=0 authoritative=false source={} {hint}",
                self.state.source(),
            ),
            GrantState::Present => format!(
                "LAYER webchat-grants state=present principals={} authoritative=false source={} {hint}",
                self.principals.len(),
                self.state.source(),
            ),
        };
        for principal in &self.principals {
            line.push('\n');
            line.push_str(&principal_line(principal));
            for scope in &principal.scopes {
                line.push_str(&format!(
                    "\nLAYER webchat-grant-scope principal={} endpoint_ref={} provider={}",
                    principal.connector_id, scope.endpoint_ref, scope.provider,
                ));
            }
        }
        for endpoint in &self.endpoints {
            line.push('\n');
            line.push_str(&endpoint_line(endpoint));
        }
        line
    }
}

fn principal_line(principal: &GrantPrincipal) -> String {
    format!(
        "LAYER webchat-grant principal={} grant_generation={} last_seen={} bound_caller_sessions={} scopes={}",
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
        principal.scopes.len(),
    )
}

/// The per-endpoint row. `webchat_control=legacy_ignored` is fixed text: the
/// stored column is a retired switch, so its raw value is only ever an
/// annotation (`stored_webchat_control=`), never a gate.
fn endpoint_line(endpoint: &EndpointConsent) -> String {
    format!(
        "LAYER webchat-endpoint endpoint_ref={} browser={} tool_bridge={} tool_bridge_mutation={} consent_revision={} webchat_control=legacy_ignored stored_webchat_control={} last_observed_at={}",
        endpoint.endpoint_ref,
        endpoint.browser_family,
        endpoint.tool_bridge_allowed,
        endpoint.tool_bridge_mutation_allowed,
        endpoint.consent_revision,
        endpoint.legacy_webchat_control_allowed,
        endpoint.last_observed_at,
    )
}

/// One-line summary used by `status` / `status --verbose` and the doctor
/// layer rollup. Carries the machine-readable `state=` token.
pub(crate) fn summary_line(
    snapshot: &WebChatGrantSnapshot,
    language: crate::locale::Locale,
) -> String {
    let label = language.text("WebChat grants", "WebChat 授权", "WebChat 認可");
    match snapshot.state {
        GrantState::Present => format!(
            "{label}: state=present principals={} · {}",
            snapshot.principals.len(),
            language.text(
                "local evidence only; Edge grants stay authoritative",
                "仅为本地证据；以 Edge 授权为准",
                "ローカル証拠のみ。権威は Edge の grant",
            ),
        ),
        // Non-prediction: this runtime cannot see the Edge grant, so empty
        // local evidence says nothing about whether binding will fail. Only a
        // real `grant_missing` refusal is the trigger for the grant (#791).
        GrantState::AbsentLocally => format!(
            "{label}: state=absent_locally · {} · {}",
            language.text(
                "no local grant evidence, Edge is authoritative",
                "本地没有授权证据，以 Edge 为准",
                "ローカルに grant 証拠なし。権威は Edge",
            ),
            language.text(
                "if binding is actually refused with grant_missing, grant from the owner device",
                "若绑定确实报 grant_missing，请在属主设备上授权",
                "実際にバインディングが grant_missing で拒否された場合はオーナーデバイスで許可する",
            ),
        ),
        GrantState::Unknown => format!(
            "{label}: state=unknown · {}",
            snapshot.reason.as_deref().unwrap_or("unavailable"),
        ),
    }
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
        // No local evidence is not an Edge absence (#791): the grant lives on
        // Edge and this runtime has no read path for it.
        GrantState::AbsentLocally
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

/// Grant evidence: connector-attributed dispatches. `connector_id`,
/// `principal_ref`, `endpoint_ref` and `provider` are grant principals/scope,
/// not secrets; no token or payload column is selected.
fn read_grant_principals(conn: &rusqlite::Connection) -> Result<Vec<GrantPrincipal>, String> {
    if !table_exists(conn, "browser_dispatches") || !table_exists(conn, "browser_caller_sessions") {
        return Err("grant_tables_missing".to_owned());
    }
    let mut stmt = conn
        .prepare(
            "SELECT authorization_connector_id,
                    MAX(authorization_grant_generation),
                    MAX(updated_at),
                    endpoint_ref,
                    provider
             FROM browser_dispatches
             WHERE authorization_connector_id IS NOT NULL
             GROUP BY authorization_connector_id, endpoint_ref, provider
             ORDER BY MAX(updated_at) DESC
             LIMIT ?1",
        )
        .map_err(|error| format!("grant_query_failed: {error}"))?;
    let rows = stmt
        .query_map([MAX_GRANT_ROWS as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|error| format!("grant_query_failed: {error}"))?;
    let mut principals: Vec<GrantPrincipal> = Vec::new();
    for row in rows {
        let (connector_id, grant_generation, last_seen, endpoint_ref, provider) =
            row.map_err(|error| format!("grant_query_failed: {error}"))?;
        let entry = match principals
            .iter_mut()
            .find(|principal| principal.connector_id == connector_id)
        {
            Some(entry) => entry,
            None => {
                // Strict: a broken caller-session read must surface as
                // unknown, never silently as zero bound sessions.
                let bound = caller_session_count(conn, &connector_id)?;
                principals.push(GrantPrincipal {
                    connector_id: connector_id.clone(),
                    grant_generation,
                    last_seen,
                    bound_caller_sessions: bound,
                    scopes: Vec::new(),
                });
                principals.last_mut().expect("just pushed")
            }
        };
        entry.grant_generation = match (entry.grant_generation, grant_generation) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        entry.last_seen = match (entry.last_seen, last_seen) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        entry.scopes.push(GrantScope {
            endpoint_ref,
            provider,
        });
    }
    Ok(principals)
}

fn caller_session_count(conn: &rusqlite::Connection, connector_id: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COUNT(*) FROM browser_caller_sessions WHERE principal_ref = ?1",
        rusqlite::params![format!("connector:{connector_id}")],
        |row| row.get::<_, i64>(0),
    )
    .map_err(|error| format!("grant_query_failed: {error}"))
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
                legacy_webchat_control_allowed: row.get::<_, i64>(2)? != 0,
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
        assert_eq!(principal.scopes.len(), 1);
        assert_eq!(principal.scopes[0].endpoint_ref, endpoint_ref);
        assert_eq!(principal.scopes[0].provider, "chatgpt");
        assert_eq!(snapshot.endpoints.len(), 1);
        let endpoint = &snapshot.endpoints[0];
        // The per-endpoint WebChat switch was retired in 1.1; consent writes
        // only move Tool Bridge flags. The row still reports its stored flag.
        assert!(!endpoint.legacy_webchat_control_allowed);
        assert!(!endpoint.tool_bridge_allowed);
        assert_eq!(endpoint.consent_revision, 1);

        let json = snapshot.as_json();
        assert_eq!(json["state"], "present");
        assert_eq!(json["authoritative"], false);
        assert_eq!(json["source"], "local_evidence");
        assert_eq!(json["next_step"], Value::Null);
        assert_eq!(json["principals"][0]["connector_id"], "conn_webchatGrant01");
        assert_eq!(
            json["principals"][0]["scopes"][0]["endpoint_ref"],
            json!(endpoint_ref)
        );
        assert_eq!(json["principals"][0]["scopes"][0]["provider"], "chatgpt");
        // The retired switch is reported as a label, never as a gate value.
        assert_eq!(
            json["endpoints"][0]["consent"]["webchat_control"],
            "legacy_ignored"
        );
        assert_eq!(
            json["endpoints"][0]["legacy_webchat_control_allowed"],
            false
        );
        assert_eq!(json["endpoints"][0]["consent_revision"], 1);

        let line = snapshot.doctor_line(crate::locale::Locale::En);
        assert!(line.contains("state=present"));
        assert!(line.contains("authoritative=false"));
        assert!(line.contains("source=local_evidence"));
        assert!(line.contains("conn_webchatGrant01"));
        assert!(line.contains("webchat-grant-scope"));
        assert!(line.contains(&endpoint_ref));
        assert!(line.contains("webchat_control=legacy_ignored"));
        // `present` is local evidence; the retired extension-consent framing
        // is gone even here.
        assert!(!line.contains("extension consent"));
        let _ = std::fs::remove_file(&path);
    }

    /// #791: the stale legacy `webchat_control_allowed=0` row plus no local
    /// grant evidence must send the operator to the Edge connector grant, and
    /// must never present the retired extension consent toggle as the gate.
    #[test]
    fn legacy_endpoint_without_grant_evidence_hints_connector_grant() {
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
        // Force the stale legacy value the MBP state DB carries (#728): the
        // consent write above always stores `webchat_control_allowed = 1`.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE browser_endpoints SET webchat_control_allowed = 0 WHERE endpoint_ref = ?1",
            rusqlite::params![&endpoint_ref],
        )
        .unwrap();
        drop(conn);

        let snapshot = snapshot_at_db_path(&path);
        assert_eq!(snapshot.state, GrantState::AbsentLocally);
        assert!(snapshot.principals.is_empty());
        assert_eq!(snapshot.endpoints.len(), 1);
        assert!(!snapshot.endpoints[0].legacy_webchat_control_allowed);

        let line = snapshot.doctor_line(crate::locale::Locale::En);
        assert!(line.contains("state=absent_locally"), "{line}");
        assert!(line.contains("authoritative=false"), "{line}");
        assert!(line.contains("source=local_evidence"), "{line}");
        // The retired switch is labelled, never rendered as a gate value.
        assert!(line.contains("webchat_control=legacy_ignored"), "{line}");
        assert!(line.contains("stored_webchat_control=false"), "{line}");
        // The real next step is the per-endpoint Edge connector grant, and it
        // leads the device-wide alternative.
        assert!(
            line.contains(CONNECTOR_GRANT_COMMAND),
            "missing connector grant hint:\n{line}"
        );
        assert!(
            line.find(CONNECTOR_GRANT_COMMAND) < line.find(DEVICE_WIDE_GRANT_COMMAND),
            "per-endpoint command must lead the device-wide form:\n{line}"
        );
        // The device-wide form is never offered unconditionally: a pre-#713
        // Edge rejects `scope=device` and writes no grant.
        assert!(
            line.contains("device_grant_requires_edge_support"),
            "device-wide form must name its Edge requirement:\n{line}"
        );
        assert!(
            line.contains("#713"),
            "device-wide form must name the #713 dependency:\n{line}"
        );
        // The status rollup must not predict an Edge-side failure from empty
        // local evidence; `grant_missing` appears only as the real refusal
        // condition, never as "binding would fail" (#791 review).
        for language in [
            crate::locale::Locale::En,
            crate::locale::Locale::ZhCn,
            crate::locale::Locale::Ja,
        ] {
            let summary = summary_line(&snapshot, language);
            assert!(summary.contains("state=absent_locally"), "{summary}");
            assert!(!summary.contains("would fail"), "{summary}");
            assert!(!summary.contains("将报"), "{summary}");
            assert!(!summary.contains("で失敗する"), "{summary}");
        }
        let summary = summary_line(&snapshot, crate::locale::Locale::En);
        assert!(summary.contains("no local grant evidence"), "{summary}");
        assert!(summary.contains("Edge is authoritative"), "{summary}");
        assert!(
            summary.contains("if binding is actually refused with grant_missing"),
            "{summary}"
        );
        // No extension-consent guidance anywhere in the output.
        for rendered in [line.clone(), snapshot.hint(crate::locale::Locale::En)] {
            assert!(!rendered.contains("extension consent"), "{rendered}");
            assert!(!rendered.contains("consent gates"), "{rendered}");
        }

        let json = snapshot.as_json();
        assert_eq!(json["state"], "absent_locally");
        assert_eq!(json["authoritative"], false);
        assert_eq!(json["source"], "local_evidence");
        assert_eq!(json["next_step"]["action"], "grant_on_owner_device");
        assert_eq!(json["next_step"]["command"], CONNECTOR_GRANT_COMMAND);
        assert_eq!(
            json["next_step"]["device_wide_command"],
            DEVICE_WIDE_GRANT_COMMAND
        );
        assert_eq!(
            json["next_step"]["device_wide_requires"],
            DEVICE_WIDE_GRANT_REQUIRES
        );
        assert_eq!(
            json["endpoints"][0]["consent"]["webchat_control"],
            "legacy_ignored"
        );
        assert_eq!(
            json["endpoints"][0]["legacy_webchat_control_allowed"],
            false
        );
        assert_eq!(json["principals"], json!([]));
        assert!(!json.to_string().contains("extension consent"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_when_db_missing_or_table_absent() {
        let missing = temp_db_path();
        let snapshot = snapshot_at_db_path(&missing);
        assert_eq!(snapshot.state, GrantState::Unknown);
        assert_eq!(snapshot.reason.as_deref(), Some("state_db_missing"));
        assert_eq!(snapshot.as_json()["state"], "unknown");
        assert_eq!(snapshot.as_json()["source"], "unavailable");
        assert_eq!(snapshot.as_json()["next_step"], Value::Null);
        let line = snapshot.doctor_line(crate::locale::Locale::En);
        assert!(line.contains("state=unknown"));
        assert!(line.contains("authoritative=false"));
        assert!(!line.contains("extension consent"));

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
        let line = snapshot.doctor_line(crate::locale::Locale::En);
        for rendered in [json_text, line] {
            assert!(!rendered.contains("token"));
            assert!(!rendered.contains("secret"));
            assert!(!rendered.contains("credential"));
            assert!(!rendered.contains("bearer"));
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hint_names_edge_grant_and_never_the_retired_extension_consent() {
        // `present`: local evidence wording, no allow command needed.
        let path = temp_db_path();
        let mut store = crate::state_store::StateStore::open(&path).unwrap();
        let endpoint_ref = register_endpoint(&mut store);
        reserve_authorized_dispatch(&mut store, &endpoint_ref, "conn_hintPresent01");
        drop(store);
        let present = snapshot_at_db_path(&path);
        assert_eq!(present.state, GrantState::Present);
        let hint = present.hint(crate::locale::Locale::En);
        assert!(hint.contains("Edge"), "{hint}");
        assert!(hint.contains("grant_missing"), "{hint}");
        assert!(!hint.contains(CONNECTOR_GRANT_COMMAND), "{hint}");
        let _ = std::fs::remove_file(&path);

        // No local evidence: the connector grant command, in every locale, for
        // both non-evidence states. The device-wide form is secondary and
        // carries its Edge requirement.
        for state in [GrantState::AbsentLocally, GrantState::Unknown] {
            let no_evidence = match state {
                GrantState::AbsentLocally => {
                    let path = temp_db_path();
                    let mut store = crate::state_store::StateStore::open(&path).unwrap();
                    register_endpoint(&mut store);
                    drop(store);
                    let snapshot = snapshot_at_db_path(&path);
                    let _ = std::fs::remove_file(&path);
                    snapshot
                }
                _ => snapshot_at_db_path(&temp_db_path()),
            };
            assert_eq!(no_evidence.state, state);
            for language in [
                crate::locale::Locale::En,
                crate::locale::Locale::ZhCn,
                crate::locale::Locale::Ja,
            ] {
                let hint = no_evidence.hint(language);
                assert!(hint.contains(CONNECTOR_GRANT_COMMAND), "{hint}");
                assert!(
                    hint.find(CONNECTOR_GRANT_COMMAND) < hint.find(DEVICE_WIDE_GRANT_COMMAND),
                    "per-endpoint command must lead:\n{hint}"
                );
                assert!(hint.contains("#713"), "{hint}");
                assert!(!hint.contains("extension consent"), "{hint}");
            }
        }
    }

    #[test]
    fn bound_caller_sessions_are_counted_not_zero_fallback() {
        let path = temp_db_path();
        let mut store = crate::state_store::StateStore::open(&path).unwrap();
        let endpoint_ref = register_endpoint(&mut store);
        reserve_authorized_dispatch(&mut store, &endpoint_ref, "conn_boundCaller01");
        // Bind one caller session for the connector's principal.
        let account = store
            .observe_browser_resource(crate::state_store::BrowserResourceObservationInput {
                endpoint_ref: &endpoint_ref,
                provider: "chatgpt",
                kind: "account",
                parent_ref: None,
                native_identity: "native-identity-bound-caller-account",
                display_label: None,
                observation_generation: 1,
                observed_at: 5,
            })
            .unwrap();
        let session = store
            .observe_browser_resource(crate::state_store::BrowserResourceObservationInput {
                endpoint_ref: &endpoint_ref,
                provider: "chatgpt",
                kind: "session",
                parent_ref: Some(&account.resource_ref),
                native_identity: "native-identity-bound-caller-session",
                display_label: None,
                observation_generation: 1,
                observed_at: 6,
            })
            .unwrap();
        store
            .bind_browser_caller_session(
                "connector:conn_boundCaller01",
                "chatgpt",
                "opaque-caller-session-bound",
                &session.resource_ref,
                7,
            )
            .unwrap();
        drop(store);

        let snapshot = snapshot_at_db_path(&path);
        assert_eq!(snapshot.state, GrantState::Present);
        assert_eq!(snapshot.principals.len(), 1);
        // A successful count must report the real value; the old code
        // swallowed read errors as 0, so this pins the strict path.
        assert_eq!(snapshot.principals[0].bound_caller_sessions, 1);
        let line = snapshot.doctor_line(crate::locale::Locale::En);
        assert!(line.contains("bound_caller_sessions=1"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn concise_lines_carry_state_and_consent_revision() {
        // Present state: principal + scope + consent lines all present.
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
        reserve_authorized_dispatch(&mut store, &endpoint_ref, "conn_conciseView01");
        drop(store);
        let snapshot = snapshot_at_db_path(&path);
        let lines = snapshot.concise_lines(crate::locale::Locale::En);
        assert!(lines.iter().any(|line| line.contains("state=present")));
        assert!(lines.iter().any(|line| line.contains("consent_revision=")));
        assert!(lines.iter().any(|line| line.contains("conn_conciseView01")));
        let _ = std::fs::remove_file(&path);

        // Unknown state: only the state line, still carrying `state=`.
        let missing = temp_db_path();
        let unknown = snapshot_at_db_path(&missing);
        let lines = unknown.concise_lines(crate::locale::Locale::En);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("state=unknown"));
        assert!(lines[0].contains("state_db_missing"));
    }
}
