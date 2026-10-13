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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRecord {
    pub status: String,
    pub connector_id: String,
    pub grant_generation: u64,
    #[serde(default)]
    pub page_assist: Vec<GrantRow>,
    #[serde(default)]
    pub webchat_control: Vec<GrantRow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
    ConfirmNotImplemented,
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
            Self::ConfirmNotImplemented => "migrate_confirm_not_implemented",
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

/// Read-only orphan listing. A non-active (e.g. revoked) record offers nothing.
pub fn list_orphans(record: &GrantRecord) -> Vec<OrphanRow<'_>> {
    if record.status != "active" {
        return Vec::new();
    }
    all_rows(record)
        .filter(|(_, row)| row.connector_id != record.connector_id)
        .map(|(kind, row)| OrphanRow { kind, row })
        .collect()
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
    if to != record.connector_id {
        return Err(MigrationError::TargetNotCurrent);
    }
    if from == record.connector_id {
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
                grant_generation: record.grant_generation,
                ..o.row.clone()
            },
        })
        .collect();
    if moves.is_empty() {
        return Err(MigrationError::NoMatchingRows);
    }
    Ok(MigrationPlan {
        from_connector_id: from.to_owned(),
        to_connector_id: to.to_owned(),
        grant_generation: record.grant_generation,
        moves,
    })
}

/// `--confirm` write path: not implemented in checkpoint 1; always refuses.
pub fn apply_migration(
    _record: &mut GrantRecord,
    _plan: &MigrationPlan,
) -> Result<(), MigrationError> {
    Err(MigrationError::ConfirmNotImplemented)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrateCli {
    Orphans,
    Migrate {
        from: String,
        to: String,
        confirm: bool,
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
            "--confirm" if !confirm => confirm = true,
            other => {
                return Err(format!(
                    "connector page-assist migrate: unexpected argument '{other}'"
                ))
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
    Ok(MigrateCli::Migrate { from, to, confirm })
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
        }
    }

    fn record() -> GrantRecord {
        GrantRecord {
            status: "active".into(),
            connector_id: NEW.into(),
            grant_generation: 1,
            page_assist: vec![
                row(OLD, "d1", "e1"),
                row(NEW, "d1", "e2"),
                row(OLD2, "d2", "e3"),
            ],
            webchat_control: vec![row(OLD, "d1", "e4")],
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
            assert_eq!(m.to.grant_generation, r.grant_generation);
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
                confirm: false
            }))
        );
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
}
