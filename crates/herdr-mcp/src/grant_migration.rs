//! #626 grant migration, checkpoint 1: pure, read-only logic.
//!
//! - `list_orphans`: rows carried forward by approve whose `connector_id` is not
//!   the record's current connector (design §1 (a)/(b)).
//! - `plan_migration`: dry run only; fails closed on any ambiguity (§1 (a)/(c)).
//! - `apply_migration`: stub that always refuses; the `--confirm` write path is
//!   not implemented in this checkpoint.
//!
//! Nothing here touches or relaxes the caller grant check (§0).
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRow {
    pub connector_id: String,
    pub grant_generation: u64,
    pub device_id: String,
    pub endpoint_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_ref: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRecord {
    pub status: String,
    /// Optional on the Edge (TS) side; absence is reported as a fixed error
    /// code by `plan_migration`, never a deserialization panic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_generation: Option<u64>,
    /// Optional set of still-active sibling connector IDs for the same OAuth
    /// client (#882). When populated, rows belonging to an active sibling
    /// connector whose client-level pointer rotated to a newer approval are
    /// accounted as active rotated-pointer rows rather than stranded orphans.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_connector_ids: Vec<String>,
    #[serde(default)]
    pub page_assist: Vec<GrantRow>,
    #[serde(default)]
    pub webchat_control: Vec<GrantRow>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    PageAssist,
    WebchatControl,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrphanRow<'a> {
    pub kind: GrantKind,
    pub row: &'a GrantRow,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlannedMove {
    pub kind: GrantKind,
    pub from: GrantRow,
    pub to: GrantRow,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MigrationPlan {
    pub from_connector_id: String,
    pub to_connector_id: String,
    pub grant_generation: u64,
    pub moves: Vec<PlannedMove>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrationError {
    MissingFrom,
    MissingTo,
    RecordNotActive,
    TargetNotCurrent,
    SourceIsCurrent,
    NoMatchingRows,
    TargetConflict,
    RecordMissingField,
    RecordInvalid,
    ConfirmNotImplemented,
    PlanStale,
}

impl MigrationError {
    /// Stable error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingFrom => "migrate_from_required",
            Self::MissingTo => "migrate_to_required",
            Self::RecordNotActive => "grant_record_not_active",
            Self::TargetNotCurrent => "migrate_target_not_current",
            Self::SourceIsCurrent => "migrate_source_is_current",
            Self::NoMatchingRows => "migrate_no_matching_rows",
            Self::TargetConflict => "migrate_target_conflict",
            Self::RecordMissingField => "grant_record_missing_field",
            Self::RecordInvalid => "grant_record_invalid",
            Self::ConfirmNotImplemented => "migrate_confirm_not_implemented",
            Self::PlanStale => "migrate_plan_stale",
        }
    }
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

/// Audit entry (design §1 (d)). `from`/`to` are optional because prune,
/// revoke, allow and deny have no target (or no source) connector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub at_ms: u64,
    pub by: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[serde(default)]
    pub from_connector_id: Option<String>,
    #[serde(default)]
    pub to_connector_id: Option<String>,
    #[serde(default)]
    pub rows: Vec<AuditRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRow {
    pub device_id: String,
    pub endpoint_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_ref: Option<String>,
}

fn all_rows(record: &GrantRecord) -> impl Iterator<Item = (GrantKind, &GrantRow)> {
    record
        .page_assist
        .iter()
        .map(|r| (GrantKind::PageAssist, r))
        .chain(
            record
                .webchat_control
                .iter()
                .map(|r| (GrantKind::WebchatControl, r)),
        )
}

/// Parse a stored grant record. Unknown fields are kept (`extra`, flattened) so a
/// later whole-record write-back cannot erase Edge fields; malformed input maps to
/// the fixed code `grant_record_invalid` instead of a raw serde error.
pub fn parse_grant_record(json: &str) -> Result<GrantRecord, MigrationError> {
    serde_json::from_str(json).map_err(|_| MigrationError::RecordInvalid)
}

/// Read-only orphan listing. A non-active (e.g. revoked) record offers nothing.
pub fn list_orphans(record: &GrantRecord) -> Vec<OrphanRow<'_>> {
    if record.status != "active" {
        return Vec::new();
    }
    // An empty current connector_id is treated exactly like a missing one.
    let Some(current) = record.connector_id.as_deref().filter(|c| !c.is_empty()) else {
        return Vec::new();
    };
    all_rows(record)
        .filter(|(_, row)| {
            row.connector_id != current
                && !record
                    .active_connector_ids
                    .iter()
                    .any(|active| active == &row.connector_id)
        })
        .map(|(kind, row)| OrphanRow { kind, row })
        .collect()
}

/// Annotate a grant row JSON value with derived `*` WebChat control metadata
/// when `kind == "page_assist"` (#882).
fn annotate_page_assist_derived_wildcard(row: &mut serde_json::Value) {
    if row.get("kind").and_then(serde_json::Value::as_str) != Some("page_assist") {
        return;
    }
    let endpoint_ref = row
        .get("endpoint_ref")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if endpoint_ref.is_empty() || endpoint_ref == "*" {
        return;
    }
    if let Some(obj) = row.as_object_mut() {
        obj.entry("derived_webchat_control".to_owned())
            .or_insert_with(|| {
                serde_json::json!({
                    "provider": "*",
                    "account_ref": "*",
                    "scope": "endpoint"
                })
            });
    }
}

/// Reconcile `/connectors/page-assist/orphans` list output with the fleet's
/// currently active connector IDs (#882):
/// - Rows whose `connector_id` is still active on `/connectors` (for example a
///   sibling ChatGPT connector whose OAuth client pointer rotated to a newer
///   approval) are moved to `active_connector_rows` with
///   `connector_status: "active_rotated_pointer"` and `effective_at_forward: true`.
/// - `orphans` retains only rows bound to revoked or non-active connectors.
/// - `page_assist` rows disclose `derived_webchat_control` (`provider: "*"`,
///   `account_ref: "*"`, `scope: "endpoint"`).
pub fn reconcile_orphan_list_response(
    mut payload: serde_json::Value,
    active_connector_ids: &std::collections::BTreeSet<String>,
) -> serde_json::Value {
    let Some(raw_orphans) = payload
        .get_mut("orphans")
        .and_then(serde_json::Value::as_array_mut)
        .map(std::mem::take)
    else {
        return payload;
    };

    let mut stranded = Vec::new();
    let mut active_rotated = payload
        .get_mut("active_connector_rows")
        .and_then(serde_json::Value::as_array_mut)
        .map(std::mem::take)
        .unwrap_or_default();

    for row in &mut active_rotated {
        annotate_page_assist_derived_wildcard(row);
        if let Some(obj) = row.as_object_mut() {
            obj.insert(
                "connector_status".to_owned(),
                serde_json::Value::String("active_rotated_pointer".to_owned()),
            );
            obj.insert(
                "effective_at_forward".to_owned(),
                serde_json::Value::Bool(true),
            );
        }
    }

    let mut existing_active_keys = std::collections::BTreeSet::new();
    for row in &active_rotated {
        let key = (
            row.get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
            row.get("connector_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
            row.get("device_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
            row.get("endpoint_ref")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
            row.get("provider")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
            row.get("account_ref")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
        );
        existing_active_keys.insert(key);
    }

    for mut row in raw_orphans {
        annotate_page_assist_derived_wildcard(&mut row);
        let conn_id = row
            .get("connector_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if !conn_id.is_empty() && active_connector_ids.contains(conn_id) {
            if let Some(obj) = row.as_object_mut() {
                obj.insert(
                    "connector_status".to_owned(),
                    serde_json::Value::String("active_rotated_pointer".to_owned()),
                );
                obj.insert(
                    "effective_at_forward".to_owned(),
                    serde_json::Value::Bool(true),
                );
            }
            let key = (
                row.get("kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                row.get("connector_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                row.get("device_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                row.get("endpoint_ref")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                row.get("provider")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                row.get("account_ref")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            );
            if existing_active_keys.insert(key) {
                active_rotated.push(row);
            }
        } else {
            if let Some(obj) = row.as_object_mut() {
                obj.entry("connector_status".to_owned()).or_insert_with(|| {
                    serde_json::Value::String("stranded_inactive_connector".to_owned())
                });
                obj.entry("effective_at_forward".to_owned())
                    .or_insert_with(|| serde_json::Value::Bool(false));
            }
            stranded.push(row);
        }
    }

    let stranded_count = stranded.len();
    let active_rotated_count = active_rotated.len();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("orphans".to_owned(), serde_json::Value::Array(stranded));
        obj.insert(
            "active_connector_rows".to_owned(),
            serde_json::Value::Array(active_rotated),
        );
        obj.insert(
            "orphan_accounting".to_owned(),
            serde_json::json!({
                "stranded_orphan_count": stranded_count,
                "active_rotated_pointer_count": active_rotated_count,
                "total_non_pointer_rows": stranded_count + active_rotated_count,
                "note": "orphans lists only rows bound to revoked or inactive connectors; active_connector_rows lists rows bound to still-active sibling connectors whose client grant pointer rotated to a newer approval"
            }),
        );
    }
    payload
}

/// Dry run. Takes `&GrantRecord`, so it cannot write. Fails closed.
pub fn plan_migration(
    record: &GrantRecord,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<MigrationPlan, MigrationError> {
    let from = from
        .filter(|v| !v.trim().is_empty())
        .ok_or(MigrationError::MissingFrom)?;
    let to = to
        .filter(|v| !v.trim().is_empty())
        .ok_or(MigrationError::MissingTo)?;
    if record.status != "active" {
        return Err(MigrationError::RecordNotActive);
    }
    let (Some(current), Some(generation)) = (
        record.connector_id.as_deref().filter(|c| !c.is_empty()),
        record.grant_generation,
    ) else {
        return Err(MigrationError::RecordMissingField);
    };
    if generation == 0 {
        return Err(MigrationError::RecordMissingField);
    }
    if to != current {
        return Err(MigrationError::TargetNotCurrent);
    }
    if from == current {
        return Err(MigrationError::SourceIsCurrent);
    }
    let moves: Vec<PlannedMove> = list_orphans(record)
        .into_iter()
        .filter(|o| o.row.connector_id == from)
        .map(|o| PlannedMove {
            kind: o.kind,
            from: o.row.clone(),
            to: GrantRow {
                connector_id: to.to_owned(),
                grant_generation: generation,
                ..o.row.clone()
            },
        })
        .collect();
    // Fail closed before returning any plan if a destination already exists
    // under `to` for the same kind (§1 (a)/(c)), or if two moves would land on
    // the same destination key.
    // Review item 3: key is (kind, device_id, endpoint_ref), regardless of provider/account.
    let mut seen: std::collections::HashSet<(GrantKind, String, String)> =
        std::collections::HashSet::new();
    for m in &moves {
        let collides_with_existing = all_rows(record).any(|(kind, row)| {
            kind == m.kind
                && row.connector_id == to
                && row.device_id == m.to.device_id
                && row.endpoint_ref == m.to.endpoint_ref
        });
        let duplicate = !seen.insert((m.kind, m.to.device_id.clone(), m.to.endpoint_ref.clone()));
        if collides_with_existing || duplicate {
            return Err(MigrationError::TargetConflict);
        }
    }
    if moves.is_empty() {
        return Err(MigrationError::NoMatchingRows);
    }
    Ok(MigrationPlan {
        from_connector_id: from.to_owned(),
        to_connector_id: to.to_owned(),
        grant_generation: generation,
        moves,
    })
}

/// `--confirm` write path: not implemented in checkpoint 1; always refuses.
/// Fail closed first: the plan must equal a fresh plan recomputed against the current
/// record (conflict/target detection re-run), else `migrate_target_conflict` /
/// `migrate_plan_stale` (or the fresh plan's own error). Never writes.
pub fn apply_migration(
    record: &mut GrantRecord,
    plan: &MigrationPlan,
) -> Result<(), MigrationError> {
    revalidate_plan(record, plan)?;
    Err(MigrationError::ConfirmNotImplemented)
}

/// Recompute the plan against `record` and require it to be identical to `plan`.
pub fn revalidate_plan(record: &GrantRecord, plan: &MigrationPlan) -> Result<(), MigrationError> {
    let fresh = plan_migration(
        record,
        Some(&plan.from_connector_id),
        Some(&plan.to_connector_id),
    )?;
    if &fresh != plan {
        return Err(MigrationError::PlanStale);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrateCli {
    Orphans,
    /// `orphans [list] --device-id <dev_…>`: assert the authenticated device. Edge scopes
    /// results to the authenticated owner device server-side regardless.
    OrphansForDevice {
        device_id: String,
    },
    Migrate {
        from: String,
        to: String,
        confirm: bool,
        idempotency_key: Option<String>,
        /// A previously printed plan digest. When present, `--confirm` applies
        /// that digest instead of re-planning, which is what makes an
        /// interrupted run replayable: the same key plus the same digest hits
        /// the Edge idempotency branch and returns the stored result.
        plan_digest: Option<String>,
    },
}

fn valid_connector_id(id: &str) -> bool {
    id.starts_with("conn_")
        && id.len() >= 13
        && id.len() <= 133
        && id
            .chars()
            .skip(5)
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

/// Parse args after `connector page-assist` for `orphans` / `migrate`.
/// Returns None if the action is neither.
pub fn parse_cli(args: &[String]) -> Option<Result<MigrateCli, String>> {
    match args.first().map(String::as_str) {
        Some("orphans") => Some(match &args[1..] {
            [] => Ok(MigrateCli::Orphans),
            [list] if list == "list" => Ok(MigrateCli::Orphans),
            [flag, id] | [_, flag, id]
                if flag == "--device-id" && (args.len() == 3 || args[1] == "list") =>
            {
                if valid_device_id(id) {
                    Ok(MigrateCli::OrphansForDevice {
                        device_id: id.clone(),
                    })
                } else {
                    Err("connector page-assist orphans: --device-id requires a valid device id (orphans_invalid_device_id)".to_owned())
                }
            }
            _ => Err("connector page-assist orphans takes no arguments (only `list`)".to_owned()),
        }),
        Some("migrate") => Some(parse_migrate(&args[1..])),
        _ => None,
    }
}

fn parse_migrate(args: &[String]) -> Result<MigrateCli, String> {
    let mut from = None;
    let mut to = None;
    let mut confirm = false;
    let mut idempotency_key: Option<String> = None;
    let mut plan_digest: Option<String> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--from" | "--to" => {
                let slot = if arg == "--from" { &mut from } else { &mut to };
                if slot.is_some() {
                    return Err(format!(
                        "connector page-assist migrate: {arg} given more than once"
                    ));
                }
                let value = iter.next().filter(|v| !v.starts_with('-')).ok_or_else(|| {
                    format!("connector page-assist migrate: {arg} requires a connector id")
                })?;
                if !valid_connector_id(value) {
                    return Err(format!(
                        "connector page-assist migrate: {arg} requires a valid connector id"
                    ));
                }
                *slot = Some(value.clone());
            }
            "--idempotency-key" if idempotency_key.is_none() => {
                let value = iter.next().filter(|v| !v.starts_with('-')).ok_or_else(|| {
                    "connector page-assist migrate: --idempotency-key requires a value".to_owned()
                })?;
                if !valid_idempotency_key(value) {
                    return Err(
                        "connector page-assist migrate: --idempotency-key requires 16-64 chars of A-Za-z0-9_-"
                            .to_owned(),
                    );
                }
                idempotency_key = Some(value.clone());
            }
            "--plan-digest" if plan_digest.is_none() => {
                let value = iter.next().filter(|v| !v.starts_with('-')).ok_or_else(|| {
                    "connector page-assist migrate: --plan-digest requires a value".to_owned()
                })?;
                if !valid_plan_digest(value) {
                    return Err(
                        "connector page-assist migrate: --plan-digest requires 64 lowercase hex chars"
                            .to_owned(),
                    );
                }
                plan_digest = Some(value.clone());
            }
            "--confirm" if !confirm => confirm = true,
            other => {
                return Err(format!(
                    "connector page-assist migrate: unexpected argument '{other}'"
                ));
            }
        }
    }
    let from = from
        .ok_or_else(|| "connector page-assist migrate requires --from <connector-id>".to_owned())?;
    let to =
        to.ok_or_else(|| "connector page-assist migrate requires --to <connector-id>".to_owned())?;
    if from == to {
        return Err("connector page-assist migrate: --from and --to must differ".to_owned());
    }
    if plan_digest.is_some() && !confirm {
        // A digest only matters for the write path; accepting it in a dry run
        // would silently ignore it.
        return Err("connector page-assist migrate: --plan-digest requires --confirm".to_owned());
    }
    Ok(MigrateCli::Migrate {
        from,
        to,
        confirm,
        idempotency_key,
        plan_digest,
    })
}

/// #630 §7.3: client idempotency key charset/length, same rule as the Edge DO.
pub fn valid_idempotency_key(key: &str) -> bool {
    (16..=64).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// Plan digest charset: 64 lowercase hex chars (same rule as the Edge DO).
pub fn valid_plan_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_lowercase() && b.is_ascii_hexdigit()))
}

/// Canonical device id: `dev_` + 26-char Crockford ULID (same rule as the Edge DO).
pub fn valid_device_id(id: &str) -> bool {
    id.strip_prefix("dev_").is_some_and(|ulid| {
        ulid.len() == 26
            && ulid.bytes().all(|b| {
                b.is_ascii_digit()
                    || (b.is_ascii_alphabetic()
                        && !matches!(b.to_ascii_uppercase(), b'I' | b'L' | b'O' | b'U'))
            })
    })
}

/// Orphans list body, optionally filtered by device id. Fails closed on an invalid id.
pub fn orphans_list_body(device_id: Option<&str>) -> Result<serde_json::Value, String> {
    match device_id {
        None => Ok(serde_json::json!({ "action": "list" })),
        Some(id) if valid_device_id(id) => {
            Ok(serde_json::json!({ "action": "list", "device_id": id }))
        }
        Some(_) => Err(
            "connector page-assist orphans: invalid device id (orphans_invalid_device_id)"
                .to_owned(),
        ),
    }
}

/// Request body for the read-only Edge route `/connectors/page-assist/orphans`.
/// Fails closed unless both selectors are absent (list) or both present (plan).
pub fn orphans_request_body(
    from: Option<&str>,
    to: Option<&str>,
) -> Result<serde_json::Value, String> {
    match (from, to) {
        (None, None) => Ok(serde_json::json!({ "action": "list" })),
        (Some(from), Some(to))
            if valid_connector_id(from) && valid_connector_id(to) && from != to =>
        {
            Ok(serde_json::json!({ "action": "plan", "from": from, "to": to }))
        }
        (None, Some(_)) => {
            Err("connector page-assist migrate requires --from <connector-id>".to_owned())
        }
        (Some(_), None) => {
            Err("connector page-assist migrate requires --to <connector-id>".to_owned())
        }
        _ => {
            Err("connector page-assist migrate requires distinct valid --from and --to".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "conn_old000000001";
    const OLD2: &str = "conn_old000000002";
    const NEW: &str = "conn_new000000001";

    fn row(conn: &str, dev: &str, ep: &str) -> GrantRow {
        GrantRow {
            connector_id: conn.into(),
            grant_generation: 1,
            device_id: dev.into(),
            endpoint_ref: ep.into(),
            provider: None,
            account_ref: None,
            extra: Default::default(),
        }
    }

    fn record() -> GrantRecord {
        GrantRecord {
            status: "active".into(),
            connector_id: Some(NEW.into()),
            grant_generation: Some(1),
            active_connector_ids: Vec::new(),
            page_assist: vec![
                row(OLD, "d1", "e1"),
                row(NEW, "d1", "e2"),
                row(OLD2, "d2", "e3"),
            ],
            webchat_control: vec![row(OLD, "d1", "e4")],
            extra: Default::default(),
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn orphans_excludes_current_connector_rows() {
        let r = record();
        let o = list_orphans(&r);
        assert_eq!(o.len(), 3);
        assert!(o.iter().all(|o| o.row.connector_id != NEW));
    }

    #[test]
    fn revoked_record_offers_no_orphans_and_no_plan() {
        let mut r = record();
        r.status = "revoked".into();
        assert!(list_orphans(&r).is_empty());
        assert_eq!(
            plan_migration(&r, Some(OLD), Some(NEW)),
            Err(MigrationError::RecordNotActive)
        );
        r.status = "inactive".into();
        assert!(list_orphans(&r).is_empty());
    }

    #[test]
    fn plan_only_moves_rows_of_from() {
        let r = record();
        let p = plan_migration(&r, Some(OLD), Some(NEW)).unwrap();
        assert_eq!(p.moves.len(), 2);
        for m in &p.moves {
            assert_eq!(m.from.connector_id, OLD);
            assert_eq!(m.to.connector_id, NEW);
            assert_eq!(Some(m.to.grant_generation), r.grant_generation);
            assert_eq!(
                m.to.endpoint_ref, m.from.endpoint_ref,
                "no endpoint remapping"
            );
            assert_eq!(m.to.device_id, m.from.device_id);
        }
    }

    #[test]
    fn plan_fails_closed_on_ambiguity() {
        let r = record();
        assert_eq!(
            plan_migration(&r, None, Some(NEW)),
            Err(MigrationError::MissingFrom)
        );
        assert_eq!(
            plan_migration(&r, Some(OLD), None),
            Err(MigrationError::MissingTo)
        );
        assert_eq!(
            plan_migration(&r, Some(""), Some(NEW)),
            Err(MigrationError::MissingFrom)
        );
        assert_eq!(
            plan_migration(&r, Some(OLD), Some(OLD2)),
            Err(MigrationError::TargetNotCurrent)
        );
        assert_eq!(
            plan_migration(&r, Some(NEW), Some(NEW)),
            Err(MigrationError::SourceIsCurrent)
        );
        assert_eq!(
            plan_migration(&r, Some("conn_unknown00001"), Some(NEW)),
            Err(MigrationError::NoMatchingRows)
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let r = record();
        let before = serde_json::to_vec(&r).unwrap();
        let _ = plan_migration(&r, Some(OLD), Some(NEW)).unwrap();
        let _ = plan_migration(&r, Some("conn_unknown00001"), Some(NEW));
        let _ = list_orphans(&r);
        assert_eq!(before, serde_json::to_vec(&r).unwrap());
    }

    #[test]
    fn target_conflict_fails_closed() {
        // A row already carried under NEW with the same destination key as an
        // OLD row means migrating would overwrite existing authority.
        let mut r = record();
        r.page_assist.push(row(NEW, "d1", "e1"));
        let before = serde_json::to_vec(&r).unwrap();
        assert_eq!(
            plan_migration(&r, Some(OLD), Some(NEW)),
            Err(MigrationError::TargetConflict)
        );
        assert_eq!(before, serde_json::to_vec(&r).unwrap());
    }

    #[test]
    fn unknown_fields_round_trip() {
        let raw = r#"{
            "status": "active",
            "connector_id": "conn_new000000001",
            "grant_generation": 1,
            "page_assist": [
                {
                    "connector_id": "conn_old000000001",
                    "grant_generation": 1,
                    "device_id": "d1",
                    "endpoint_ref": "e1",
                    "note": "n"
                }
            ],
            "webchat_control": [],
            "audit": [{"by": "x"}],
            "future_field": 7
        }"#;
        let rec: GrantRecord = serde_json::from_str(raw).unwrap();
        let round = serde_json::to_string(&rec).unwrap();
        let expected: serde_json::Value = serde_json::from_str(raw).unwrap();
        let actual: serde_json::Value = serde_json::from_str(&round).unwrap();
        assert_eq!(expected, actual);

        let plan = plan_migration(&rec, Some(OLD), Some(NEW)).unwrap();
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(
            plan.moves[0].to.extra.get("note"),
            Some(&serde_json::Value::String("n".into()))
        );
    }

    #[test]
    fn target_conflict_ignores_provider_and_account() {
        // Review item 3 key is (device_id, endpoint_ref): a webchat row under NEW
        // for the same device/endpoint conflicts even with another provider.
        let mut r = record();
        let mut existing = row(NEW, "d1", "e4");
        existing.provider = Some("other".into());
        r.webchat_control.push(existing);
        assert_eq!(
            plan_migration(&r, Some(OLD), Some(NEW)),
            Err(MigrationError::TargetConflict)
        );
        // A different endpoint on the target is not a conflict.
        let mut ok = record();
        ok.page_assist.push(row(NEW, "d1", "e9"));
        assert!(plan_migration(&ok, Some(OLD), Some(NEW)).is_ok());
    }

    #[test]
    fn edge_shaped_record_round_trips_and_missing_fields_get_fixed_code() {
        // Real Edge record: client_id, approved_at_ms, revocation_scope etc. are
        // not modelled and must survive; connector_id/grant_generation absent.
        let raw = r#"{"status":"active","client_id":"https://chatgpt.example/client","approved_at_ms":1000,"approved_by":"device:owner","revocation_scope":"client","resource":"https://issuer/mcp","scope":"mcp","page_assist":[{"connector_id":"conn_old000000001","grant_generation":1,"device_id":"d1","endpoint_ref":"e1"}],"webchat_control":[]}"#;
        let rec = parse_grant_record(raw).unwrap();
        assert_eq!(rec.connector_id, None);
        let once = serde_json::to_string(&rec).unwrap();
        let expected: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&once).unwrap(),
            expected
        );
        // Re-serialisation is byte-stable (idempotent).
        let twice = serde_json::to_string(&parse_grant_record(&once).unwrap()).unwrap();
        assert_eq!(once.as_bytes(), twice.as_bytes());
        assert!(list_orphans(&rec).is_empty());
        assert_eq!(
            plan_migration(&rec, Some(OLD), Some(NEW)),
            Err(MigrationError::RecordMissingField)
        );
        assert_eq!(
            MigrationError::RecordMissingField.code(),
            "grant_record_missing_field"
        );
        let mut zero = record();
        zero.grant_generation = Some(0);
        assert_eq!(
            plan_migration(&zero, Some(OLD), Some(NEW)),
            Err(MigrationError::RecordMissingField)
        );
        // Malformed input -> fixed code, not a raw serde error.
        for bad in [
            r#"{"connector_id":"conn_new000000001"}"#,
            "not json",
            r#"{"status":"active","grant_generation":"x"}"#,
        ] {
            assert_eq!(
                parse_grant_record(bad),
                Err(MigrationError::RecordInvalid),
                "{bad}"
            );
        }
    }

    #[test]
    fn confirm_stub_refuses_and_leaves_record_unchanged() {
        let mut r = record();
        let before = serde_json::to_vec(&r).unwrap();
        let p = plan_migration(&r, Some(OLD), Some(NEW)).unwrap();
        assert_eq!(
            apply_migration(&mut r, &p),
            Err(MigrationError::ConfirmNotImplemented)
        );
        assert_eq!(before, serde_json::to_vec(&r).unwrap());
    }

    #[test]
    fn stale_plan_is_rejected_after_a_conflicting_row_appears() {
        let mut r = record();
        let p = plan_migration(&r, Some(OLD), Some(NEW)).unwrap();
        let dest = p.moves[0].to.clone();
        match p.moves[0].kind {
            GrantKind::PageAssist => r.page_assist.push(dest),
            GrantKind::WebchatControl => r.webchat_control.push(dest),
        }
        let before = serde_json::to_vec(&r).unwrap();
        assert_eq!(
            apply_migration(&mut r, &p),
            Err(MigrationError::TargetConflict)
        );
        assert_eq!(before, serde_json::to_vec(&r).unwrap());
        // A plan that no longer matches the record (e.g. rows changed) is stale.
        let mut r2 = record();
        let mut p2 = plan_migration(&r2, Some(OLD), Some(NEW)).unwrap();
        p2.moves.pop();
        assert_eq!(
            apply_migration(&mut r2, &p2),
            Err(MigrationError::PlanStale)
        );
        assert_eq!(MigrationError::PlanStale.code(), "migrate_plan_stale");
    }

    #[test]
    fn empty_current_connector_id_is_treated_as_missing() {
        let mut r = record();
        r.connector_id = Some(String::new());
        assert!(list_orphans(&r).is_empty());
        assert_eq!(
            plan_migration(&r, Some(OLD), Some(NEW)),
            Err(MigrationError::RecordMissingField)
        );
    }

    #[test]
    fn audit_record_from_to_are_optional() {
        let a = AuditRecord {
            at_ms: 1,
            by: "dev".into(),
            action: "revoke".into(),
            proposal_id: None,
            from_connector_id: None,
            to_connector_id: None,
            rows: vec![],
        };
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(
            json,
            r#"{"at_ms":1,"by":"dev","action":"revoke","from_connector_id":null,"to_connector_id":null,"rows":[]}"#
        );
        let back: AuditRecord = serde_json::from_str(
            r#"{"at_ms":1,"by":"dev","action":"prune","from_connector_id":"conn_old000000001"}"#,
        )
        .unwrap();
        assert_eq!(back.to_connector_id, None);
    }

    #[test]
    fn orphans_request_body_is_read_only_and_fails_closed() {
        assert_eq!(
            orphans_request_body(None, None).unwrap(),
            serde_json::json!({"action": "list"})
        );
        // #630 review: the dry-run body never carries a client device_id — the
        // Edge injects the authenticated registry device server-side.
        let plan_body = orphans_request_body(Some(OLD), Some(NEW)).unwrap();
        assert_eq!(
            plan_body,
            serde_json::json!({"action": "plan", "from": OLD, "to": NEW})
        );
        assert!(plan_body.get("device_id").is_none());
        assert!(orphans_request_body(Some(OLD), None).is_err());
        assert!(orphans_request_body(None, Some(NEW)).is_err());
        assert!(orphans_request_body(Some(NEW), Some(NEW)).is_err());
        assert!(orphans_request_body(Some("bogus"), Some(NEW)).is_err());
    }

    #[test]
    fn orphans_device_filter_parses_and_fails_closed() {
        const DEV: &str = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        assert_eq!(
            parse_cli(&s(&["orphans", "--device-id", DEV])),
            Some(Ok(MigrateCli::OrphansForDevice {
                device_id: DEV.into()
            }))
        );
        assert_eq!(
            parse_cli(&s(&["orphans", "list", "--device-id", DEV])),
            Some(Ok(MigrateCli::OrphansForDevice {
                device_id: DEV.into()
            }))
        );
        for bad in [
            vec!["orphans", "--device-id"],
            vec!["orphans", "--device-id", "dev_short"],
            vec!["orphans", "--device-id", "dev_01ARZ3NDEKTSV4RRFFQ69G5FAI"],
            vec!["orphans", "--device-id", "-x"],
            vec!["orphans", "x", "--device-id", DEV],
            vec!["orphans", "--device-id", DEV, "extra"],
        ] {
            assert!(matches!(parse_cli(&s(&bad)), Some(Err(_))), "{bad:?}");
        }
        assert_eq!(
            orphans_list_body(Some(DEV)).unwrap(),
            serde_json::json!({"action": "list", "device_id": DEV})
        );
        assert_eq!(
            orphans_list_body(None).unwrap(),
            serde_json::json!({"action": "list"})
        );
        assert!(orphans_list_body(Some("bogus")).is_err());
    }

    #[test]
    fn cli_parse_orphans_and_migrate() {
        assert_eq!(parse_cli(&s(&["orphans"])), Some(Ok(MigrateCli::Orphans)));
        assert_eq!(
            parse_cli(&s(&["orphans", "list"])),
            Some(Ok(MigrateCli::Orphans))
        );
        assert!(matches!(parse_cli(&s(&["orphans", "x"])), Some(Err(_))));
        assert_eq!(parse_cli(&s(&["allow"])), None);
        assert_eq!(
            parse_cli(&s(&["migrate", "--from", OLD, "--to", NEW])),
            Some(Ok(MigrateCli::Migrate {
                from: OLD.into(),
                to: NEW.into(),
                confirm: false,
                idempotency_key: None,
                plan_digest: None,
            }))
        );
        assert_eq!(
            parse_cli(&s(&[
                "migrate",
                "--from",
                OLD,
                "--to",
                NEW,
                "--idempotency-key",
                "rust-key-00000001",
                "--confirm"
            ])),
            Some(Ok(MigrateCli::Migrate {
                from: OLD.into(),
                to: NEW.into(),
                confirm: true,
                idempotency_key: Some("rust-key-00000001".into()),
                plan_digest: None,
            }))
        );
    }

    #[test]
    fn cli_migrate_accepts_a_plan_digest_for_replay() {
        let digest = "a".repeat(64);
        assert_eq!(
            parse_cli(&s(&[
                "migrate",
                "--from",
                OLD,
                "--to",
                NEW,
                "--idempotency-key",
                "rust-key-00000001",
                "--plan-digest",
                &digest,
                "--confirm"
            ])),
            Some(Ok(MigrateCli::Migrate {
                from: OLD.into(),
                to: NEW.into(),
                confirm: true,
                idempotency_key: Some("rust-key-00000001".into()),
                plan_digest: Some(digest.clone()),
            }))
        );
        // A digest without --confirm, a malformed digest, and a repeated flag
        // all fail closed.
        for bad in [
            vec![
                "migrate",
                "--from",
                OLD,
                "--to",
                NEW,
                "--plan-digest",
                &digest,
            ],
            vec![
                "migrate",
                "--from",
                OLD,
                "--to",
                NEW,
                "--plan-digest",
                "abc",
                "--confirm",
            ],
            vec![
                "migrate",
                "--from",
                OLD,
                "--to",
                NEW,
                "--plan-digest",
                &digest,
                "--plan-digest",
                &digest,
                "--confirm",
            ],
        ] {
            assert!(matches!(parse_cli(&s(&bad)), Some(Err(_))), "{bad:?}");
        }
    }

    #[test]
    fn cli_migrate_missing_or_ambiguous_selectors_fail() {
        for args in [
            vec!["migrate"],
            vec!["migrate", "--from", OLD],
            vec!["migrate", "--to", NEW],
            vec!["migrate", "--from", "--to", NEW],
            vec!["migrate", "--from", OLD, "--from", OLD2, "--to", NEW],
            vec!["migrate", "--from", OLD, "--to", OLD],
            vec!["migrate", "--from", "bogus", "--to", NEW],
            vec![
                "migrate",
                "--from",
                OLD,
                "--to",
                NEW,
                "--confirm",
                "--confirm",
            ],
            vec!["migrate", "--from", OLD, "--to", NEW, "extra"],
        ] {
            assert!(matches!(parse_cli(&s(&args)), Some(Err(_))), "{args:?}");
        }
    }

    /// Snapshot (design §1 (g)): provider-facing `caller_grant_missing` body
    /// in mcp.rs must stay byte-identical.
    #[test]
    fn caller_grant_missing_body_snapshot() {
        let src = include_str!("mcp.rs");
        assert!(src.contains(
            r#"return json!({"ok": false, "code": "caller_grant_missing", "retryable": false});"#
        ));
        let body =
            serde_json::json!({"ok": false, "code": "caller_grant_missing", "retryable": false});
        assert_eq!(
            serde_json::to_string(&body).unwrap(),
            r#"{"code":"caller_grant_missing","ok":false,"retryable":false}"#
        );
    }

    #[test]
    fn list_orphans_excludes_active_sibling_connectors_when_present() {
        let mut rec = record();
        rec.active_connector_ids = vec![OLD2.to_owned()];
        let orphans = list_orphans(&rec);
        assert!(orphans.iter().all(|o| o.row.connector_id == OLD));
        assert_eq!(orphans.len(), 2);
    }

    #[test]
    fn reconcile_orphan_list_response_separates_active_sibling_rows_and_discloses_derived_wildcard()
    {
        let raw = serde_json::json!({
            "ok": true,
            "orphans": [
                {
                    "kind": "page_assist",
                    "client_id": "https://chatgpt.com/oauth/client.json",
                    "current_connector_id": NEW,
                    "connector_id": OLD,
                    "grant_generation": 1,
                    "device_id": "dev_01M1BDHFE8TJ2PZGQAAG4T1A3V",
                    "endpoint_ref": "ep_1"
                },
                {
                    "kind": "page_assist",
                    "client_id": "https://chatgpt.com/oauth/client.json",
                    "current_connector_id": NEW,
                    "connector_id": OLD2,
                    "grant_generation": 2,
                    "device_id": "dev_01M1BDHFE8TJ2PZGQAAG4T1A3V",
                    "endpoint_ref": "ep_2"
                }
            ],
            "invalid_records": 0
        });
        let active = std::collections::BTreeSet::from([NEW.to_owned(), OLD2.to_owned()]);
        let reconciled = reconcile_orphan_list_response(raw, &active);
        let orphans = reconciled["orphans"].as_array().unwrap();
        let active_rows = reconciled["active_connector_rows"].as_array().unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0]["connector_id"], OLD);
        assert_eq!(
            orphans[0]["connector_status"],
            "stranded_inactive_connector"
        );
        assert_eq!(orphans[0]["derived_webchat_control"]["provider"], "*");
        assert_eq!(active_rows.len(), 1);
        assert_eq!(active_rows[0]["connector_id"], OLD2);
        assert_eq!(active_rows[0]["connector_status"], "active_rotated_pointer");
        assert_eq!(active_rows[0]["effective_at_forward"], true);
        assert_eq!(active_rows[0]["derived_webchat_control"]["provider"], "*");
        assert_eq!(reconciled["orphan_accounting"]["stranded_orphan_count"], 1);
        assert_eq!(
            reconciled["orphan_accounting"]["active_rotated_pointer_count"],
            1
        );
        assert_eq!(reconciled["orphan_accounting"]["total_non_pointer_rows"], 2);
    }
}
