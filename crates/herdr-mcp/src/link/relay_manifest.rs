//! Signed, provider-neutral Relay Pool manifests.
//!
//! The envelope carries `payload` as base64 of the exact UTF-8 JSON bytes
//! signed by the publisher. The Ed25519 message is stable and reproducible by
//! an external signer:
//!
//! `HERDR-RELAY-POOL-V1\0 || u32_be(payload_len) || payload_bytes`
//!
//! The client never canonicalizes or reserializes JSON before signature
//! verification. Production builds carry public verification keys only; the
//! matching signing private keys are publisher-side material and must never be
//! stored in the repository or runtime config. Test keys exist only under
//! `cfg(test)`.
//!
//! Link startup only reads the last-known-good cache. Network fetching is a
//! separate bounded primitive and is not wired into the healthy Link loop: a
//! newly accepted manifest therefore applies on the next Link daemon
//! construction/process recycle, not on an ordinary reconnect.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

use super::ladder::{RelayEndpoint, default_embedded_relays};
use crate::paths::RuntimePaths;

const SIGNING_DOMAIN: &[u8] = b"HERDR-RELAY-POOL-V1\0";
pub const SCHEMA_VERSION: u64 = 1;
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
pub const MAX_PAYLOAD_BYTES: usize = 128 * 1024;
pub const MAX_RELAYS: usize = 32;
pub const CACHE_FILE_NAME: &str = "last-known-good.json";
const MAX_KEY_ID_LEN: usize = 64;
const MAX_SIGNATURE_B64_LEN: usize = 128;
const MAX_TIMESTAMP_LEN: usize = 64;
const MAX_RELAY_ID_LEN: usize = 64;
const MAX_FAILURE_DOMAIN_LEN: usize = 128;
const MAX_RELAY_URL_LEN: usize = 2048;
const MAX_PRIORITY: u32 = 1_000_000;
const GENERATED_FUTURE_SKEW_SECONDS: i64 = 300;
const RELAY_PROD_2026_09_KEY_ID: &str = "relay-prod-2026-09";
const RELAY_PROD_2026_09_PUBLIC_KEY: [u8; 32] = [
    0x22, 0xe4, 0xea, 0xeb, 0xbd, 0xfa, 0x2a, 0xfa, 0x9b, 0xdd, 0x85, 0x8a, 0x2d, 0x52, 0x1e, 0x2b,
    0xf9, 0xa9, 0xb8, 0x0d, 0xba, 0xa7, 0xb1, 0x33, 0xad, 0xcd, 0xca, 0x14, 0x3a, 0x60, 0x17, 0xee,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Malformed(&'static str),
    UnknownKey,
    InvalidSignature,
    InvalidPayload(&'static str),
    Expired,
    NotYetValid,
    Rollback { current: u64, received: u64 },
    UntrustedCache(&'static str),
    Io(String),
    Fetch(&'static str),
}

impl ManifestError {
    pub const fn class(&self) -> &'static str {
        match self {
            Self::Malformed(_) => "malformed",
            Self::UnknownKey => "unknown_key",
            Self::InvalidSignature => "invalid_signature",
            Self::InvalidPayload(_) => "invalid_payload",
            Self::Expired => "expired",
            Self::NotYetValid => "not_yet_valid",
            Self::Rollback { .. } => "rollback",
            Self::UntrustedCache(_) => "untrusted_cache",
            Self::Io(_) => "io",
            Self::Fetch(_) => "fetch",
        }
    }
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(reason) => write!(f, "malformed relay manifest: {reason}"),
            Self::UnknownKey => write!(f, "relay manifest uses an unknown verification key"),
            Self::InvalidSignature => write!(f, "relay manifest signature is invalid"),
            Self::InvalidPayload(reason) => write!(f, "invalid relay manifest payload: {reason}"),
            Self::Expired => write!(f, "relay manifest is expired"),
            Self::NotYetValid => write!(f, "relay manifest is not yet valid"),
            Self::Rollback { current, received } => write!(
                f,
                "relay manifest revision rollback current={current} received={received}"
            ),
            Self::UntrustedCache(reason) => {
                write!(
                    f,
                    "existing relay manifest cache cannot establish rollback floor: {reason}"
                )
            }
            Self::Io(error) => write!(f, "relay manifest cache I/O failed: {error}"),
            Self::Fetch(reason) => write!(f, "relay manifest fetch failed: {reason}"),
        }
    }
}

impl std::error::Error for ManifestError {}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    key_id: String,
    signature: String,
    payload: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestPayloadWire {
    schema: u64,
    revision: u64,
    generated_at: String,
    not_before: String,
    expires_at: String,
    relays: Vec<ManifestRelayWire>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRelayWire {
    id: String,
    url: String,
    priority: u32,
    failure_domain: String,
    enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestMetadata {
    pub revision: u64,
    pub key_id: String,
    pub generated_at: String,
    pub not_before: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayPoolLoad {
    pub relays: Vec<RelayEndpoint>,
    pub source: &'static str,
    pub metadata: Option<ManifestMetadata>,
    pub freshness: &'static str,
    pub error_class: Option<&'static str>,
    /// Last cryptographically trusted revision, including an expired cache.
    pub revision_floor: Option<u64>,
}

#[derive(Debug, Clone)]
struct VerifiedManifest {
    relays: Vec<RelayEndpoint>,
    metadata: ManifestMetadata,
    generated_at_unix: i64,
    not_before_unix: i64,
    expires_at_unix: i64,
}

pub fn signed_bytes(payload: &[u8]) -> Result<Vec<u8>, ManifestError> {
    if payload.len() > MAX_PAYLOAD_BYTES || payload.len() > u32::MAX as usize {
        return Err(ManifestError::Malformed("payload exceeds size limit"));
    }
    let mut message = Vec::with_capacity(SIGNING_DOMAIN.len() + 4 + payload.len());
    message.extend_from_slice(SIGNING_DOMAIN);
    message.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    message.extend_from_slice(payload);
    Ok(message)
}

/// Validate a signed envelope using the production verification-key registry.
///
/// `minimum_revision` is the last cryptographically accepted revision. New
/// manifests must advance strictly beyond it.
#[allow(dead_code)]
pub fn validate_envelope(
    raw: &[u8],
    now: i64,
    minimum_revision: Option<u64>,
) -> Result<(Vec<RelayEndpoint>, ManifestMetadata), ManifestError> {
    validate_envelope_with_keys(raw, now, minimum_revision, production_verification_key)
}

fn validate_envelope_with_keys<F>(
    raw: &[u8],
    now: i64,
    minimum_revision: Option<u64>,
    key_lookup: F,
) -> Result<(Vec<RelayEndpoint>, ManifestMetadata), ManifestError>
where
    F: Fn(&str) -> Option<VerifyingKey>,
{
    let verified = verify_envelope_with_keys(raw, key_lookup)?;
    if let Some(current) = minimum_revision
        && verified.metadata.revision <= current
    {
        return Err(ManifestError::Rollback {
            current,
            received: verified.metadata.revision,
        });
    }
    validate_temporal(&verified, now)?;
    Ok((verified.relays, verified.metadata))
}

fn verify_envelope_with_keys<F>(
    raw: &[u8],
    key_lookup: F,
) -> Result<VerifiedManifest, ManifestError>
where
    F: Fn(&str) -> Option<VerifyingKey>,
{
    if raw.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::Malformed("envelope exceeds size limit"));
    }
    let envelope: Envelope = serde_json::from_slice(raw)
        .map_err(|_| ManifestError::Malformed("envelope is not strict JSON"))?;
    validate_key_id(&envelope.key_id)?;
    if envelope.signature.is_empty() || envelope.signature.len() > MAX_SIGNATURE_B64_LEN {
        return Err(ManifestError::Malformed(
            "signature field exceeds size limit",
        ));
    }
    let max_payload_b64_len = MAX_PAYLOAD_BYTES.div_ceil(3) * 4;
    if envelope.payload.is_empty() || envelope.payload.len() > max_payload_b64_len {
        return Err(ManifestError::Malformed("payload field exceeds size limit"));
    }

    let key = key_lookup(&envelope.key_id).ok_or(ManifestError::UnknownKey)?;
    let payload_bytes = STANDARD
        .decode(envelope.payload.as_bytes())
        .map_err(|_| ManifestError::Malformed("payload is not valid base64"))?;
    if payload_bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(ManifestError::Malformed("payload exceeds size limit"));
    }
    let signature_bytes = STANDARD
        .decode(envelope.signature.as_bytes())
        .map_err(|_| ManifestError::Malformed("signature is not valid base64"))?;
    if signature_bytes.len() != 64 {
        return Err(ManifestError::Malformed(
            "signature must be exactly 64 bytes",
        ));
    }
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| ManifestError::Malformed("signature encoding is invalid"))?;
    key.verify(&signed_bytes(&payload_bytes)?, &signature)
        .map_err(|_| ManifestError::InvalidSignature)?;

    let payload: ManifestPayloadWire = serde_json::from_slice(&payload_bytes)
        .map_err(|_| ManifestError::InvalidPayload("payload is not strict JSON"))?;
    validate_static_payload(payload, envelope.key_id)
}

fn validate_static_payload(
    payload: ManifestPayloadWire,
    key_id: String,
) -> Result<VerifiedManifest, ManifestError> {
    if payload.schema != SCHEMA_VERSION {
        return Err(ManifestError::InvalidPayload("unsupported schema"));
    }
    if payload.revision == 0 {
        return Err(ManifestError::InvalidPayload("revision must be positive"));
    }
    for value in [
        &payload.generated_at,
        &payload.not_before,
        &payload.expires_at,
    ] {
        if value.is_empty() || value.len() > MAX_TIMESTAMP_LEN {
            return Err(ManifestError::InvalidPayload(
                "validity timestamp exceeds size limit",
            ));
        }
    }
    let generated_at_unix = parse_time(&payload.generated_at)?;
    let not_before_unix = parse_time(&payload.not_before)?;
    let expires_at_unix = parse_time(&payload.expires_at)?;
    if generated_at_unix > expires_at_unix {
        return Err(ManifestError::InvalidPayload(
            "generated_at must not follow expires_at",
        ));
    }
    if not_before_unix >= expires_at_unix {
        return Err(ManifestError::InvalidPayload(
            "not_before must precede expires_at",
        ));
    }
    if payload.relays.len() > MAX_RELAYS {
        return Err(ManifestError::InvalidPayload("too many relays"));
    }

    let mut ids = HashSet::with_capacity(payload.relays.len());
    let mut relays = Vec::with_capacity(payload.relays.len());
    for relay in payload.relays {
        if relay.id.is_empty()
            || relay.id.len() > MAX_RELAY_ID_LEN
            || !relay
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ManifestError::InvalidPayload("invalid relay id"));
        }
        if !ids.insert(relay.id.clone()) {
            return Err(ManifestError::InvalidPayload("duplicate relay id"));
        }
        if relay.failure_domain.is_empty()
            || relay.failure_domain.len() > MAX_FAILURE_DOMAIN_LEN
            || !relay
                .failure_domain
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(ManifestError::InvalidPayload("invalid failure domain"));
        }
        if relay.priority > MAX_PRIORITY {
            return Err(ManifestError::InvalidPayload(
                "relay priority is out of bounds",
            ));
        }
        validate_relay_url(&relay.url)?;
        relays.push(RelayEndpoint {
            id: relay.id,
            url: relay.url,
            priority: relay.priority,
            failure_domain: relay.failure_domain,
            enabled: relay.enabled,
        });
    }

    Ok(VerifiedManifest {
        relays,
        metadata: ManifestMetadata {
            revision: payload.revision,
            key_id,
            generated_at: payload.generated_at,
            not_before: payload.not_before,
            expires_at: payload.expires_at,
        },
        generated_at_unix,
        not_before_unix,
        expires_at_unix,
    })
}

fn validate_temporal(manifest: &VerifiedManifest, now: i64) -> Result<(), ManifestError> {
    if manifest.generated_at_unix > now.saturating_add(GENERATED_FUTURE_SKEW_SECONDS) {
        return Err(ManifestError::NotYetValid);
    }
    if manifest.not_before_unix > now {
        return Err(ManifestError::NotYetValid);
    }
    if manifest.expires_at_unix <= now {
        return Err(ManifestError::Expired);
    }
    Ok(())
}

fn validate_key_id(key_id: &str) -> Result<(), ManifestError> {
    if key_id.is_empty()
        || key_id.len() > MAX_KEY_ID_LEN
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ManifestError::Malformed("invalid key_id"));
    }
    Ok(())
}

fn parse_time(value: &str) -> Result<i64, ManifestError> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map(|timestamp| timestamp.unix_timestamp())
        .map_err(|_| ManifestError::InvalidPayload("timestamp must be RFC3339"))
}

fn validate_relay_url(raw: &str) -> Result<(), ManifestError> {
    if raw.is_empty() || raw.len() > MAX_RELAY_URL_LEN {
        return Err(ManifestError::InvalidPayload(
            "relay URL exceeds size limit",
        ));
    }
    let url = Url::parse(raw).map_err(|_| ManifestError::InvalidPayload("relay URL is invalid"))?;
    if !matches!(url.scheme(), "wss" | "https")
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some()
    {
        return Err(ManifestError::InvalidPayload(
            "relay URL must use HTTPS/WSS without credentials, query, fragment, or explicit port",
        ));
    }
    if url
        .host()
        .is_some_and(|host| matches!(host, url::Host::Ipv4(_) | url::Host::Ipv6(_)))
    {
        return Err(ManifestError::InvalidPayload(
            "relay URL must not use an IP literal",
        ));
    }
    Ok(())
}

/// Read the cache synchronously without network I/O.
pub fn load_cached_pool(paths: &RuntimePaths, now: i64) -> RelayPoolLoad {
    load_cached_pool_with_keys(paths, now, production_verification_key)
}

fn load_cached_pool_with_keys<F>(paths: &RuntimePaths, now: i64, key_lookup: F) -> RelayPoolLoad
where
    F: Fn(&str) -> Option<VerifyingKey> + Copy,
{
    let fallback = default_embedded_relays();
    let raw = match read_cache_bytes(&paths.config_dir) {
        Ok(Some(raw)) => raw,
        Ok(None) => {
            return RelayPoolLoad {
                relays: fallback,
                source: "embedded",
                metadata: None,
                freshness: "missing",
                error_class: None,
                revision_floor: None,
            };
        }
        Err(error) => {
            return RelayPoolLoad {
                relays: fallback,
                source: "embedded",
                metadata: None,
                freshness: "unknown",
                error_class: Some(error.class()),
                revision_floor: None,
            };
        }
    };

    match verify_envelope_with_keys(&raw, key_lookup) {
        Ok(verified) => {
            let revision = verified.metadata.revision;
            let metadata = verified.metadata.clone();
            match validate_temporal(&verified, now) {
                Ok(()) => RelayPoolLoad {
                    relays: verified.relays,
                    source: "cached-remote",
                    metadata: Some(metadata),
                    freshness: "fresh",
                    error_class: None,
                    revision_floor: Some(revision),
                },
                Err(error) => RelayPoolLoad {
                    relays: fallback,
                    source: "embedded",
                    metadata: Some(metadata),
                    freshness: match error {
                        ManifestError::Expired => "expired",
                        ManifestError::NotYetValid => "not-yet-valid",
                        _ => "invalid",
                    },
                    error_class: Some(error.class()),
                    revision_floor: Some(revision),
                },
            }
        }
        Err(error) => RelayPoolLoad {
            relays: fallback,
            source: "embedded",
            metadata: None,
            freshness: "untrusted",
            error_class: Some(error.class()),
            revision_floor: None,
        },
    }
}

/// Atomically install a validated, strictly newer envelope as last-known-good.
#[allow(dead_code)]
pub fn accept_cache(
    paths: &RuntimePaths,
    raw: &[u8],
    now: i64,
) -> Result<ManifestMetadata, ManifestError> {
    accept_cache_with_keys(paths, raw, now, production_verification_key)
}

fn accept_cache_with_keys<F>(
    paths: &RuntimePaths,
    raw: &[u8],
    now: i64,
    key_lookup: F,
) -> Result<ManifestMetadata, ManifestError>
where
    F: Fn(&str) -> Option<VerifyingKey> + Copy,
{
    let current_revision = trusted_cached_revision(paths, key_lookup)?;
    let (_, metadata) = validate_envelope_with_keys(raw, now, current_revision, key_lookup)?;
    write_cache_atomic(&paths.config_dir, raw)?;
    Ok(metadata)
}

fn trusted_cached_revision<F>(
    paths: &RuntimePaths,
    key_lookup: F,
) -> Result<Option<u64>, ManifestError>
where
    F: Fn(&str) -> Option<VerifyingKey>,
{
    match read_cache_bytes(&paths.config_dir)? {
        Some(raw) => verify_envelope_with_keys(&raw, key_lookup)
            .map(|verified| Some(verified.metadata.revision))
            .map_err(|error| ManifestError::UntrustedCache(error.class())),
        None => Ok(None),
    }
}

fn read_cache_bytes(config_dir: &Path) -> Result<Option<Vec<u8>>, ManifestError> {
    read_cache_bytes_inner(config_dir, |_| {})
}

fn read_cache_bytes_inner<F>(
    config_dir: &Path,
    before_open: F,
) -> Result<Option<Vec<u8>>, ManifestError>
where
    F: FnOnce(&Path),
{
    let cache = cache_path(config_dir);
    let metadata = match fs::symlink_metadata(&cache) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ManifestError::UntrustedCache(
            "cache path is not a regular owned file",
        ));
    }
    if metadata.len() > MAX_MANIFEST_BYTES as u64 {
        return Err(ManifestError::UntrustedCache("cache exceeds size limit"));
    }
    before_open(&cache);
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options.open(&cache).map_err(io_error)?;
    let opened_metadata = file.metadata().map_err(io_error)?;
    if !opened_metadata.is_file() || opened_metadata.len() > MAX_MANIFEST_BYTES as u64 {
        return Err(ManifestError::UntrustedCache(
            "cache changed before it could be opened safely",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.dev() != opened_metadata.dev() || metadata.ino() != opened_metadata.ino() {
            return Err(ManifestError::UntrustedCache(
                "cache changed before it could be opened safely",
            ));
        }
    }
    read_bounded(&mut file, MAX_MANIFEST_BYTES)
        .map(Some)
        .map_err(|error| ManifestError::UntrustedCache(error.class()))
}

fn write_cache_atomic(config_dir: &Path, raw: &[u8]) -> Result<(), ManifestError> {
    if raw.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::Malformed("envelope exceeds size limit"));
    }
    ensure_secure_dir(config_dir, "config dir")?;
    let dir = config_dir.join("relay-pool");
    ensure_secure_dir(&dir, "relay cache dir")?;
    let cache = cache_path(config_dir);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = dir.join(format!(
        ".{CACHE_FILE_NAME}.tmp-{}-{stamp}",
        std::process::id()
    ));

    let write_result = (|| -> Result<(), ManifestError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).map_err(io_error)?;
        file.write_all(raw).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        drop(file);
        if cache.exists() {
            // Reuse the repository's cross-platform transactional replacement
            // semantics for an existing managed state file. The relay cache has
            // no live reader: Link only reads it during daemon construction.
            crate::mutation::atomic_write(&cache, raw)
                .map_err(|error| ManifestError::Io(error.to_string()))?;
            fs::remove_file(&tmp).map_err(io_error)?;
        } else {
            fs::rename(&tmp, &cache).map_err(io_error)?;
        }
        #[cfg(unix)]
        fs::File::open(&dir)
            .and_then(|directory| directory.sync_all())
            .map_err(io_error)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result
}

fn ensure_secure_dir(path: &Path, label: &'static str) -> Result<(), ManifestError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ManifestError::Io(format!(
                    "{label} is not an owned directory"
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(io_error)?;
        }
        Err(error) => return Err(io_error(error)),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    }
    Ok(())
}

pub fn cache_path(config_dir: &Path) -> PathBuf {
    config_dir.join("relay-pool").join(CACHE_FILE_NAME)
}

fn io_error(error: std::io::Error) -> ManifestError {
    ManifestError::Io(error.to_string())
}

fn production_verification_key(key_id: &str) -> Option<VerifyingKey> {
    match key_id {
        RELAY_PROD_2026_09_KEY_ID => VerifyingKey::from_bytes(&RELAY_PROD_2026_09_PUBLIC_KEY).ok(),
        _ => None,
    }
}

/// Build a bounded, no-redirect HTTPS client and fetch one manifest envelope.
///
/// This function is intentionally not called from the healthy Link startup path.
#[allow(dead_code)]
pub fn fetch_manifest(
    url: &str,
    connect_timeout: Duration,
    total_timeout: Duration,
) -> Result<Vec<u8>, ManifestError> {
    let parsed = validate_manifest_url(url)?;
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect_timeout)
        .timeout(total_timeout)
        .build()
        .map_err(|_| ManifestError::Fetch("client_build_failed"))?;
    let mut response = client
        .get(parsed)
        .send()
        .map_err(|_| ManifestError::Fetch("request_failed"))?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(ManifestError::Fetch("unexpected_http_status"));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_MANIFEST_BYTES as u64)
    {
        return Err(ManifestError::Malformed(
            "manifest response exceeds size limit",
        ));
    }
    read_bounded(&mut response, MAX_MANIFEST_BYTES)
}

fn validate_manifest_url(raw: &str) -> Result<Url, ManifestError> {
    if raw.is_empty() || raw.len() > MAX_RELAY_URL_LEN {
        return Err(ManifestError::Malformed("manifest URL exceeds size limit"));
    }
    let url = Url::parse(raw).map_err(|_| ManifestError::Malformed("manifest URL is invalid"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some()
    {
        return Err(ManifestError::Malformed(
            "manifest URL must use HTTPS without credentials, query, fragment, or explicit port",
        ));
    }
    if url
        .host()
        .is_some_and(|host| matches!(host, url::Host::Ipv4(_) | url::Host::Ipv6(_)))
    {
        return Err(ManifestError::Malformed(
            "manifest URL must not use an IP literal",
        ));
    }
    Ok(url)
}

fn read_bounded<R: Read>(reader: &mut R, max_bytes: usize) -> Result<Vec<u8>, ManifestError> {
    let mut limited = reader.take((max_bytes as u64).saturating_add(1));
    let mut bytes = Vec::with_capacity(max_bytes.min(16 * 1024));
    limited
        .read_to_end(&mut bytes)
        .map_err(|_| ManifestError::Fetch("response_read_failed"))?;
    if bytes.len() > max_bytes {
        return Err(ManifestError::Malformed(
            "manifest response exceeds size limit",
        ));
    }
    Ok(bytes)
}

pub fn status_line(paths: &RuntimePaths, now: i64) -> String {
    let pool = load_cached_pool(paths, now);
    let revision = pool
        .metadata
        .as_ref()
        .map(|metadata| metadata.revision.to_string())
        .unwrap_or_else(|| "none".to_owned());
    let key_id = pool
        .metadata
        .as_ref()
        .map(|metadata| metadata.key_id.as_str())
        .unwrap_or("none");
    let error = pool.error_class.unwrap_or("none");
    format!(
        "source={} revision={} key_id={} freshness={} error={}",
        pool.source, revision, key_id, pool.freshness, error
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::{Value, json};
    use std::fs;
    use std::io::{self, Cursor};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const TEST_NOW: i64 = 1_735_689_600; // 2025-01-01T00:00:00Z

    fn test_signing_key() -> SigningKey {
        // Test-only deterministic key; this code is not present in production builds.
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn test_key_lookup(key_id: &str) -> Option<VerifyingKey> {
        (key_id == "test-key").then(|| test_signing_key().verifying_key())
    }

    fn relay(id: &str, domain: &str) -> Value {
        json!({
            "id": id,
            "url": "wss://relay.example/v1",
            "priority": 100,
            "failure_domain": domain,
            "enabled": true
        })
    }

    fn payload(
        revision: u64,
        generated_at: &str,
        not_before: &str,
        expires_at: &str,
        relays: Value,
    ) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema": 1,
            "revision": revision,
            "generated_at": generated_at,
            "not_before": not_before,
            "expires_at": expires_at,
            "relays": relays
        }))
        .unwrap()
    }

    fn envelope(payload: &[u8], key_id: &str) -> Vec<u8> {
        let signature = test_signing_key().sign(&signed_bytes(payload).unwrap());
        serde_json::to_vec(&json!({
            "key_id": key_id,
            "signature": STANDARD.encode(signature.to_bytes()),
            "payload": STANDARD.encode(payload)
        }))
        .unwrap()
    }

    fn valid(revision: u64) -> Vec<u8> {
        envelope(
            &payload(
                revision,
                "2024-12-31T23:59:00Z",
                "2024-12-31T00:00:00Z",
                "2030-01-01T00:00:00Z",
                json!([relay("a", "provider-a")]),
            ),
            "test-key",
        )
    }

    fn test_paths(name: &str) -> RuntimePaths {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let config_dir = std::env::temp_dir().join(format!(
            "herdr-relay-manifest-{name}-{}-{stamp}",
            std::process::id()
        ));
        RuntimePaths {
            instance: crate::instance::InstanceId::default_instance(),
            config_file: config_dir.join("config.toml"),
            dev_state_dir: config_dir.join("dev-state"),
            config_dir,
            herdr_socket: None,
        }
    }

    #[test]
    fn exact_signed_bytes_are_domain_and_big_endian_length() {
        let got = signed_bytes(b"abc").unwrap();
        assert_eq!(&got[..SIGNING_DOMAIN.len()], SIGNING_DOMAIN);
        assert_eq!(
            &got[SIGNING_DOMAIN.len()..SIGNING_DOMAIN.len() + 4],
            &[0, 0, 0, 3]
        );
        assert_eq!(&got[SIGNING_DOMAIN.len() + 4..], b"abc");
    }

    #[test]
    fn real_validation_flow_accepts_valid_signature_and_rejects_tampering_unknown_key_and_rollback()
    {
        let raw = valid(2);
        let (relays, metadata) =
            validate_envelope_with_keys(&raw, TEST_NOW, None, test_key_lookup).unwrap();
        assert_eq!(relays.len(), 1);
        assert_eq!(metadata.revision, 2);

        let parsed: Value = serde_json::from_slice(&raw).unwrap();
        let mut payload_bytes = STANDARD
            .decode(parsed["payload"].as_str().unwrap())
            .unwrap();
        payload_bytes[0] ^= 1;
        let tampered = serde_json::to_vec(&json!({
            "key_id": "test-key",
            "signature": parsed["signature"],
            "payload": STANDARD.encode(payload_bytes)
        }))
        .unwrap();
        assert!(matches!(
            validate_envelope_with_keys(&tampered, TEST_NOW, None, test_key_lookup),
            Err(ManifestError::InvalidSignature)
        ));

        let unknown = envelope(
            &payload(
                3,
                "2024-12-31T23:59:00Z",
                "2024-12-31T00:00:00Z",
                "2030-01-01T00:00:00Z",
                json!([]),
            ),
            "unknown-key",
        );
        assert!(matches!(
            validate_envelope_with_keys(&unknown, TEST_NOW, None, test_key_lookup),
            Err(ManifestError::UnknownKey)
        ));
        assert!(matches!(
            validate_envelope_with_keys(&raw, TEST_NOW, Some(2), test_key_lookup),
            Err(ManifestError::Rollback {
                current: 2,
                received: 2
            })
        ));
    }

    #[test]
    fn temporal_revision_and_timestamp_ordering_fail_closed() {
        let expired = envelope(
            &payload(
                1,
                "2024-01-01T00:00:00Z",
                "2024-01-01T00:00:00Z",
                "2024-12-31T00:00:00Z",
                json!([]),
            ),
            "test-key",
        );
        assert!(matches!(
            validate_envelope_with_keys(&expired, TEST_NOW, None, test_key_lookup),
            Err(ManifestError::Expired)
        ));

        let future = envelope(
            &payload(
                1,
                "2030-01-01T00:00:00Z",
                "2030-01-01T00:00:00Z",
                "2031-01-01T00:00:00Z",
                json!([]),
            ),
            "test-key",
        );
        assert!(matches!(
            validate_envelope_with_keys(&future, TEST_NOW, None, test_key_lookup),
            Err(ManifestError::NotYetValid)
        ));

        let zero = envelope(
            &payload(
                0,
                "2024-01-01T00:00:00Z",
                "2024-01-01T00:00:00Z",
                "2030-01-01T00:00:00Z",
                json!([]),
            ),
            "test-key",
        );
        assert!(matches!(
            validate_envelope_with_keys(&zero, TEST_NOW, None, test_key_lookup),
            Err(ManifestError::InvalidPayload("revision must be positive"))
        ));

        let bad_order = envelope(
            &payload(
                1,
                "2031-01-01T00:00:00Z",
                "2024-01-01T00:00:00Z",
                "2030-01-01T00:00:00Z",
                json!([]),
            ),
            "test-key",
        );
        assert!(matches!(
            validate_envelope_with_keys(&bad_order, TEST_NOW, None, test_key_lookup),
            Err(ManifestError::InvalidPayload(
                "generated_at must not follow expires_at"
            ))
        ));
    }

    #[test]
    fn unknown_fields_duplicate_ids_and_unsafe_urls_are_rejected() {
        let payload_with_unknown = serde_json::to_vec(&json!({
            "schema": 1,
            "revision": 1,
            "generated_at": "2024-12-31T23:59:00Z",
            "not_before": "2024-12-31T00:00:00Z",
            "expires_at": "2030-01-01T00:00:00Z",
            "relays": [],
            "extra": true
        }))
        .unwrap();
        assert!(matches!(
            validate_envelope_with_keys(
                &envelope(&payload_with_unknown, "test-key"),
                TEST_NOW,
                None,
                test_key_lookup
            ),
            Err(ManifestError::InvalidPayload("payload is not strict JSON"))
        ));

        let base = valid(1);
        let mut envelope_value: Value = serde_json::from_slice(&base).unwrap();
        envelope_value["extra"] = json!(true);
        assert!(matches!(
            validate_envelope_with_keys(
                &serde_json::to_vec(&envelope_value).unwrap(),
                TEST_NOW,
                None,
                test_key_lookup
            ),
            Err(ManifestError::Malformed("envelope is not strict JSON"))
        ));

        let duplicate = envelope(
            &payload(
                1,
                "2024-12-31T23:59:00Z",
                "2024-12-31T00:00:00Z",
                "2030-01-01T00:00:00Z",
                json!([relay("a", "one"), relay("a", "two")]),
            ),
            "test-key",
        );
        assert!(matches!(
            validate_envelope_with_keys(&duplicate, TEST_NOW, None, test_key_lookup),
            Err(ManifestError::InvalidPayload("duplicate relay id"))
        ));

        let unsafe_url = envelope(
            &payload(
                1,
                "2024-12-31T23:59:00Z",
                "2024-12-31T00:00:00Z",
                "2030-01-01T00:00:00Z",
                json!([{
                    "id": "bad",
                    "url": "http://relay.example/v1",
                    "priority": 1,
                    "failure_domain": "provider",
                    "enabled": true
                }]),
            ),
            "test-key",
        );
        assert!(matches!(
            validate_envelope_with_keys(&unsafe_url, TEST_NOW, None, test_key_lookup),
            Err(ManifestError::InvalidPayload(_))
        ));

        let relay_unknown_field = envelope(
            &payload(
                1,
                "2024-12-31T23:59:00Z",
                "2024-12-31T00:00:00Z",
                "2030-01-01T00:00:00Z",
                json!([{
                    "id": "strict",
                    "url": "wss://relay.example/v1",
                    "priority": 1,
                    "failure_domain": "provider",
                    "enabled": true,
                    "unexpected": true
                }]),
            ),
            "test-key",
        );
        assert!(matches!(
            validate_envelope_with_keys(&relay_unknown_field, TEST_NOW, None, test_key_lookup),
            Err(ManifestError::InvalidPayload("payload is not strict JSON"))
        ));
    }

    #[test]
    fn expired_signed_cache_supplies_revision_floor_but_no_routes_and_blocks_rollback() {
        let paths = test_paths("expired-floor");
        let cached = envelope(
            &payload(
                5,
                "2024-01-01T00:00:00Z",
                "2024-01-01T00:00:00Z",
                "2025-01-02T00:00:00Z",
                json!([relay("old", "provider-old")]),
            ),
            "test-key",
        );
        accept_cache_with_keys(&paths, &cached, TEST_NOW, test_key_lookup).unwrap();
        let expired_now = TEST_NOW + 172_800;
        let loaded = load_cached_pool_with_keys(&paths, expired_now, test_key_lookup);
        assert!(loaded.relays.is_empty());
        assert_eq!(loaded.revision_floor, Some(5));
        assert_eq!(loaded.freshness, "expired");

        let rollback = envelope(
            &payload(
                4,
                "2025-01-02T00:00:00Z",
                "2025-01-02T00:00:00Z",
                "2030-01-01T00:00:00Z",
                json!([relay("newer-time", "provider-new")]),
            ),
            "test-key",
        );
        assert!(matches!(
            accept_cache_with_keys(&paths, &rollback, expired_now, test_key_lookup),
            Err(ManifestError::Rollback {
                current: 5,
                received: 4
            })
        ));
        let _ = fs::remove_dir_all(&paths.config_dir);
    }

    #[test]
    fn invalid_remote_never_replaces_good_cache_and_cache_permissions_are_private() {
        let paths = test_paths("atomic-cache");
        let good = valid(2);
        accept_cache_with_keys(&paths, &good, TEST_NOW, test_key_lookup).unwrap();
        let before = fs::read(cache_path(&paths.config_dir)).unwrap();

        let parsed: Value = serde_json::from_slice(&valid(3)).unwrap();
        let bad = serde_json::to_vec(&json!({
            "key_id": "test-key",
            "signature": STANDARD.encode([0u8; 64]),
            "payload": parsed["payload"]
        }))
        .unwrap();
        assert!(matches!(
            accept_cache_with_keys(&paths, &bad, TEST_NOW, test_key_lookup),
            Err(ManifestError::InvalidSignature)
        ));
        assert_eq!(fs::read(cache_path(&paths.config_dir)).unwrap(), before);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(paths.config_dir.join("relay-pool"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(cache_path(&paths.config_dir))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let leftovers = fs::read_dir(paths.config_dir.join("relay-pool"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0);
        let _ = fs::remove_dir_all(&paths.config_dir);
    }

    #[test]
    fn untrusted_existing_cache_fails_closed_for_replacement() {
        let paths = test_paths("untrusted-cache");
        fs::create_dir_all(paths.config_dir.join("relay-pool")).unwrap();
        fs::write(cache_path(&paths.config_dir), b"not-json").unwrap();
        assert!(matches!(
            accept_cache_with_keys(&paths, &valid(2), TEST_NOW, test_key_lookup),
            Err(ManifestError::UntrustedCache(_))
        ));
        let _ = fs::remove_dir_all(&paths.config_dir);
    }

    #[test]
    fn oversized_or_symlink_cache_is_rejected_before_unbounded_read() {
        let paths = test_paths("unsafe-cache-path");
        fs::create_dir_all(paths.config_dir.join("relay-pool")).unwrap();
        let cache = cache_path(&paths.config_dir);
        fs::write(&cache, vec![b'x'; MAX_MANIFEST_BYTES + 1]).unwrap();
        assert!(matches!(
            read_cache_bytes(&paths.config_dir),
            Err(ManifestError::UntrustedCache("cache exceeds size limit"))
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            fs::remove_file(&cache).unwrap();
            let target = paths.config_dir.join("signed-elsewhere.json");
            fs::write(&target, valid(9)).unwrap();
            symlink(&target, &cache).unwrap();
            assert!(matches!(
                read_cache_bytes(&paths.config_dir),
                Err(ManifestError::UntrustedCache(
                    "cache path is not a regular owned file"
                ))
            ));

            // Reproduce the audit finding: the cache is regular during the
            // metadata check, then an attacker replaces it with a symlink
            // immediately before open. The opened handle must fail closed.
            fs::remove_file(&cache).unwrap();
            fs::write(&cache, valid(10)).unwrap();
            let raced_target = paths.config_dir.join("raced-target.json");
            fs::write(&raced_target, valid(11)).unwrap();
            let raced = read_cache_bytes_inner(&paths.config_dir, |path| {
                fs::remove_file(path).unwrap();
                symlink(&raced_target, path).unwrap();
            });
            assert!(raced.is_err());
        }
        let _ = fs::remove_dir_all(&paths.config_dir);
    }

    struct CountingReader {
        remaining: usize,
        reads: AtomicUsize,
    }

    impl Read for CountingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if self.remaining == 0 {
                return Ok(0);
            }
            let count = self.remaining.min(buffer.len());
            buffer[..count].fill(b'x');
            self.remaining -= count;
            Ok(count)
        }
    }

    #[test]
    fn bounded_reader_rejects_overflow_without_unbounded_read() {
        let mut reader = CountingReader {
            remaining: MAX_MANIFEST_BYTES * 8,
            reads: AtomicUsize::new(0),
        };
        assert!(matches!(
            read_bounded(&mut reader, MAX_MANIFEST_BYTES),
            Err(ManifestError::Malformed(
                "manifest response exceeds size limit"
            ))
        ));
        assert!(reader.remaining > MAX_MANIFEST_BYTES * 6);
        assert!(reader.reads.load(Ordering::Relaxed) > 0);

        let mut exact = Cursor::new(vec![b'x'; MAX_MANIFEST_BYTES]);
        assert_eq!(
            read_bounded(&mut exact, MAX_MANIFEST_BYTES).unwrap().len(),
            MAX_MANIFEST_BYTES
        );
    }

    #[test]
    fn production_registry_contains_only_public_trust_and_embedded_pool_remains_empty() {
        assert!(production_verification_key(RELAY_PROD_2026_09_KEY_ID).is_some());
        assert!(production_verification_key("test-key").is_none());
        assert!(production_verification_key("unknown-key").is_none());
        assert!(default_embedded_relays().is_empty());
    }

    #[test]
    fn ladder_consumes_only_relays_loaded_from_a_validated_cache() {
        use crate::link::ladder::{
            DEFAULT_MAX_FAILURES_PER_ROUTE, TransportLadder, TransportRouteKind,
        };

        let paths = test_paths("ladder-validated-only");
        accept_cache_with_keys(&paths, &valid(7), TEST_NOW, test_key_lookup).unwrap();
        let loaded = load_cached_pool_with_keys(&paths, TEST_NOW, test_key_lookup);
        assert_eq!(loaded.source, "cached-remote");
        assert_eq!(loaded.revision_floor, Some(7));

        let ladder = TransportLadder::from_config(
            "wss://my-worker.workers.dev/ws",
            None,
            Some("https://my-worker.workers.dev"),
            "dev_test",
            None,
            &loaded.relays,
            DEFAULT_MAX_FAILURES_PER_ROUTE,
        )
        .unwrap();
        assert!(
            ladder
                .routes()
                .iter()
                .any(|route| route.kind == TransportRouteKind::SharedRelay)
        );

        fs::write(cache_path(&paths.config_dir), b"corrupt-cache").unwrap();
        let rejected = load_cached_pool_with_keys(&paths, TEST_NOW, test_key_lookup);
        assert!(rejected.relays.is_empty());
        assert_eq!(rejected.source, "embedded");
        assert_eq!(rejected.freshness, "untrusted");
        let ladder = TransportLadder::from_config(
            "wss://my-worker.workers.dev/ws",
            None,
            Some("https://my-worker.workers.dev"),
            "dev_test",
            None,
            &rejected.relays,
            DEFAULT_MAX_FAILURES_PER_ROUTE,
        )
        .unwrap();
        assert!(
            ladder
                .routes()
                .iter()
                .all(|route| route.kind != TransportRouteKind::SharedRelay)
        );
        let _ = fs::remove_dir_all(&paths.config_dir);
    }

    #[test]
    fn manifest_url_is_https_only_and_has_no_target_override_fields() {
        assert!(validate_manifest_url("https://example.com/relay-pool.json").is_ok());
        for invalid in [
            "http://example.com/relay.json",
            "https://user@example.com/relay.json",
            "https://example.com:8443/relay.json",
            "https://example.com/relay.json?target=x",
            "https://127.0.0.1/relay.json",
        ] {
            assert!(validate_manifest_url(invalid).is_err(), "{invalid}");
        }
    }
}
