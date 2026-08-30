use crate::{fs_mutation, herdr::HerdrClient, paths::RuntimePaths, snapshot};
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, LOCATION};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::process::ExitCode;
use std::time::Duration;
use url::{Host, Url};

pub(crate) const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_CAPABILITY_ENV: &str = "HERDR_ARTIFACT_CAPABILITY";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportArgs {
    pub url: String,
    pub path: String,
    pub expected_sha256: Option<String>,
    pub capability_env: String,
    pub signed_url: bool,
    pub overwrite: bool,
    pub confirm_dirty: bool,
    pub confirm_busy: bool,
}

#[derive(Debug)]
struct FetchedArtifact {
    bytes: Vec<u8>,
    mime_type: &'static str,
    sha256: String,
    final_url: String,
}

pub fn run(args: ImportArgs) -> Result<ExitCode, String> {
    let capability = if args.signed_url {
        None
    } else {
        let capability = std::env::var(&args.capability_env).map_err(|_| {
            format!(
                "artifact capability is missing; set {} in the process environment",
                args.capability_env
            )
        })?;
        if capability.is_empty() || capability.len() > 512 {
            return Err("artifact capability has invalid length".to_owned());
        }
        Some(capability)
    };
    let artifact = fetch_artifact(
        &args.url,
        capability.as_deref(),
        args.expected_sha256.as_deref(),
    )?;

    let paths = RuntimePaths::discover()?;
    let socket = paths
        .herdr_socket
        .as_ref()
        .ok_or_else(|| "Herdr socket path is unavailable".to_owned())?;
    let snapshot = snapshot::fetch(&HerdrClient::new(socket))?.value;
    let result = fs_mutation::write_bytes(
        &snapshot,
        &args.path,
        &artifact.bytes,
        args.overwrite,
        args.confirm_dirty,
        args.confirm_busy,
    );
    if !result.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        println!(
            "{}",
            serde_json::to_string(&result).map_err(|error| error.to_string())?
        );
        return Ok(ExitCode::FAILURE);
    }

    println!(
        "{}",
        serde_json::to_string(&json!({
            "ok": true,
            "path": args.path,
            "bytes": artifact.bytes.len(),
            "mime_type": artifact.mime_type,
            "sha256": artifact.sha256,
            "source": if args.signed_url { "signed_https_url" } else { "r2_artifact_relay" },
            "final_url": artifact.final_url,
            "write": result,
        }))
        .map_err(|error| error.to_string())?
    );
    Ok(ExitCode::SUCCESS)
}

fn fetch_artifact(
    source: &str,
    capability: Option<&str>,
    expected_sha256: Option<&str>,
) -> Result<FetchedArtifact, String> {
    let mut url = parse_allowed_url(source)?;
    let origin = origin_tuple(&url)?;
    for redirect_count in 0..=MAX_REDIRECTS {
        let client = pinned_client(&url)?;
        let mut request = client.get(url.clone());
        if let Some(capability) = capability {
            request = request.header(AUTHORIZATION, format!("Bearer {capability}"));
        }
        let response = request
            .send()
            .map_err(|error| format_reqwest_error("artifact fetch failed", &error))?;
        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err("artifact redirect limit exceeded".to_owned());
            }
            let location = response
                .headers()
                .get(LOCATION)
                .ok_or_else(|| "artifact redirect is missing Location".to_owned())?
                .to_str()
                .map_err(|_| "artifact redirect Location is invalid".to_owned())?;
            let next = url
                .join(location)
                .map_err(|error| format!("invalid artifact redirect URL: {error}"))?;
            let next = parse_allowed_url(next.as_str())?;
            if origin_tuple(&next)? != origin {
                return Err("cross-origin artifact redirect rejected".to_owned());
            }
            url = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!(
                "artifact fetch returned HTTP {}",
                response.status().as_u16()
            ));
        }
        return read_artifact(response, &url, expected_sha256);
    }
    Err("artifact redirect limit exceeded".to_owned())
}

fn format_reqwest_error(prefix: &str, error: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut message = format!("{prefix}: {error}");
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

fn read_artifact(
    response: Response,
    final_url: &Url,
    expected_sha256: Option<&str>,
) -> Result<FetchedArtifact, String> {
    let response_digest = response_digest_header(&response);
    if let Some(length) = response.headers().get(CONTENT_LENGTH) {
        let length = length
            .to_str()
            .map_err(|_| "invalid artifact Content-Length".to_owned())?
            .parse::<usize>()
            .map_err(|_| "invalid artifact Content-Length".to_owned())?;
        if length == 0 || length > MAX_ARTIFACT_BYTES {
            return Err("artifact size is outside the allowed range".to_owned());
        }
    }
    let declared_mime = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .ok_or_else(|| "artifact response is missing Content-Type".to_owned())?;
    let mime_type = supported_mime(declared_mime)
        .ok_or_else(|| format!("unsupported artifact MIME type '{declared_mime}'"))?;

    let mut bytes = Vec::new();
    response
        .take((MAX_ARTIFACT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("artifact body read failed: {error}"))?;
    if bytes.is_empty() || bytes.len() > MAX_ARTIFACT_BYTES {
        return Err("artifact size is outside the allowed range".to_owned());
    }
    if is_image_mime(mime_type) && !magic_matches(mime_type, &bytes) {
        return Err("artifact MIME type does not match file signature".to_owned());
    }

    let sha256 = hex_sha256(&bytes);
    if let Some(value) = response_digest.as_deref() {
        validate_digest(value)?;
        if !value.eq_ignore_ascii_case(&sha256) {
            return Err("artifact response digest mismatch".to_owned());
        }
    }
    if let Some(expected) = expected_sha256 {
        validate_digest(expected)?;
        if !expected.eq_ignore_ascii_case(&sha256) {
            return Err("artifact expected digest mismatch".to_owned());
        }
    }

    Ok(FetchedArtifact {
        bytes,
        mime_type,
        sha256,
        final_url: redacted_url(final_url),
    })
}

fn response_digest_header(response: &Response) -> Option<String> {
    response
        .headers()
        .get("x-artifact-sha256")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn pinned_client(url: &Url) -> Result<Client, String> {
    let host = url
        .host()
        .ok_or_else(|| "artifact URL host is missing".to_owned())?;
    let port = url.port_or_known_default().unwrap_or(443);
    let mut builder = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(format!("herdr-mcp/{}", env!("CARGO_PKG_VERSION")));

    match host {
        Host::Domain(domain) => {
            let addresses = resolve_public(domain, port)?;
            builder = builder.resolve_to_addrs(domain, &addresses);
        }
        Host::Ipv4(address) => ensure_public_ip(IpAddr::V4(address))?,
        Host::Ipv6(address) => ensure_public_ip(IpAddr::V6(address))?,
    }
    builder
        .build()
        .map_err(|error| format!("cannot build artifact HTTP client: {error}"))
}

fn parse_allowed_url(source: &str) -> Result<Url, String> {
    let url = Url::parse(source).map_err(|error| format!("invalid artifact URL: {error}"))?;
    if url.scheme() != "https" {
        return Err("artifact URL must use https".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("artifact URL credentials are forbidden".to_owned());
    }
    if url.host().is_none() {
        return Err("artifact URL host is missing".to_owned());
    }
    if url.fragment().is_some() {
        return Err("artifact URL fragment is forbidden".to_owned());
    }
    if let Some(Host::Ipv4(address)) = url.host() {
        ensure_public_ip(IpAddr::V4(address))?;
    }
    if let Some(Host::Ipv6(address)) = url.host() {
        ensure_public_ip(IpAddr::V6(address))?;
    }
    Ok(url)
}

fn resolve_public(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("artifact DNS resolution failed: {error}"))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err("artifact DNS resolution returned no addresses".to_owned());
    }
    for address in &addresses {
        ensure_public_ip(address.ip())?;
    }
    Ok(addresses)
}

fn ensure_public_ip(ip: IpAddr) -> Result<(), String> {
    let allowed = match ip {
        IpAddr::V4(address) => ipv4_public(address),
        IpAddr::V6(address) => ipv6_public(address),
    };
    if allowed {
        Ok(())
    } else {
        Err("artifact URL resolved to a non-public IP address".to_owned())
    }
}

fn ipv4_public(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        || octets[0] >= 240)
}

fn ipv6_public(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return ipv4_public(mapped);
    }
    let segments = ip.segments();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

pub(crate) fn supported_mime(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        // images (strict magic enforced)
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        // inert text / docs
        "text/plain" => Some("text/plain"),
        "text/markdown" => Some("text/markdown"),
        "text/csv" => Some("text/csv"),
        "text/css" => Some("text/css"),
        "application/json" => Some("application/json"),
        "application/pdf" => Some("application/pdf"),
        // archives
        "application/zip" => Some("application/zip"),
        "application/gzip" => Some("application/gzip"),
        "application/x-tar" => Some("application/x-tar"),
        // opaque fallback
        "application/octet-stream" => Some("application/octet-stream"),
        _ => None,
    }
}

pub(crate) fn is_image_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

pub(crate) fn magic_matches(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => {
            bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP")
        }
        _ => false,
    }
}

pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_digest(value: &str) -> Result<(), String> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("artifact SHA-256 must be 64 hexadecimal characters".to_owned())
    }
}

fn origin_tuple(url: &Url) -> Result<(String, String, u16), String> {
    Ok((
        url.scheme().to_owned(),
        url.host_str()
            .ok_or_else(|| "artifact URL host is missing".to_owned())?
            .to_ascii_lowercase(),
        url.port_or_known_default().unwrap_or(443),
    ))
}

fn redacted_url(url: &Url) -> String {
    let mut output = url.clone();
    output.set_query(None);
    output.set_fragment(None);
    output.to_string()
}

pub fn default_capability_env() -> &'static str {
    DEFAULT_CAPABILITY_ENV
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_policy_requires_https_and_rejects_credentials_and_private_ips() {
        assert!(parse_allowed_url("http://example.com/a").is_err());
        assert!(parse_allowed_url("https://user:pass@example.com/a").is_err());
        assert!(parse_allowed_url("https://127.0.0.1/a").is_err());
        assert!(parse_allowed_url("https://10.0.0.1/a").is_err());
        assert!(parse_allowed_url("https://169.254.1.1/a").is_err());
        assert!(parse_allowed_url("https://[::1]/a").is_err());
        assert!(parse_allowed_url("https://[fc00::1]/a").is_err());
        assert!(parse_allowed_url("https://[fe80::1]/a").is_err());
        assert!(parse_allowed_url("https://8.8.8.8/a").is_ok());
    }

    #[test]
    fn image_signature_and_digest_validation_are_bounded() {
        let png = b"\x89PNG\r\n\x1a\nrest";
        assert!(magic_matches("image/png", png));
        assert!(!magic_matches("image/jpeg", png));
        let digest = hex_sha256(png);
        assert_eq!(digest.len(), 64);
        assert!(validate_digest(&digest).is_ok());
        assert!(validate_digest("nope").is_err());
    }

    #[test]
    fn non_image_allowlist_and_octet_stream_are_accepted_without_magic() {
        for mime in [
            "text/plain",
            "text/markdown",
            "text/csv",
            "text/css",
            "application/json",
            "application/pdf",
            "application/zip",
            "application/gzip",
            "application/x-tar",
            "application/octet-stream",
        ] {
            assert_eq!(supported_mime(mime), Some(mime), "{mime} should be allowed");
            assert!(!is_image_mime(mime), "{mime} must not require image magic");
        }
        // image/jpg canonicalizes to image/jpeg on both sides.
        assert_eq!(supported_mime("image/jpg"), Some("image/jpeg"));
        assert!(is_image_mime("image/jpeg"));
    }

    #[test]
    fn active_content_mime_types_are_rejected() {
        for mime in [
            "text/html",
            "application/xhtml+xml",
            "image/svg+xml",
            "text/javascript",
            "application/javascript",
            "application/xml",
            "text/xml",
            "application/x-sh",
            "application/x-msdownload",
            "application/x-executable",
        ] {
            assert_eq!(supported_mime(mime), None, "{mime} must be rejected");
        }
    }

    #[test]
    fn image_magic_is_required_only_for_images() {
        // A non-image allowlisted MIME with arbitrary bytes is accepted (no magic).
        assert!(!is_image_mime("application/octet-stream"));
        assert!(!is_image_mime("text/plain"));
        // An image MIME with mismatched bytes is rejected.
        assert!(is_image_mime("image/png"));
        assert!(!magic_matches("image/png", b"not a png"));
    }

    #[test]
    fn special_ip_ranges_are_rejected() {
        assert!(!ipv4_public(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(!ipv4_public(Ipv4Addr::new(198, 51, 100, 10)));
        assert!(ipv4_public(Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!ipv6_public("2001:db8::1".parse().unwrap()));
        assert!(ipv6_public("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn same_origin_redirect_contract_is_strict() {
        let a = Url::parse("https://example.com/artifacts/a").unwrap();
        let b = Url::parse("https://example.com/next").unwrap();
        let c = Url::parse("https://other.example/next").unwrap();
        assert_eq!(origin_tuple(&a).unwrap(), origin_tuple(&b).unwrap());
        assert_ne!(origin_tuple(&a).unwrap(), origin_tuple(&c).unwrap());
    }

    #[test]
    fn redacted_url_drops_query_capabilities() {
        let url = Url::parse("https://example.com/a?secret=x#frag").unwrap();
        assert_eq!(redacted_url(&url), "https://example.com/a");
    }
}
