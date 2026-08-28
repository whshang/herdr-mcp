//! Outbound proxy discovery and HTTP CONNECT tunneling for Link WSS.
//!
//! Link reads proxy settings from explicit env vars and, on macOS, system proxy
//! configuration. When no proxy is configured, callers connect directly.

use std::io;
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::process::Command;

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
    ConnectFailed(io::Error),
    ConnectRequestFailed(io::Error),
    ConnectResponseFailed(io::Error),
    ConnectRejected { status: u16, detail: String },
    ConnectResponseTooLarge,
    ConnectResponseMalformed,
}

impl std::fmt::Display for ProxyTunnelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidProxyUrl => write!(f, "invalid proxy URL"),
            Self::UnsupportedProxyScheme => write!(f, "unsupported proxy scheme"),
            Self::ConnectFailed(error) => write!(f, "proxy connect failed: {error}"),
            Self::ConnectRequestFailed(error) => write!(f, "proxy CONNECT write failed: {error}"),
            Self::ConnectResponseFailed(error) => write!(f, "proxy CONNECT read failed: {error}"),
            Self::ConnectRejected { status, detail } => {
                write!(f, "proxy CONNECT rejected http={status} detail={detail}")
            }
            Self::ConnectResponseTooLarge => write!(f, "proxy CONNECT response too large"),
            Self::ConnectResponseMalformed => write!(f, "proxy CONNECT response malformed"),
        }
    }
}

impl std::error::Error for ProxyTunnelError {}

/// Resolve the effective HTTP proxy for outbound `wss://` Link connections.
///
/// Precedence:
/// 1. `HERDR_LINK_PROXY`
/// 2. `https_proxy` / `HTTPS_PROXY`
/// 3. `http_proxy` / `HTTP_PROXY`
/// 4. `all_proxy` / `ALL_PROXY` (HTTP/HTTPS schemes only)
/// 5. macOS system proxy via `scutil --proxy` (HTTPS preferred, then HTTP)
pub fn resolve_link_proxy() -> Option<ResolvedProxy> {
    resolve_link_proxy_from_values(proxy_env_candidates())
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
    if scheme != "http" && scheme != "https" {
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
    macos_proxy_from_scutil(&text, false)
}

#[cfg(not(target_os = "macos"))]
fn macos_system_proxy() -> Option<ResolvedProxy> {
    None
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

    let mut stream = tokio::time::timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((proxy_host, proxy_port)),
    )
    .await
    .map_err(|error| {
        ProxyTunnelError::ConnectFailed(io::Error::new(io::ErrorKind::TimedOut, error))
    })?
    .map_err(ProxyTunnelError::ConnectFailed)?;

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
    fn precedence_prefers_herdr_link_proxy_over_standard_env() {
        let resolved = resolve_link_proxy_from_values(vec![
            (
                ProxySource::HerdrLinkProxy,
                "http://127.0.0.1:7891".to_owned(),
            ),
            (ProxySource::HttpsProxy, "http://127.0.0.1:7890".to_owned()),
        ])
        .expect("proxy");
        assert_eq!(resolved.source, ProxySource::HerdrLinkProxy);
        assert_eq!(resolved.url, "http://127.0.0.1:7891/");
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
            normalize_proxy_url("socks5://127.0.0.1:7890", ProxySource::AllProxy),
            Err(ProxyResolveError::UnsupportedScheme)
        );
        assert_eq!(
            normalize_proxy_url("http://user:pass@127.0.0.1:7890", ProxySource::HttpProxy),
            Err(ProxyResolveError::InvalidUrl)
        );
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
}
