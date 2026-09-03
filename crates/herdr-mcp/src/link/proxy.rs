//! Outbound proxy discovery and tunneling (HTTP CONNECT and SOCKS5) for Link WSS.
//!
//! Link reads proxy settings from explicit env vars and, on macOS, system proxy
//! configuration. When no proxy is configured, callers connect directly. A
//! macOS PAC configuration is detected but deliberately never evaluated: Link
//! does not fetch or execute PAC scripts and falls back to a direct connection
//! when only a PAC is configured.

use std::io;
#[cfg(target_os = "macos")]
use std::process::Command;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use url::Url;

pub const ENV_HERDR_LINK_PROXY: &str = "HERDR_LINK_PROXY";
const ENV_HTTPS_PROXY: &str = "HTTPS_PROXY";
const ENV_HTTPS_PROXY_LOWER: &str = "https_proxy";
const ENV_HTTP_PROXY: &str = "HTTP_PROXY";
const ENV_HTTP_PROXY_LOWER: &str = "http_proxy";
const ENV_ALL_PROXY: &str = "ALL_PROXY";
const ENV_ALL_PROXY_LOWER: &str = "all_proxy";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_RESPONSE_BUDGET: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxySource {
    HerdrLinkProxy,
    HttpsProxy,
    HttpProxy,
    AllProxy,
    MacosSystemHttps,
    MacosSystemHttp,
    MacosSystemSocks,
}

impl ProxySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HerdrLinkProxy => "HERDR_LINK_PROXY",
            Self::HttpsProxy => "HTTPS_PROXY",
            Self::HttpProxy => "HTTP_PROXY",
            Self::AllProxy => "ALL_PROXY",
            Self::MacosSystemHttps => "macos-system-https",
            Self::MacosSystemHttp => "macos-system-http",
            Self::MacosSystemSocks => "macos-system-socks",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProxy {
    pub url: String,
    pub source: ProxySource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyResolveError {
    InvalidUrl,
    UnsupportedScheme,
    EmptyHost,
}

#[derive(Debug)]
pub enum ProxyTunnelError {
    InvalidProxyUrl,
    UnsupportedProxyScheme,
    InvalidTargetHost,
    ConnectFailed(io::Error),
    ConnectRequestFailed(io::Error),
    ConnectResponseFailed(io::Error),
    ConnectRejected { status: u16, detail: String },
    ConnectResponseTooLarge,
    ConnectResponseMalformed,
    Socks5AuthNotSupported,
    Socks5Rejected { code: u8 },
}

impl std::fmt::Display for ProxyTunnelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidProxyUrl => write!(f, "invalid proxy URL"),
            Self::UnsupportedProxyScheme => write!(f, "unsupported proxy scheme"),
            Self::InvalidTargetHost => write!(f, "invalid tunnel target host"),
            Self::ConnectFailed(error) => write!(f, "proxy connect failed: {error}"),
            Self::ConnectRequestFailed(error) => write!(f, "proxy CONNECT write failed: {error}"),
            Self::ConnectResponseFailed(error) => write!(f, "proxy CONNECT read failed: {error}"),
            Self::ConnectRejected { status, detail } => {
                write!(f, "proxy CONNECT rejected http={status} detail={detail}")
            }
            Self::ConnectResponseTooLarge => write!(f, "proxy CONNECT response too large"),
            Self::ConnectResponseMalformed => write!(f, "proxy CONNECT response malformed"),
            Self::Socks5AuthNotSupported => {
                write!(
                    f,
                    "socks5 proxy requires authentication, which is unsupported"
                )
            }
            Self::Socks5Rejected { code } => {
                write!(
                    f,
                    "socks5 proxy rejected CONNECT code={code} {}",
                    socks5_rep_reason(*code)
                )
            }
        }
    }
}

impl std::error::Error for ProxyTunnelError {}

/// Resolution outcome for Link outbound connections.
///
/// `PacDetectedNotEvaluated` reports a macOS PAC configuration that was seen
/// but deliberately not evaluated: Link never fetches or executes PAC scripts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkProxyResolution {
    Direct,
    Proxy(ResolvedProxy),
    PacDetectedNotEvaluated { url: String },
}

/// Resolve the effective proxy for outbound `wss://` Link connections.
///
/// Precedence:
/// 1. `HERDR_LINK_PROXY`
/// 2. `https_proxy` / `HTTPS_PROXY`
/// 3. `http_proxy` / `HTTP_PROXY`
/// 4. `all_proxy` / `ALL_PROXY` (HTTP/HTTPS/SOCKS5 schemes)
/// 5. macOS system proxy via `scutil --proxy` (HTTPS preferred, then HTTP,
///    then SOCKS)
///
/// When only a macOS PAC is configured, this returns
/// [`LinkProxyResolution::PacDetectedNotEvaluated`] and callers connect
/// directly; a PAC engine is intentionally not implemented.
pub fn resolve_link_proxy_detailed() -> LinkProxyResolution {
    match resolve_link_proxy_from_values(proxy_env_candidates()) {
        Some(proxy) => LinkProxyResolution::Proxy(proxy),
        None => {
            #[cfg(target_os = "macos")]
            if let Some(url) = macos_pac_detected() {
                return LinkProxyResolution::PacDetectedNotEvaluated { url };
            }
            LinkProxyResolution::Direct
        }
    }
}

/// Backward-compatible view: `Some` only when an explicit proxy resolved.
pub fn resolve_link_proxy() -> Option<ResolvedProxy> {
    match resolve_link_proxy_detailed() {
        LinkProxyResolution::Proxy(proxy) => Some(proxy),
        LinkProxyResolution::Direct | LinkProxyResolution::PacDetectedNotEvaluated { .. } => None,
    }
}

fn proxy_env_candidates() -> Vec<(ProxySource, String)> {
    [
        (ProxySource::HerdrLinkProxy, ENV_HERDR_LINK_PROXY),
        (ProxySource::HttpsProxy, ENV_HTTPS_PROXY),
        (ProxySource::HttpsProxy, ENV_HTTPS_PROXY_LOWER),
        (ProxySource::HttpProxy, ENV_HTTP_PROXY),
        (ProxySource::HttpProxy, ENV_HTTP_PROXY_LOWER),
        (ProxySource::AllProxy, ENV_ALL_PROXY),
        (ProxySource::AllProxy, ENV_ALL_PROXY_LOWER),
    ]
    .into_iter()
    .filter_map(|(source, key)| env_trimmed(key).map(|value| (source, value)))
    .collect()
}

fn resolve_link_proxy_from_values(
    candidates: impl IntoIterator<Item = (ProxySource, String)>,
) -> Option<ResolvedProxy> {
    for (source, value) in candidates {
        if let Ok(proxy) = normalize_proxy_url(&value, source) {
            return Some(proxy);
        }
    }
    macos_system_proxy()
}

pub fn normalize_proxy_url(
    raw: &str,
    source: ProxySource,
) -> Result<ResolvedProxy, ProxyResolveError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ProxyResolveError::EmptyHost);
    }

    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("http://{trimmed}")
    };

    let parsed = Url::parse(&with_scheme).map_err(|_| ProxyResolveError::InvalidUrl)?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" && scheme != "socks5" && scheme != "socks5h" {
        return Err(ProxyResolveError::UnsupportedScheme);
    }
    if parsed.host_str().filter(|host| !host.is_empty()).is_none() {
        return Err(ProxyResolveError::EmptyHost);
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err(ProxyResolveError::InvalidUrl);
    }

    Ok(ResolvedProxy {
        url: parsed.to_string(),
        source,
    })
}

fn env_trimmed(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(target_os = "macos")]
fn macos_system_proxy() -> Option<ResolvedProxy> {
    let output = Command::new("scutil").arg("--proxy").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    if let Some(proxy) = macos_proxy_from_scutil(&text, true) {
        return Some(proxy);
    }
    if let Some(proxy) = macos_proxy_from_scutil(&text, false) {
        return Some(proxy);
    }
    macos_socks_from_scutil(&text)
}

#[cfg(not(target_os = "macos"))]
fn macos_system_proxy() -> Option<ResolvedProxy> {
    None
}

/// Parse the explicit SOCKS proxy entry from a `scutil --proxy` dictionary.
///
/// The resolved URL uses `socks5h` semantics: hostname resolution happens at
/// the proxy (remote DNS), which avoids local DNS pollution for workers.dev.
#[cfg(target_os = "macos")]
fn macos_socks_from_scutil(text: &str) -> Option<ResolvedProxy> {
    if !scutil_flag_enabled(text, "SOCKSEnable") {
        return None;
    }
    let host = scutil_string_value(text, "SOCKSProxy")?;
    let port = scutil_number_value(text, "SOCKSPort").unwrap_or(1080);
    Some(ResolvedProxy {
        url: format!("socks5h://{host}:{port}/"),
        source: ProxySource::MacosSystemSocks,
    })
}

/// Detect a macOS PAC configuration without evaluating it.
///
/// The PAC URL is returned only as a diagnostic; Link never fetches or
/// executes PAC scripts, so PAC-only configurations fall back to a direct
/// connection.
#[cfg(target_os = "macos")]
pub fn macos_pac_detected() -> Option<String> {
    let output = Command::new("scutil").arg("--proxy").output().ok()?;
    if !output.status.success() {
        return None;
    }
    macos_pac_from_scutil(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "macos")]
fn macos_pac_from_scutil(text: &str) -> Option<String> {
    if !scutil_flag_enabled(text, "ProxyAutoConfigEnable") {
        return None;
    }
    scutil_string_value(text, "ProxyAutoConfigURLString")
}

#[cfg(target_os = "macos")]
fn macos_proxy_from_scutil(text: &str, prefer_https: bool) -> Option<ResolvedProxy> {
    let enabled_key = if prefer_https {
        "HTTPSEnable"
    } else {
        "HTTPEnable"
    };
    let host_key = if prefer_https {
        "HTTPSProxy"
    } else {
        "HTTPProxy"
    };
    let port_key = if prefer_https {
        "HTTPSPort"
    } else {
        "HTTPPort"
    };

    if !scutil_flag_enabled(text, enabled_key) {
        return None;
    }
    let host = scutil_string_value(text, host_key)?;
    let port = scutil_number_value(text, port_key).unwrap_or(8080);
    let source = if prefer_https {
        ProxySource::MacosSystemHttps
    } else {
        ProxySource::MacosSystemHttp
    };
    normalize_proxy_url(&format!("http://{host}:{port}"), source).ok()
}

#[cfg(target_os = "macos")]
fn scutil_flag_enabled(text: &str, key: &str) -> bool {
    scutil_number_value(text, key).is_some_and(|value| value != 0)
}

#[cfg(target_os = "macos")]
fn scutil_string_value(text: &str, key: &str) -> Option<String> {
    let needle = format!("{key} : ");
    let start = text.find(&needle)? + needle.len();
    let rest = text[start..].trim_start();
    let end = rest.find('\n').unwrap_or(rest.len());
    let value = rest[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

#[cfg(target_os = "macos")]
fn scutil_number_value(text: &str, key: &str) -> Option<u16> {
    let value = scutil_string_value(text, key)?;
    value.parse::<u16>().ok()
}

/// Dial `target_host:target_port` through the resolved proxy.
///
/// `socks5`/`socks5h` proxies use the SOCKS5 CONNECT handshake with
/// remote-DNS semantics (the hostname is sent to the proxy unresolved, so
/// `workers.dev` is resolved by the proxy, not by a possibly polluted local
/// resolver). `http`/`https` proxies use the existing HTTP CONNECT path.
pub async fn connect_via_proxy(
    proxy_url: &str,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream, ProxyTunnelError> {
    let proxy = Url::parse(proxy_url).map_err(|_| ProxyTunnelError::InvalidProxyUrl)?;
    match proxy.scheme() {
        "socks5" | "socks5h" => connect_via_socks5_proxy(&proxy, target_host, target_port).await,
        "http" | "https" => connect_via_http_proxy(proxy_url, target_host, target_port).await,
        _ => Err(ProxyTunnelError::UnsupportedProxyScheme),
    }
}

async fn connect_proxy_tcp(
    proxy_host: &str,
    proxy_port: u16,
) -> Result<TcpStream, ProxyTunnelError> {
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((proxy_host, proxy_port)),
    )
    .await
    .map_err(|error| {
        ProxyTunnelError::ConnectFailed(io::Error::new(io::ErrorKind::TimedOut, error))
    })?
    .map_err(ProxyTunnelError::ConnectFailed)
}

pub async fn connect_via_http_proxy(
    proxy_url: &str,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream, ProxyTunnelError> {
    let proxy = Url::parse(proxy_url).map_err(|_| ProxyTunnelError::InvalidProxyUrl)?;
    let scheme = proxy.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(ProxyTunnelError::UnsupportedProxyScheme);
    }
    let proxy_host = proxy
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or(ProxyTunnelError::InvalidProxyUrl)?;
    let proxy_port = proxy
        .port()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });

    let mut stream = connect_proxy_tcp(proxy_host, proxy_port).await?;

    let request = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\nProxy-Connection: Keep-Alive\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(ProxyTunnelError::ConnectRequestFailed)?;
    stream
        .flush()
        .await
        .map_err(ProxyTunnelError::ConnectRequestFailed)?;

    let mut buffer = vec![0_u8; CONNECT_RESPONSE_BUDGET];
    let mut total = 0_usize;
    loop {
        if total >= buffer.len() {
            return Err(ProxyTunnelError::ConnectResponseTooLarge);
        }
        let read = stream
            .read(&mut buffer[total..])
            .await
            .map_err(ProxyTunnelError::ConnectResponseFailed)?;
        if read == 0 {
            break;
        }
        total += read;
        if buffer[..total]
            .windows(4)
            .any(|window| window == b"\r\n\r\n")
        {
            break;
        }
    }

    let response = String::from_utf8_lossy(&buffer[..total]);
    let status = parse_http_status(&response).ok_or(ProxyTunnelError::ConnectResponseMalformed)?;
    if status != 200 {
        return Err(ProxyTunnelError::ConnectRejected {
            status,
            detail: compact_proxy_detail(&response),
        });
    }
    Ok(stream)
}

const SOCKS5_VERSION: u8 = 0x05;
const SOCKS5_CMD_CONNECT: u8 = 0x01;
const SOCKS5_AUTH_NONE: u8 = 0x00;
const SOCKS5_ATYP_IPV4: u8 = 0x01;
const SOCKS5_ATYP_DOMAIN: u8 = 0x03;
const SOCKS5_ATYP_IPV6: u8 = 0x04;

/// Connect through a SOCKS5 proxy with remote-DNS semantics.
///
/// The target hostname is always sent to the proxy unresolved (ATYP=domain
/// for names, ATYP=1/4 only for IP literals), which matches `socks5h`
/// behavior regardless of whether the configured scheme was `socks5` or
/// `socks5h`. This keeps `workers.dev` resolution at the proxy and away from
/// a locally polluted resolver.
///
/// Only the NO-AUTH method is offered; username/password authentication is
/// deliberately unsupported so proxy credentials can never be sent, stored,
/// or logged.
pub async fn connect_via_socks5_proxy(
    proxy: &Url,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream, ProxyTunnelError> {
    let proxy_host = proxy
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or(ProxyTunnelError::InvalidProxyUrl)?;
    let proxy_port = proxy.port().unwrap_or(1080);

    let mut stream = connect_proxy_tcp(proxy_host, proxy_port).await?;

    // Greeting: offer exactly one method, NO-AUTH.
    stream
        .write_all(&[SOCKS5_VERSION, 0x01, SOCKS5_AUTH_NONE])
        .await
        .map_err(ProxyTunnelError::ConnectRequestFailed)?;
    stream
        .flush()
        .await
        .map_err(ProxyTunnelError::ConnectRequestFailed)?;

    let mut method = [0_u8; 2];
    stream
        .read_exact(&mut method)
        .await
        .map_err(ProxyTunnelError::ConnectResponseFailed)?;
    if method[0] != SOCKS5_VERSION {
        return Err(ProxyTunnelError::ConnectResponseMalformed);
    }
    if method[1] != SOCKS5_AUTH_NONE {
        return Err(ProxyTunnelError::Socks5AuthNotSupported);
    }

    let request = socks5_connect_request(target_host, target_port)?;
    stream
        .write_all(&request)
        .await
        .map_err(ProxyTunnelError::ConnectRequestFailed)?;
    stream
        .flush()
        .await
        .map_err(ProxyTunnelError::ConnectRequestFailed)?;

    socks5_read_connect_reply(&mut stream).await?;
    Ok(stream)
}

fn socks5_connect_request(
    target_host: &str,
    target_port: u16,
) -> Result<Vec<u8>, ProxyTunnelError> {
    let mut request = Vec::with_capacity(7 + target_host.len());
    request.push(SOCKS5_VERSION);
    request.push(SOCKS5_CMD_CONNECT);
    request.push(0x00);
    if let Ok(ip) = target_host.parse::<std::net::Ipv4Addr>() {
        request.push(SOCKS5_ATYP_IPV4);
        request.extend_from_slice(&ip.octets());
    } else if let Ok(ip) = target_host.parse::<std::net::Ipv6Addr>() {
        request.push(SOCKS5_ATYP_IPV6);
        request.extend_from_slice(&ip.octets());
    } else {
        let bytes = target_host.as_bytes();
        if bytes.is_empty() || bytes.len() > 255 {
            return Err(ProxyTunnelError::InvalidTargetHost);
        }
        request.push(SOCKS5_ATYP_DOMAIN);
        request.push(bytes.len() as u8);
        request.extend_from_slice(bytes);
    }
    request.extend_from_slice(&target_port.to_be_bytes());
    Ok(request)
}

async fn socks5_read_connect_reply(stream: &mut TcpStream) -> Result<(), ProxyTunnelError> {
    let mut header = [0_u8; 4];
    stream
        .read_exact(&mut header)
        .await
        .map_err(ProxyTunnelError::ConnectResponseFailed)?;
    if header[0] != SOCKS5_VERSION {
        return Err(ProxyTunnelError::ConnectResponseMalformed);
    }
    if header[1] != 0x00 {
        return Err(ProxyTunnelError::Socks5Rejected { code: header[1] });
    }
    // Consume the bound address and port so the stream is clean for TLS.
    match header[3] {
        SOCKS5_ATYP_IPV4 => {
            let mut rest = [0_u8; 4 + 2];
            stream
                .read_exact(&mut rest)
                .await
                .map_err(ProxyTunnelError::ConnectResponseFailed)?;
        }
        SOCKS5_ATYP_DOMAIN => {
            let mut len = [0_u8; 1];
            stream
                .read_exact(&mut len)
                .await
                .map_err(ProxyTunnelError::ConnectResponseFailed)?;
            let mut rest = vec![0_u8; usize::from(len[0]) + 2];
            stream
                .read_exact(&mut rest)
                .await
                .map_err(ProxyTunnelError::ConnectResponseFailed)?;
        }
        SOCKS5_ATYP_IPV6 => {
            let mut rest = [0_u8; 16 + 2];
            stream
                .read_exact(&mut rest)
                .await
                .map_err(ProxyTunnelError::ConnectResponseFailed)?;
        }
        _ => return Err(ProxyTunnelError::ConnectResponseMalformed),
    }
    Ok(())
}

fn socks5_rep_reason(code: u8) -> &'static str {
    match code {
        0x01 => "(general socks server failure)",
        0x02 => "(connection not allowed by ruleset)",
        0x03 => "(network unreachable)",
        0x04 => "(host unreachable)",
        0x05 => "(connection refused)",
        0x06 => "(TTL expired)",
        0x07 => "(command not supported)",
        0x08 => "(address type not supported)",
        _ => "(unknown reply code)",
    }
}

fn parse_http_status(response: &str) -> Option<u16> {
    let line = response.lines().next()?;
    let mut parts = line.split_whitespace();
    let scheme = parts.next()?;
    if scheme != "HTTP/1.0" && scheme != "HTTP/1.1" {
        return None;
    }
    parts.next()?.parse().ok()
}

fn compact_proxy_detail(response: &str) -> String {
    response
        .lines()
        .next()
        .unwrap_or("connect-failed")
        .chars()
        .map(|ch| if ch.is_whitespace() { '-' } else { ch })
        .take(120)
        .collect()
}

pub fn wss_target(url: &str) -> Option<(String, u16)> {
    let parsed = Url::parse(url).ok()?;
    if parsed.scheme() != "wss" {
        return None;
    }
    let host = parsed.host_str()?.to_owned();
    let port = parsed.port().unwrap_or(443);
    Some((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_prefers_herdr_link_proxy_socks5_over_standard_env() {
        let resolved = resolve_link_proxy_from_values(vec![
            (
                ProxySource::HerdrLinkProxy,
                "socks5://127.0.0.1:1080".to_owned(),
            ),
            (ProxySource::HttpsProxy, "http://127.0.0.1:7890".to_owned()),
        ])
        .expect("proxy");
        assert_eq!(resolved.source, ProxySource::HerdrLinkProxy);
        assert_eq!(
            resolved.url.trim_end_matches('/'),
            "socks5://127.0.0.1:1080"
        );
    }

    #[test]
    fn https_proxy_wins_over_http_proxy() {
        let resolved = resolve_link_proxy_from_values(vec![
            (ProxySource::HttpsProxy, "http://127.0.0.1:7890".to_owned()),
            (ProxySource::HttpProxy, "http://127.0.0.1:8080".to_owned()),
        ])
        .expect("proxy");
        assert_eq!(resolved.source, ProxySource::HttpsProxy);
        assert_eq!(resolved.url, "http://127.0.0.1:7890/");
    }

    #[test]
    fn normalizes_host_port_without_scheme() {
        let resolved =
            normalize_proxy_url("127.0.0.1:7890", ProxySource::HttpProxy).expect("proxy");
        assert_eq!(resolved.url, "http://127.0.0.1:7890/");
        assert_eq!(resolved.source, ProxySource::HttpProxy);
    }

    #[test]
    fn rejects_unsupported_schemes_and_credential_urls() {
        assert_eq!(
            normalize_proxy_url("socks4://127.0.0.1:1080", ProxySource::AllProxy),
            Err(ProxyResolveError::UnsupportedScheme)
        );
        assert_eq!(
            normalize_proxy_url("ftp://127.0.0.1:21", ProxySource::AllProxy),
            Err(ProxyResolveError::UnsupportedScheme)
        );
        assert_eq!(
            normalize_proxy_url("socks5://user:pass@127.0.0.1:1080", ProxySource::AllProxy),
            Err(ProxyResolveError::InvalidUrl)
        );
        assert_eq!(
            normalize_proxy_url("http://user:pass@127.0.0.1:7890", ProxySource::HttpProxy),
            Err(ProxyResolveError::InvalidUrl)
        );
    }

    #[test]
    fn accepts_socks5_and_socks5h_schemes() {
        let socks5 = normalize_proxy_url("socks5://127.0.0.1:1080", ProxySource::HerdrLinkProxy)
            .expect("socks5 proxy");
        assert_eq!(socks5.url.trim_end_matches('/'), "socks5://127.0.0.1:1080");
        let socks5h = normalize_proxy_url("socks5h://127.0.0.1:1080", ProxySource::AllProxy)
            .expect("socks5h proxy");
        assert_eq!(
            socks5h.url.trim_end_matches('/'),
            "socks5h://127.0.0.1:1080"
        );
        assert_eq!(socks5h.source, ProxySource::AllProxy);
    }

    #[tokio::test]
    async fn dials_local_mock_socks5_with_remote_dns() {
        for scheme in ["socks5", "socks5h"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut greeting = [0_u8; 3];
                sock.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [0x05, 0x01, 0x00]);
                sock.write_all(&[0x05, 0x00]).await.unwrap();

                let mut header = [0_u8; 4];
                sock.read_exact(&mut header).await.unwrap();
                assert_eq!(header[0], 0x05);
                assert_eq!(header[1], 0x01);
                // ATYP=domain: the hostname is sent unresolved (remote DNS).
                assert_eq!(header[3], 0x03);
                let mut len = [0_u8; 1];
                sock.read_exact(&mut len).await.unwrap();
                let mut host = vec![0_u8; usize::from(len[0])];
                sock.read_exact(&mut host).await.unwrap();
                let mut port = [0_u8; 2];
                sock.read_exact(&mut port).await.unwrap();

                sock.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                    .await
                    .unwrap();

                // Prove the tunnel carries bytes after the handshake.
                let mut echoed = [0_u8; 1];
                sock.read_exact(&mut echoed).await.unwrap();
                sock.write_all(&echoed).await.unwrap();
                (host, u16::from_be_bytes(port))
            });

            let proxy_url = format!("{scheme}://{addr}");
            // A name that cannot resolve locally: dialing must not need local
            // DNS because the hostname is forwarded to the proxy.
            let mut stream = connect_via_proxy(&proxy_url, "edge.example.workers.dev", 443)
                .await
                .expect("socks5 tunnel");
            // The tunnel is bidirectional: bytes flow after the handshake.
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            stream.write_all(b"t").await.unwrap();
            let (host, port) = server.await.unwrap();
            assert_eq!(host, b"edge.example.workers.dev");
            assert_eq!(port, 443);
            let mut echoed = [0_u8; 1];
            stream.read_exact(&mut echoed).await.unwrap();
            assert_eq!(&echoed, b"t");
        }
    }

    #[tokio::test]
    async fn dispatcher_keeps_http_connect_dial_working() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 256];
            loop {
                let read = sock.read(&mut chunk).await.unwrap();
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&buffer).to_string();
            sock.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            let mut echoed = [0_u8; 1];
            sock.read_exact(&mut echoed).await.unwrap();
            sock.write_all(&echoed).await.unwrap();
            request
        });

        let proxy_url = format!("http://{addr}/");
        let mut stream = connect_via_proxy(&proxy_url, "edge.example.workers.dev", 443)
            .await
            .expect("http CONNECT tunnel");
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        stream.write_all(b"h").await.unwrap();
        let request = server.await.unwrap();
        assert!(request.starts_with("CONNECT edge.example.workers.dev:443 HTTP/1.1\r\n"));
        let mut echoed = [0_u8; 1];
        stream.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"h");
    }

    #[test]
    fn parses_connect_response_status() {
        assert_eq!(
            parse_http_status("HTTP/1.1 200 Connection established\r\n\r\n"),
            Some(200)
        );
        assert_eq!(
            parse_http_status("HTTP/1.1 403 Forbidden\r\n\r\n"),
            Some(403)
        );
        assert_eq!(parse_http_status("not-http"), None);
    }

    #[test]
    fn extracts_wss_target_host_and_port() {
        assert_eq!(
            wss_target("wss://edge.example.workers.dev/ws/ws1"),
            Some(("edge.example.workers.dev".to_owned(), 443))
        );
        assert_eq!(
            wss_target("wss://edge.example:8443/ws"),
            Some(("edge.example".to_owned(), 8443))
        );
        assert_eq!(wss_target("ws://edge.example/ws"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_scutil_proxy_dictionary() {
        let sample = r#"<dictionary> {
  HTTPEnable : 0
  HTTPPort : 8080
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 1
  HTTPSPort : 7890
  HTTPSProxy : 127.0.0.1
}"#;
        let proxy = macos_proxy_from_scutil(sample, true).expect("https proxy");
        assert_eq!(proxy.source, ProxySource::MacosSystemHttps);
        assert_eq!(proxy.url, "http://127.0.0.1:7890/");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_scutil_socks_entry_as_remote_dns_proxy() {
        let sample = r#"<dictionary> {
  HTTPSEnable : 0
  SOCKSEnable : 1
  SOCKSPort : 1080
  SOCKSProxy : 127.0.0.1
}"#;
        let proxy = macos_socks_from_scutil(sample).expect("socks proxy");
        assert_eq!(proxy.source, ProxySource::MacosSystemSocks);
        assert_eq!(proxy.url, "socks5h://127.0.0.1:1080/");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ignores_disabled_or_incomplete_scutil_socks_entry() {
        assert_eq!(macos_socks_from_scutil("SOCKSEnable : 0"), None);
        assert_eq!(
            macos_socks_from_scutil("SOCKSEnable : 1\nSOCKSPort : 1080"),
            None
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn detects_pac_configuration_without_evaluating_it() {
        let sample = r#"<dictionary> {
  ProxyAutoConfigEnable : 1
  ProxyAutoConfigURLString : http://proxy.example/pac.js
}"#;
        assert_eq!(
            macos_pac_from_scutil(sample),
            Some("http://proxy.example/pac.js".to_owned())
        );
        assert_eq!(
            macos_pac_from_scutil(
                "ProxyAutoConfigEnable : 0\nProxyAutoConfigURLString : http://proxy.example/pac.js"
            ),
            None
        );
    }
}
