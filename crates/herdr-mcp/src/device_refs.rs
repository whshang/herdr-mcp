//! #1.2 S1: local resolution of device-bound `herdr_ref_` references.
//!
//! Edge issues opaque `herdr_ref_<base64url>` values that bind a workspace or
//! pane id to an enrolled `dev_*` device. Calls arriving at the local runtime
//! boundary (stdio/HTTP MCP `tools/call`, and `utility_exec` through
//! `herdr_exec`) must resolve the same contract Edge enforces:
//! a ref whose `d` equals the runtime's enrolled `edge.device_id` unwraps to
//! the bare workspace/pane id; anything else fails closed with
//! `device_ref_conflict` — never silently forwarded or coerced.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value};

const REF_PREFIX: &str = "herdr_ref_";
const REF_VERSION: u64 = 1;

/// Workspace fields must carry workspace refs; pane fields must carry pane refs.
const WORKSPACE_REF_FIELDS: [&str; 2] = ["workspace", "workspace_id"];
const PANE_REF_FIELDS: [&str; 5] = ["pane_id", "pane", "target", "split_from", "parent_target"];
/// Scan-only fields contribute device evidence for conflict detection but are
/// never unwrapped (`repair_of` carries a receipt id, not a workspace/pane id).
const SCAN_ONLY_REF_FIELDS: [&str; 1] = ["repair_of"];
/// Nested containers whose own ref fields are resolved one level deep.
const NESTED_REF_CONTAINERS: [&str; 1] = ["spawn"];

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceRefPayload {
    pub device_id: String,
    pub workspace_id: Option<String>,
    pub pane_id: Option<String>,
}

fn is_legacy_workspace_id(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('w') else {
        return false;
    };
    !rest.is_empty()
        && rest
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn is_legacy_pane_id(value: &str) -> bool {
    let Some((workspace, pane)) = value.split_once(':') else {
        return false;
    };
    let Some(rest) = pane.strip_prefix('p') else {
        return false;
    };
    is_legacy_workspace_id(workspace)
        && !rest.is_empty()
        && rest
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// Decode a `herdr_ref_` value to its payload, or `None` when the value is not
/// a well-formed ref. Mirrors the Edge `decodeDeviceRef` strictness: exactly
/// one subject (`w` xor `p`), canonical device id, legacy id grammars.
pub fn decode_device_ref(value: &str) -> Option<DeviceRefPayload> {
    let encoded = value.strip_prefix(REF_PREFIX)?;
    if encoded.len() < 8 {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    let payload: Value = serde_json::from_slice(&bytes).ok()?;
    if payload.get("v").and_then(Value::as_u64) != Some(REF_VERSION) {
        return None;
    }
    let device_id = payload.get("d").and_then(Value::as_str)?;
    let normalized = crate::config::normalize_device_id(device_id).ok()?;
    if normalized != device_id {
        return None;
    }
    let has_w = payload.get("w").is_some();
    let has_p = payload.get("p").is_some();
    if has_w == has_p {
        return None;
    }
    let workspace_id = payload.get("w").and_then(Value::as_str);
    let pane_id = payload.get("p").and_then(Value::as_str);
    if let Some(workspace) = workspace_id
        && !is_legacy_workspace_id(workspace)
    {
        return None;
    }
    if let Some(pane) = pane_id
        && !is_legacy_pane_id(pane)
    {
        return None;
    }
    Some(DeviceRefPayload {
        device_id: normalized,
        workspace_id: workspace_id.map(str::to_owned),
        pane_id: pane_id.map(str::to_owned),
    })
}

#[derive(Debug, Clone, PartialEq)]
enum RefKind {
    Workspace,
    Pane,
    /// Scanned for device evidence only; never unwrapped, never type-checked.
    ScanOnly,
}

fn field_ref_kind(field: &str) -> Option<RefKind> {
    if WORKSPACE_REF_FIELDS.contains(&field) {
        Some(RefKind::Workspace)
    } else if PANE_REF_FIELDS.contains(&field) {
        Some(RefKind::Pane)
    } else if SCAN_ONLY_REF_FIELDS.contains(&field) {
        Some(RefKind::ScanOnly)
    } else {
        None
    }
}

fn payload_kind(payload: &DeviceRefPayload) -> Option<RefKind> {
    match (&payload.workspace_id, &payload.pane_id) {
        (Some(_), None) => Some(RefKind::Workspace),
        (None, Some(_)) => Some(RefKind::Pane),
        _ => None,
    }
}

/// The failure modes Edge maps onto `device_ref_conflict` at the local
/// boundary. `field` names the offending argument (never the ref text itself).
#[derive(Debug, Clone, PartialEq)]
pub enum RefConflict {
    Malformed {
        field: String,
    },
    TypeMismatch {
        field: String,
    },
    ForeignDevice {
        field: String,
    },
    AmbiguousDevices {
        field: String,
    },
    /// A valid local ref exists but the runtime has no enrolled device id to
    /// verify it against — same-device proof is impossible, so fail closed.
    Unverifiable {
        field: String,
    },
}

impl RefConflict {
    pub fn into_result(self) -> Value {
        let (field, reason) = match self {
            RefConflict::Malformed { field } => (field, "malformed_device_ref"),
            RefConflict::TypeMismatch { field } => (field, "ref_kind_mismatch"),
            RefConflict::ForeignDevice { field } => (field, "ref_targets_another_device"),
            RefConflict::AmbiguousDevices { field } => (field, "conflicting_device_refs"),
            RefConflict::Unverifiable { field } => (field, "local_device_unenrolled"),
        };
        serde_json::json!({
            "ok": false,
            "code": "device_ref_conflict",
            "field": field,
            "reason": reason,
            "delivery_state": "not_delivered",
        })
    }
}

fn is_scanned(field: &str) -> bool {
    field_ref_kind(field).is_some()
}

/// Scan one flat object level: first failure wins, otherwise the first decoded
/// hit plus every observed device id for conflict detection.
fn scan_level(
    obj: &Map<String, Value>,
    prefix: &str,
    first: &mut Option<(String, String)>,
    devices: &mut Vec<String>,
) -> Result<(), RefConflict> {
    for (key, value) in obj {
        let Some(text) = value.as_str() else { continue };
        if !is_scanned(key) {
            continue;
        }
        let field = format!("{prefix}{key}");
        let Some(decoded) = decode_device_ref(text) else {
            if text.starts_with(REF_PREFIX) {
                return Err(RefConflict::Malformed { field });
            }
            continue;
        };
        let expected = field_ref_kind(key);
        let actual = payload_kind(&decoded);
        if let (Some(expected), Some(actual)) = (&expected, &actual)
            && expected != actual
            && *expected != RefKind::ScanOnly
        {
            return Err(RefConflict::TypeMismatch { field });
        }
        devices.push(decoded.device_id.clone());
        if first.is_none() {
            first.get_or_insert((field, decoded.device_id.clone()));
        }
    }
    Ok(())
}

/// Every scanned level of a call argument object: top level, `params` (JSON
/// string or object), and one level of `spawn` inside either.
fn scanned_levels(args: &Map<String, Value>) -> Vec<(Map<String, Value>, String)> {
    let mut levels = vec![(args.clone(), String::new())];
    let parsed_params: Option<Map<String, Value>> = match args.get("params") {
        Some(Value::String(text)) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|value| value.as_object().cloned()),
        Some(Value::Object(map)) => Some(map.clone()),
        _ => None,
    };
    if let Some(params) = parsed_params.clone() {
        levels.push((params, "params.".to_owned()));
    }
    for (obj, prefix) in [
        (args, ""),
        (
            parsed_params
                .as_ref()
                .map(|m| m as &Map<String, Value>)
                .unwrap_or(args),
            "params.",
        ),
    ] {
        for container in NESTED_REF_CONTAINERS {
            if let Some(Value::Object(nested)) = obj.get(container) {
                levels.push((nested.clone(), format!("{prefix}{container}.")));
            }
        }
    }
    levels
}

/// Unwrap one flat object level: workspace fields take `w` payloads, pane
/// fields take `p` payloads, scan-only fields are left alone; then `spawn`
/// containers one level deep. Mismatched payloads are never coerced.
fn unwrap_level(obj: &mut Map<String, Value>) {
    for key in WORKSPACE_REF_FIELDS {
        if let Some(Value::String(text)) = obj.get(key)
            && let Some(decoded) = decode_device_ref(text)
            && let Some(workspace) = decoded.workspace_id
        {
            obj.insert(key.to_owned(), Value::String(workspace));
        }
    }
    for key in PANE_REF_FIELDS {
        if let Some(Value::String(text)) = obj.get(key)
            && let Some(decoded) = decode_device_ref(text)
            && let Some(pane) = decoded.pane_id
        {
            obj.insert(key.to_owned(), Value::String(pane));
        }
    }
    for container in NESTED_REF_CONTAINERS {
        if let Some(Value::Object(nested)) = obj.get_mut(container) {
            unwrap_level(nested);
        }
    }
}

/// Resolve every `herdr_ref_` in tool arguments against the runtime's enrolled
/// local device id. Local refs unwrap in place (bare ids reach the existing
/// handlers unchanged); any other outcome is a `device_ref_conflict` the caller
/// returns without dispatching. When `local_device_id` is `None` (unenrolled
/// runtime) a decoded ref is unverifiable and fails closed, while plain ids
/// keep working exactly as before.
pub fn resolve_local_device_refs(
    args: &mut Map<String, Value>,
    local_device_id: Option<&str>,
) -> Result<(), RefConflict> {
    let mut first: Option<(String, String)> = None;
    let mut devices: Vec<String> = Vec::new();
    let levels = scanned_levels(args);
    for (obj, prefix) in &levels {
        scan_level(obj, prefix, &mut first, &mut devices)?;
    }
    let Some((field, _device)) = first else {
        return Ok(());
    };
    if devices
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        > 1
    {
        return Err(RefConflict::AmbiguousDevices { field });
    }
    match local_device_id {
        Some(local) if devices.iter().all(|d| d == local) => {}
        Some(_) => return Err(RefConflict::ForeignDevice { field }),
        None => return Err(RefConflict::Unverifiable { field }),
    }
    // All refs verified local: unwrap so downstream handlers see bare ids.
    unwrap_level(args);
    match args.get_mut("params") {
        Some(Value::String(text)) => {
            if let Ok(Value::Object(mut parsed)) = serde_json::from_str::<Value>(text) {
                unwrap_level(&mut parsed);
                if let Ok(serialized) = serde_json::to_string(&parsed) {
                    *text = serialized;
                }
            }
        }
        Some(Value::Object(parsed)) => unwrap_level(parsed),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV_A: &str = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ";
    const DEV_B: &str = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYY";

    fn encode(device_id: &str, workspace: Option<&str>, pane: Option<&str>) -> String {
        let mut payload = serde_json::json!({"v": 1, "d": device_id});
        if let Some(w) = workspace {
            payload["w"] = Value::String(w.to_owned());
        }
        if let Some(p) = pane {
            payload["p"] = Value::String(p.to_owned());
        }
        format!(
            "{REF_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(payload.to_string())
        )
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn local_workspace_ref_unwraps_to_bare_id() {
        let mut arguments = args(serde_json::json!({"workspace": encode(DEV_A, Some("w1"), None)}));
        resolve_local_device_refs(&mut arguments, Some(DEV_A)).unwrap();
        assert_eq!(arguments["workspace"], "w1");
    }

    #[test]
    fn local_pane_ref_unwraps_in_pane_fields() {
        for field in ["pane_id", "pane", "target", "split_from", "parent_target"] {
            let mut arguments =
                args(serde_json::json!({field: encode(DEV_A, None, Some("w1:p1"))}));
            resolve_local_device_refs(&mut arguments, Some(DEV_A)).unwrap();
            assert_eq!(arguments[field], "w1:p1", "{field}");
        }
    }

    #[test]
    fn foreign_device_ref_fails_closed() {
        let mut arguments = args(serde_json::json!({"workspace": encode(DEV_B, Some("w2"), None)}));
        let conflict = resolve_local_device_refs(&mut arguments, Some(DEV_A)).unwrap_err();
        assert_eq!(
            conflict,
            RefConflict::ForeignDevice {
                field: "workspace".to_owned(),
            }
        );
        let result = conflict.into_result();
        assert_eq!(result["code"], "device_ref_conflict");
        assert_eq!(result["ok"], false);
    }

    #[test]
    fn malformed_ref_fails_closed_instead_of_passing_through() {
        let mut arguments = args(serde_json::json!({"pane_id": "herdr_ref_not-base64!!"}));
        let conflict = resolve_local_device_refs(&mut arguments, Some(DEV_A)).unwrap_err();
        assert_eq!(
            conflict,
            RefConflict::Malformed {
                field: "pane_id".to_owned(),
            }
        );
    }

    #[test]
    fn kind_mismatch_fails_closed_without_coercion() {
        let mut arguments =
            args(serde_json::json!({"workspace": encode(DEV_A, None, Some("w1:p1"))}));
        assert_eq!(
            resolve_local_device_refs(&mut arguments, Some(DEV_A)),
            Err(RefConflict::TypeMismatch {
                field: "workspace".to_owned()
            })
        );
        let mut arguments =
            args(serde_json::json!({"split_from": encode(DEV_A, Some("w1"), None)}));
        assert_eq!(
            resolve_local_device_refs(&mut arguments, Some(DEV_A)),
            Err(RefConflict::TypeMismatch {
                field: "split_from".to_owned()
            })
        );
    }

    #[test]
    fn unenrolled_runtime_fails_closed_on_decoded_ref() {
        let mut arguments = args(serde_json::json!({"workspace": encode(DEV_A, Some("w1"), None)}));
        assert_eq!(
            resolve_local_device_refs(&mut arguments, None),
            Err(RefConflict::Unverifiable {
                field: "workspace".to_owned()
            })
        );
        // Plain ids stay untouched when nothing decodes.
        let mut plain = args(serde_json::json!({"workspace": "w1"}));
        resolve_local_device_refs(&mut plain, None).unwrap();
        assert_eq!(plain["workspace"], "w1");
    }

    #[test]
    fn conflicting_devices_fail_closed() {
        let mut arguments = args(serde_json::json!({
            "workspace": encode(DEV_A, Some("w1"), None),
            "target": encode(DEV_B, None, Some("w2:p1")),
        }));
        assert_eq!(
            resolve_local_device_refs(&mut arguments, Some(DEV_A)),
            Err(RefConflict::AmbiguousDevices {
                field: "target".to_owned()
            })
        );
    }

    #[test]
    fn nested_spawn_fields_resolve_and_unwrap() {
        let mut arguments = args(serde_json::json!({
            "spawn": {
                "kind": "pi",
                "workspace_id": encode(DEV_A, Some("w1"), None),
                "split_from": encode(DEV_A, None, Some("w1:p1")),
            }
        }));
        resolve_local_device_refs(&mut arguments, Some(DEV_A)).unwrap();
        assert_eq!(arguments["spawn"]["workspace_id"], "w1");
        assert_eq!(arguments["spawn"]["split_from"], "w1:p1");
    }

    #[test]
    fn herdr_call_params_resolve_in_object_and_string_forms() {
        let mut object_form = args(serde_json::json!({
            "params": {"workspace_id": encode(DEV_A, Some("w1"), None), "repair_of": encode(DEV_A, None, Some("w1:p1"))}
        }));
        resolve_local_device_refs(&mut object_form, Some(DEV_A)).unwrap();
        assert_eq!(object_form["params"]["workspace_id"], "w1");
        // scan-only repair_of keeps its evidence value untouched
        assert!(
            object_form["params"]["repair_of"]
                .as_str()
                .unwrap()
                .starts_with(REF_PREFIX)
        );

        let string_params = serde_json::json!({
            "spawn": {"workspace_id": encode(DEV_A, Some("w3"), None)}
        })
        .to_string();
        let mut string_form = args(serde_json::json!({"params": string_params}));
        resolve_local_device_refs(&mut string_form, Some(DEV_A)).unwrap();
        let parsed: Value = serde_json::from_str(string_form["params"].as_str().unwrap()).unwrap();
        assert_eq!(parsed["spawn"]["workspace_id"], "w3");
    }

    #[test]
    fn repair_of_foreign_device_surfaces_conflict() {
        let mut arguments = args(serde_json::json!({
            "workspace": encode(DEV_A, Some("w1"), None),
            "repair_of": encode(DEV_B, Some("w2"), None),
        }));
        assert_eq!(
            resolve_local_device_refs(&mut arguments, Some(DEV_A)),
            Err(RefConflict::AmbiguousDevices {
                field: "repair_of".to_owned()
            })
        );
    }
}
