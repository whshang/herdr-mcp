use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;

const MAX_OBSERVATIONS: usize = 16;
const MAX_URL_BYTES: usize = 16 * 1024;
const MAX_CONTEXT_BYTES: usize = 512;

#[derive(Debug, Clone)]
struct Observation {
    url: String,
    source: String,
    conversation_id: Option<String>,
    asset_id: Option<String>,
    observed_at: i64,
    expires_at: Option<i64>,
}

static OBSERVATIONS: OnceLock<Mutex<VecDeque<Observation>>> = OnceLock::new();

fn registry() -> &'static Mutex<VecDeque<Observation>> {
    OBSERVATIONS.get_or_init(|| Mutex::new(VecDeque::new()))
}

pub fn observe(payload: &Value) -> Result<Value, String> {
    let url = required_bounded(payload, "url", MAX_URL_BYTES)?;
    let parsed = validate_chatgpt_signed_url(url)?;
    let source = payload
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("chatgpt_image_gen");
    if source.len() > 64 {
        return Err("artifact_source_too_large".to_owned());
    }
    let conversation_id = optional_bounded(payload, "conversation_id", MAX_CONTEXT_BYTES)?;
    let asset_id = optional_bounded(payload, "asset_id", MAX_CONTEXT_BYTES)?
        .or_else(|| asset_id_from_url(&parsed));
    let observed_at = payload
        .get("observed_at")
        .and_then(Value::as_i64)
        .unwrap_or_else(now_ms);
    let expires_at = signed_url_expiry_ms(&parsed);
    if expires_at.is_some_and(|value| value <= now_ms()) {
        return Err("artifact_url_expired".to_owned());
    }

    let record = Observation {
        url: url.to_owned(),
        source: source.to_owned(),
        conversation_id,
        asset_id: asset_id.clone(),
        observed_at,
        expires_at,
    };
    let mut guard = registry()
        .lock()
        .map_err(|_| "web_artifact_registry_lock_poisoned".to_owned())?;

    if let Some(key) = asset_id.as_deref() {
        guard.retain(|entry| entry.asset_id.as_deref() != Some(key));
    } else {
        guard.retain(|entry| entry.url != record.url);
    }
    guard.push_front(record);
    while guard.len() > MAX_OBSERVATIONS {
        guard.pop_back();
    }

    Ok(json!({
        "ok": true,
        "accepted": true,
        "transport": "thin_web_bridge",
        "expires_at": expires_at,
    }))
}

pub fn inspect_view() -> Value {
    let now = now_ms();
    let Ok(mut guard) = registry().lock() else {
        return json!({
            "available": false,
            "source": "thin-web-bridge",
            "artifacts": [],
            "error": "registry_unavailable",
        });
    };
    guard.retain(|entry| entry.expires_at.is_none_or(|expires| expires > now));
    let artifacts = guard
        .iter()
        .map(|entry| {
            json!({
                "url": entry.url,
                "source": entry.source,
                "conversation_id": entry.conversation_id,
                "asset_id": entry.asset_id,
                "observed_at": entry.observed_at,
                "expires_at": entry.expires_at,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "available": !artifacts.is_empty(),
        "source": "thin-web-bridge",
        "artifacts": artifacts,
        "policy": "optional-enhancement-no-polling",
    })
}

fn validate_chatgpt_signed_url(value: &str) -> Result<Url, String> {
    if value.len() > MAX_URL_BYTES {
        return Err("artifact_url_too_large".to_owned());
    }
    let parsed = Url::parse(value).map_err(|_| "artifact_url_invalid".to_owned())?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("artifact_url_requires_https".to_owned());
    }
    let host = parsed
        .host_str()
        .map(|value| value.to_ascii_lowercase())
        .ok_or_else(|| "artifact_url_host_required".to_owned())?;
    if host != "oaiusercontent.com" && !host.ends_with(".oaiusercontent.com") {
        return Err("artifact_url_host_not_allowed".to_owned());
    }
    if !parsed.path().ends_with("/raw") {
        return Err("artifact_url_path_not_allowed".to_owned());
    }
    let mut has_sig = false;
    let mut has_expiry = false;
    for (name, _) in parsed.query_pairs() {
        match name.as_ref() {
            "sig" => has_sig = true,
            "se" => has_expiry = true,
            _ => {}
        }
    }
    if !has_sig || !has_expiry {
        return Err("artifact_url_signature_required".to_owned());
    }
    Ok(parsed)
}

fn signed_url_expiry_ms(url: &Url) -> Option<i64> {
    let expiry = url
        .query_pairs()
        .find_map(|(name, value)| (name == "se").then(|| value.into_owned()))?;
    let parsed = OffsetDateTime::parse(&expiry, &Rfc3339).ok()?;
    let millis = parsed.unix_timestamp_nanos() / 1_000_000;
    i64::try_from(millis).ok()
}

fn asset_id_from_url(url: &Url) -> Option<String> {
    let segments = url.path_segments()?.collect::<Vec<_>>();
    let index = segments.iter().position(|segment| *segment == "files")?;
    segments.get(index + 1).map(|value| (*value).to_owned())
}

fn required_bounded<'a>(payload: &'a Value, name: &str, max: usize) -> Result<&'a str, String> {
    payload
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{name}_required"))
        .and_then(|value| {
            if value.len() > max {
                Err(format!("{name}_too_large"))
            } else {
                Ok(value)
            }
        })
}

fn optional_bounded(payload: &Value, name: &str, max: usize) -> Result<Option<String>, String> {
    let Some(value) = payload.get(name).and_then(Value::as_str) else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > max {
        return Err(format!("{name}_too_large"));
    }
    Ok(Some(value.to_owned()))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_short_lived_openai_signed_urls_and_extracts_asset_id() {
        let expiry = (OffsetDateTime::now_utc() + time::Duration::hours(1))
            .format(&Rfc3339)
            .unwrap();
        let url = format!(
            "https://example.oaiusercontent.com/files/asset-123/raw?se={}&sig=abc",
            url::form_urlencoded::byte_serialize(expiry.as_bytes()).collect::<String>()
        );
        let parsed = validate_chatgpt_signed_url(&url).unwrap();
        assert_eq!(asset_id_from_url(&parsed).as_deref(), Some("asset-123"));
        assert!(signed_url_expiry_ms(&parsed).unwrap() > now_ms());
    }

    #[test]
    fn rejects_non_openai_or_unsigned_urls() {
        assert!(validate_chatgpt_signed_url("https://example.com/files/a/raw?se=x&sig=y").is_err());
        assert!(validate_chatgpt_signed_url("https://x.oaiusercontent.com/files/a/raw").is_err());
        assert!(
            validate_chatgpt_signed_url("http://x.oaiusercontent.com/files/a/raw?se=x&sig=y")
                .is_err()
        );
    }
}
