//! Provider-neutral Link transport ladder and route selection.
//!
//! Implements the Link transport policy specified in `docs/_wip/v0.4.5-release-plan.md`:
//! 1. direct Custom Domain when configured/healthy
//! 2. direct workers.dev
//! 3. validated explicit/system local proxy (existing HTTP/SOCKS implementation)
//! 4. sticky shared Relay endpoints
//! 5. next healthy relay after bounded transport failures
//!
//! Critical invariants:
//! - Transport changes never replay an in-flight mutation.
//! - Ordinary socket drops use existing reconnect backoff before failover.
//! - A successful route is sticky for the healthy connection.
//! - Status/doctor exposes non-secret selected transport/proxy/relay/failover evidence.
//! - Invalid or non-workers.dev relay candidates fail closed and cannot become a generic proxy.
//! - No hardcoded operator/private upstreams or default relays before qualification.

use url::Url;

use super::proxy::{LinkProxyResolution, ResolvedProxy, resolve_link_proxy_detailed};
use super::socket_driver::build_edge_url;

pub const WORKERS_DEV_SUFFIX: &str = ".workers.dev";
pub const DEFAULT_MAX_FAILURES_PER_ROUTE: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LadderError {
    InvalidUrl(String),
    InsecureScheme(String),
    InvalidOrigin(String),
    InvalidUpstreamHost(String),
    InvalidWorkstationId(String),
    NoAvailableRoutes,
}

impl std::fmt::Display for LadderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUrl(url) => write!(f, "invalid URL: {url}"),
            Self::InsecureScheme(url) => {
                write!(f, "insecure scheme (must use wss:// or https://): {url}")
            }
            Self::InvalidOrigin(origin) => write!(f, "invalid origin: {origin}"),
            Self::InvalidUpstreamHost(host) => write!(f, "invalid relay upstream host: {host}"),
            Self::InvalidWorkstationId(id) => write!(f, "invalid workstation id: {id}"),
            Self::NoAvailableRoutes => {
                write!(f, "no available transport routes could be constructed")
            }
        }
    }
}

impl std::error::Error for LadderError {}

/// Validate that an upstream host is a valid `*.workers.dev` domain with no
/// port, userinfo, IP literal, or illegal characters.
pub fn validate_workers_dev_host(raw: &str) -> Result<String, LadderError> {
    let host = raw.trim().to_ascii_lowercase();
    if host.is_empty()
        || !host.ends_with(WORKERS_DEV_SUFFIX)
        || host == "workers.dev"
        || host.contains(':')
        || host.contains('@')
        || host.contains('/')
        || host.contains('?')
        || host.contains('#')
    {
        return Err(LadderError::InvalidUpstreamHost(raw.to_owned()));
    }
    let prefix = &host[..host.len() - WORKERS_DEV_SUFFIX.len()];
    if prefix.is_empty() {
        return Err(LadderError::InvalidUpstreamHost(raw.to_owned()));
    }
    for label in prefix.split('.') {
        if label.is_empty()
            || label.starts_with('-')
            || label.ends_with('-')
            || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(LadderError::InvalidUpstreamHost(raw.to_owned()));
        }
    }
    Ok(host)
}

/// Validate workstation id according to the Relay v1 grammar.
pub fn validate_workstation_id(raw: &str) -> Result<&str, LadderError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(LadderError::InvalidWorkstationId(raw.to_owned()));
    }
    let first = trimmed.chars().next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err(LadderError::InvalidWorkstationId(raw.to_owned()));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == ':' || c == '-')
    {
        return Err(LadderError::InvalidWorkstationId(raw.to_owned()));
    }
    Ok(trimmed)
}

/// Build a canonical Relay v1 WebSocket URL:
/// `wss://<relay-host>/v1/<upstream-workers-dev-host>/ws/<workstation-id>`
///
/// Fails closed if the upstream host is not a valid `*.workers.dev` domain.
pub fn build_relay_edge_url(
    relay_base_url: &str,
    upstream_workers_dev_host: &str,
    workstation_id: &str,
) -> Result<String, LadderError> {
    let valid_host = validate_workers_dev_host(upstream_workers_dev_host)?;
    let valid_id = validate_workstation_id(workstation_id)?;

    let mut url = Url::parse(relay_base_url)
        .map_err(|_| LadderError::InvalidUrl(relay_base_url.to_owned()))?;
    match url.scheme() {
        "wss" => {}
        "https" => {
            url.set_scheme("wss")
                .map_err(|_| LadderError::InvalidUrl(relay_base_url.to_owned()))?;
        }
        _ => return Err(LadderError::InsecureScheme(relay_base_url.to_owned())),
    }

    let base_path = url.path().trim_end_matches('/');
    let path_prefix = if base_path.ends_with("/v1") {
        base_path.to_owned()
    } else if base_path.is_empty() || base_path == "/" {
        "/v1".to_owned()
    } else {
        format!("{base_path}/v1")
    };

    url.set_path(&format!("{path_prefix}/{valid_host}/ws/{valid_id}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RelayEndpoint {
    pub id: String,
    pub url: String,
    pub priority: u32,
    pub failure_domain: String,
    pub enabled: bool,
}

/// Default embedded relay candidates.
///
/// Kept deliberately empty until exact-host mainland UAT passes per the
/// v0.4.5 release plan. Relay endpoints are injected via configuration/data
/// seam once qualified.
pub fn default_embedded_relays() -> Vec<RelayEndpoint> {
    Vec::new()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportRouteKind {
    DirectCustomDomain,
    DirectWorkersDev,
    LocalProxy,
    SharedRelay,
}

impl TransportRouteKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DirectCustomDomain => "direct-custom-domain",
            Self::DirectWorkersDev => "direct-workers-dev",
            Self::LocalProxy => "local-proxy",
            Self::SharedRelay => "shared-relay",
        }
    }

    pub const fn category_str(self) -> &'static str {
        match self {
            Self::DirectCustomDomain | Self::DirectWorkersDev => "direct",
            Self::LocalProxy => "local-proxy",
            Self::SharedRelay => "shared-relay",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportRoute {
    pub kind: TransportRouteKind,
    pub endpoint_url: String,
    pub proxy: Option<ResolvedProxy>,
    pub relay_id: Option<String>,
}

fn extract_origin_host(raw: &str) -> Result<String, LadderError> {
    let url = Url::parse(raw).map_err(|_| LadderError::InvalidOrigin(raw.to_owned()))?;
    match url.scheme() {
        "http" | "https" | "ws" | "wss" => {}
        _ => return Err(LadderError::InsecureScheme(raw.to_owned())),
    }
    let host = url
        .host_str()
        .ok_or_else(|| LadderError::InvalidOrigin(raw.to_owned()))?
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err(LadderError::InvalidOrigin(raw.to_owned()));
    }
    Ok(host)
}

fn normalize_origin_to_ws_base(raw: &str) -> Result<String, LadderError> {
    let mut url = Url::parse(raw).map_err(|_| LadderError::InvalidOrigin(raw.to_owned()))?;
    let scheme = match url.scheme() {
        "http" | "ws" => "ws",
        "https" | "wss" => "wss",
        _ => return Err(LadderError::InsecureScheme(raw.to_owned())),
    };
    let _ = url.set_scheme(scheme);
    let path = url.path().trim_end_matches('/');
    if path.is_empty() || path == "/" {
        url.set_path("/ws");
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

/// Build ordered transport routes according to the transport ladder policy:
/// 1. Direct Custom Domain when configured
/// 2. Direct workers.dev
/// 3. Validated explicit/system local proxy
/// 4. Shared Relay endpoints (sorted by priority descending)
///
/// Fails explicitly if given malformed origins or if no valid route can be formed.
/// Never manufactures or synthesizes a hardcoded upstream.
pub fn build_ladder_routes(
    edge_url: &str,
    public_origin: Option<&str>,
    link_upstream_origin: Option<&str>,
    workstation_id: &str,
    proxy: Option<ResolvedProxy>,
    relay_pool: &[RelayEndpoint],
) -> Result<Vec<TransportRoute>, LadderError> {
    validate_workstation_id(workstation_id)?;

    // Validate and classify public origin if provided
    let mut custom_domain = None;
    let mut workers_dev = None;

    if let Some(origin) = public_origin {
        let host = extract_origin_host(origin)?;
        if host.ends_with(WORKERS_DEV_SUFFIX) {
            workers_dev = Some(origin);
        } else {
            custom_domain = Some(origin);
        }
    }

    if let Some(origin) = link_upstream_origin {
        let host = extract_origin_host(origin)?;
        if host.ends_with(WORKERS_DEV_SUFFIX) {
            workers_dev = Some(origin);
        } else if custom_domain.is_none() {
            custom_domain = Some(origin);
        }
    }

    // If neither origin override was provided, parse the base edge_url
    if custom_domain.is_none() && workers_dev.is_none() {
        let host = extract_origin_host(edge_url)?;
        if host.ends_with(WORKERS_DEV_SUFFIX) {
            workers_dev = Some(edge_url);
        } else {
            custom_domain = Some(edge_url);
        }
    }

    let mut routes = Vec::new();

    // 1. Direct Custom Domain
    if let Some(cd) = custom_domain {
        let base_url = normalize_origin_to_ws_base(cd)?;
        let ws_url = build_edge_url(&base_url, workstation_id)
            .map_err(|_| LadderError::InvalidUrl(base_url))?;
        routes.push(TransportRoute {
            kind: TransportRouteKind::DirectCustomDomain,
            endpoint_url: ws_url,
            proxy: None,
            relay_id: None,
        });
    }

    // 2. Direct workers.dev
    if let Some(wd) = workers_dev {
        let base_url = normalize_origin_to_ws_base(wd)?;
        let ws_url = build_edge_url(&base_url, workstation_id)
            .map_err(|_| LadderError::InvalidUrl(base_url))?;
        routes.push(TransportRoute {
            kind: TransportRouteKind::DirectWorkersDev,
            endpoint_url: ws_url,
            proxy: None,
            relay_id: None,
        });
    }

    // 3. Validated explicit/system local proxy
    if let Some(resolved_proxy) = proxy {
        let target = custom_domain.or(workers_dev);
        if let Some(target_origin) = target {
            let base_url = normalize_origin_to_ws_base(target_origin)?;
            let ws_url = build_edge_url(&base_url, workstation_id)
                .map_err(|_| LadderError::InvalidUrl(base_url))?;
            routes.push(TransportRoute {
                kind: TransportRouteKind::LocalProxy,
                endpoint_url: ws_url,
                proxy: Some(resolved_proxy),
                relay_id: None,
            });
        }
    }

    // 4. & 5. Sticky shared Relay endpoints & pool failovers
    if let Some(wd) = workers_dev {
        let host = extract_origin_host(wd)?;
        if validate_workers_dev_host(&host).is_ok() {
            let mut sorted_relays = relay_pool.to_vec();
            sorted_relays.sort_by_key(|b| std::cmp::Reverse(b.priority));
            for relay in sorted_relays {
                if relay.enabled
                    && let Ok(relay_url) = build_relay_edge_url(&relay.url, &host, workstation_id)
                {
                    routes.push(TransportRoute {
                        kind: TransportRouteKind::SharedRelay,
                        endpoint_url: relay_url,
                        proxy: None,
                        relay_id: Some(relay.id.clone()),
                    });
                }
            }
        }
    }

    if routes.is_empty() {
        return Err(LadderError::NoAvailableRoutes);
    }

    Ok(routes)
}

/// Transport ladder state machine.
///
/// Encapsulates route selection, sticky connection state, and bounded
/// failover across routes without taking ownership of reconnect backoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportLadder {
    routes: Vec<TransportRoute>,
    current_index: usize,
    consecutive_failures: usize,
    max_failures_per_route: usize,
    active_route: Option<TransportRoute>,
}

impl TransportLadder {
    pub fn new(
        routes: Vec<TransportRoute>,
        max_failures_per_route: usize,
    ) -> Result<Self, LadderError> {
        if routes.is_empty() {
            return Err(LadderError::NoAvailableRoutes);
        }
        Ok(Self {
            routes,
            current_index: 0,
            consecutive_failures: 0,
            max_failures_per_route: max_failures_per_route.max(1),
            active_route: None,
        })
    }

    pub fn from_config(
        edge_url: &str,
        public_origin: Option<&str>,
        link_upstream_origin: Option<&str>,
        workstation_id: &str,
        proxy: Option<ResolvedProxy>,
        relay_pool: &[RelayEndpoint],
        max_failures_per_route: usize,
    ) -> Result<Self, LadderError> {
        let routes = build_ladder_routes(
            edge_url,
            public_origin,
            link_upstream_origin,
            workstation_id,
            proxy,
            relay_pool,
        )?;
        Self::new(routes, max_failures_per_route)
    }

    pub fn routes(&self) -> &[TransportRoute] {
        &self.routes
    }

    pub fn current_route(&self) -> &TransportRoute {
        &self.routes[self.current_index]
    }

    pub fn active_route(&self) -> Option<&TransportRoute> {
        self.active_route.as_ref()
    }

    pub fn current_index(&self) -> usize {
        self.current_index
    }

    pub fn consecutive_failures(&self) -> usize {
        self.consecutive_failures
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn failover_ready(&self) -> bool {
        self.routes.len() > 1
    }

    /// Mark the current route as successfully connected (`Online`).
    /// Resets failure counters and preserves this route as sticky for subsequent
    /// connection attempts.
    pub fn record_success(&mut self) {
        if self.routes.is_empty() {
            return;
        }
        self.consecutive_failures = 0;
        self.active_route = Some(self.routes[self.current_index].clone());
    }

    /// Record a transport failure (connect failed or handshake failed/timeout).
    ///
    /// Increments the route's consecutive failure count. Once failures reach
    /// `max_failures_per_route`, advances `current_index` to the next route in
    /// the ladder and returns `true`. Otherwise returns `false` (stays on the
    /// current route for standard reconnect backoff).
    pub fn record_failure(&mut self) -> bool {
        if self.routes.is_empty() {
            return false;
        }
        self.consecutive_failures += 1;
        if self.consecutive_failures >= self.max_failures_per_route {
            self.current_index = (self.current_index + 1) % self.routes.len();
            self.consecutive_failures = 0;
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransportEvidence {
    pub mcp_origin: String,
    pub link_upstream: String,
    pub link_transport: String,
    pub proxy_source: String,
    pub relay: String,
    pub pool_source: String,
    pub failover_ready: bool,
}

/// Collect non-secret transport ladder evidence for `status` and `doctor`.
pub fn collect_transport_evidence(
    edge_public_origin: Option<&str>,
    link_upstream_origin: Option<&str>,
) -> TransportEvidence {
    let mcp_origin = match edge_public_origin {
        Some(origin) => {
            if let Ok(url) = Url::parse(origin) {
                let host = url.host_str().unwrap_or("");
                if host.ends_with(WORKERS_DEV_SUFFIX) {
                    "workers.dev".to_owned()
                } else if !host.is_empty() {
                    "custom-domain".to_owned()
                } else {
                    "unconfigured".to_owned()
                }
            } else {
                "unconfigured".to_owned()
            }
        }
        None => "unconfigured".to_owned(),
    };

    let link_upstream = match link_upstream_origin {
        Some(origin) => {
            if let Ok(url) = Url::parse(origin) {
                let host = url.host_str().unwrap_or("");
                if host.ends_with(WORKERS_DEV_SUFFIX) {
                    "workers.dev".to_owned()
                } else if !host.is_empty() {
                    host.to_owned()
                } else {
                    "unconfigured".to_owned()
                }
            } else {
                "unconfigured".to_owned()
            }
        }
        None => "unconfigured".to_owned(),
    };

    let proxy_resolution = resolve_link_proxy_detailed();
    let (proxy_source, resolved_proxy) = match proxy_resolution {
        LinkProxyResolution::Proxy(p) => (p.source.as_str().to_owned(), Some(p)),
        _ => ("none".to_owned(), None),
    };

    let relays = default_embedded_relays();
    let edge_candidate = link_upstream_origin
        .or(edge_public_origin)
        .unwrap_or("wss://unconfigured.local/ws");

    let routes = build_ladder_routes(
        edge_candidate,
        edge_public_origin,
        link_upstream_origin,
        "probe-device",
        resolved_proxy,
        &relays,
    )
    .unwrap_or_default();

    let first_route = routes.first();
    let link_transport = first_route
        .map(|r| r.kind.category_str().to_owned())
        .unwrap_or_else(|| "none".to_owned());
    let relay = first_route
        .and_then(|r| r.relay_id.clone())
        .unwrap_or_else(|| "none".to_owned());
    let failover_ready = routes.len() > 1;

    TransportEvidence {
        mcp_origin,
        link_upstream,
        link_transport,
        proxy_source,
        relay,
        pool_source: "embedded".to_owned(),
        failover_ready,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::proxy::ProxySource;

    #[test]
    fn default_embedded_relays_is_empty_before_qualification() {
        let pool = default_embedded_relays();
        assert!(
            pool.is_empty(),
            "default embedded relay pool must be empty before exact-host mainland UAT qualification"
        );
    }

    #[test]
    fn validates_workers_dev_host_strictly() {
        assert_eq!(
            validate_workers_dev_host("my-worker.workers.dev").unwrap(),
            "my-worker.workers.dev"
        );
        assert_eq!(
            validate_workers_dev_host("herdr-edge-prod.whshang.workers.dev").unwrap(),
            "herdr-edge-prod.whshang.workers.dev"
        );

        // Fails closed on invalid upstreams
        assert!(validate_workers_dev_host("workers.dev").is_err());
        assert!(validate_workers_dev_host("evil.com").is_err());
        assert!(validate_workers_dev_host("192.168.1.1").is_err());
        assert!(validate_workers_dev_host("target.workers.dev:8443").is_err());
        assert!(validate_workers_dev_host("target.workers.dev/path").is_err());
        assert!(validate_workers_dev_host("target.workers.dev?query=1").is_err());
        assert!(validate_workers_dev_host("-invalid.workers.dev").is_err());
    }

    #[test]
    fn builds_canonical_relay_url() {
        let built = build_relay_edge_url(
            "wss://relay.test.net/v1",
            "my-team.workers.dev",
            "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
        )
        .unwrap();
        assert_eq!(
            built,
            "wss://relay.test.net/v1/my-team.workers.dev/ws/dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );

        // Handles https base url
        let built_https = build_relay_edge_url(
            "https://relay.test.net",
            "my-team.workers.dev",
            "prod-real-runtime",
        )
        .unwrap();
        assert_eq!(
            built_https,
            "wss://relay.test.net/v1/my-team.workers.dev/ws/prod-real-runtime"
        );
    }

    #[test]
    fn empty_or_invalid_config_fails_closed_without_hardcoded_fallback() {
        // Invalid origin scheme / syntax must fail explicitly rather than manufacturing an upstream
        let err = build_ladder_routes("not-a-valid-url", None, None, "dev_test", None, &[]);
        assert!(matches!(err, Err(LadderError::InvalidOrigin(_))));

        let err_bad_custom = build_ladder_routes(
            "wss://my-worker.workers.dev/ws",
            Some("ftp://bad-scheme.com"),
            None,
            "dev_test",
            None,
            &[],
        );
        assert!(matches!(
            err_bad_custom,
            Err(LadderError::InsecureScheme(_))
        ));

        let ladder_err = TransportLadder::new(vec![], 2);
        assert!(matches!(ladder_err, Err(LadderError::NoAvailableRoutes)));
    }

    #[test]
    fn preserves_existing_edge_url_when_no_origins_configured() {
        let routes = build_ladder_routes(
            "wss://my-custom.domain.com/ws",
            None,
            None,
            "dev_test",
            None,
            &[],
        )
        .unwrap();

        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].kind, TransportRouteKind::DirectCustomDomain);
        assert_eq!(
            routes[0].endpoint_url,
            "wss://my-custom.domain.com/ws/dev_test"
        );
        assert!(routes[0].proxy.is_none());
    }

    #[test]
    fn ladder_orders_direct_custom_domain_then_direct_workers_dev_then_proxy_then_relays() {
        let proxy = Some(ResolvedProxy {
            url: "http://127.0.0.1:7890".to_owned(),
            source: ProxySource::HttpsProxy,
        });
        let relays = vec![
            RelayEndpoint {
                id: "relay-a".to_owned(),
                url: "wss://relay-a.test.net/v1".to_owned(),
                priority: 100,
                failure_domain: "domain-a".to_owned(),
                enabled: true,
            },
            RelayEndpoint {
                id: "relay-b".to_owned(),
                url: "wss://relay-b.test.net/v1".to_owned(),
                priority: 80,
                failure_domain: "domain-b".to_owned(),
                enabled: true,
            },
        ];

        let routes = build_ladder_routes(
            "wss://herdr.example.com/ws",
            Some("https://herdr.example.com"),
            Some("https://my-worker.workers.dev"),
            "dev_test",
            proxy,
            &relays,
        )
        .unwrap();

        assert_eq!(routes.len(), 5);
        assert_eq!(routes[0].kind, TransportRouteKind::DirectCustomDomain);
        assert_eq!(
            routes[0].endpoint_url,
            "wss://herdr.example.com/ws/dev_test"
        );
        assert!(routes[0].proxy.is_none());

        assert_eq!(routes[1].kind, TransportRouteKind::DirectWorkersDev);
        assert_eq!(
            routes[1].endpoint_url,
            "wss://my-worker.workers.dev/ws/dev_test"
        );
        assert!(routes[1].proxy.is_none());

        assert_eq!(routes[2].kind, TransportRouteKind::LocalProxy);
        assert_eq!(
            routes[2].endpoint_url,
            "wss://herdr.example.com/ws/dev_test"
        );
        assert_eq!(
            routes[2].proxy.as_ref().unwrap().url,
            "http://127.0.0.1:7890"
        );

        assert_eq!(routes[3].kind, TransportRouteKind::SharedRelay);
        assert_eq!(routes[3].relay_id.as_deref(), Some("relay-a"));
        assert_eq!(
            routes[3].endpoint_url,
            "wss://relay-a.test.net/v1/my-worker.workers.dev/ws/dev_test"
        );

        assert_eq!(routes[4].kind, TransportRouteKind::SharedRelay);
        assert_eq!(routes[4].relay_id.as_deref(), Some("relay-b"));
        assert_eq!(
            routes[4].endpoint_url,
            "wss://relay-b.test.net/v1/my-worker.workers.dev/ws/dev_test"
        );
    }

    #[test]
    fn ladder_bounded_failover_advances_after_threshold() {
        let routes = vec![
            TransportRoute {
                kind: TransportRouteKind::DirectCustomDomain,
                endpoint_url: "wss://custom.test/ws/w1".to_owned(),
                proxy: None,
                relay_id: None,
            },
            TransportRoute {
                kind: TransportRouteKind::DirectWorkersDev,
                endpoint_url: "wss://worker.workers.dev/ws/w1".to_owned(),
                proxy: None,
                relay_id: None,
            },
            TransportRoute {
                kind: TransportRouteKind::SharedRelay,
                endpoint_url: "wss://relay.test/v1/worker.workers.dev/ws/w1".to_owned(),
                proxy: None,
                relay_id: Some("relay-1".to_owned()),
            },
        ];

        let mut ladder = TransportLadder::new(routes, 2).unwrap();
        assert_eq!(ladder.current_index(), 0);
        assert_eq!(
            ladder.current_route().kind,
            TransportRouteKind::DirectCustomDomain
        );

        // Failure 1 on route 0 -> no failover yet
        assert!(!ladder.record_failure());
        assert_eq!(ladder.current_index(), 0);
        assert_eq!(ladder.consecutive_failures(), 1);

        // Failure 2 on route 0 -> triggers failover to route 1
        assert!(ladder.record_failure());
        assert_eq!(ladder.current_index(), 1);
        assert_eq!(ladder.consecutive_failures(), 0);
        assert_eq!(
            ladder.current_route().kind,
            TransportRouteKind::DirectWorkersDev
        );

        // Failure 1 on route 1 -> no failover
        assert!(!ladder.record_failure());
        assert_eq!(ladder.current_index(), 1);

        // Failure 2 on route 1 -> triggers failover to route 2
        assert!(ladder.record_failure());
        assert_eq!(ladder.current_index(), 2);
        assert_eq!(ladder.current_route().kind, TransportRouteKind::SharedRelay);
    }

    #[test]
    fn ladder_successful_connection_is_sticky() {
        let routes = vec![
            TransportRoute {
                kind: TransportRouteKind::DirectCustomDomain,
                endpoint_url: "wss://custom.test/ws/w1".to_owned(),
                proxy: None,
                relay_id: None,
            },
            TransportRoute {
                kind: TransportRouteKind::DirectWorkersDev,
                endpoint_url: "wss://worker.workers.dev/ws/w1".to_owned(),
                proxy: None,
                relay_id: None,
            },
            TransportRoute {
                kind: TransportRouteKind::SharedRelay,
                endpoint_url: "wss://relay.test/v1/worker.workers.dev/ws/w1".to_owned(),
                proxy: None,
                relay_id: Some("relay-1".to_owned()),
            },
        ];

        let mut ladder = TransportLadder::new(routes, 2).unwrap();

        // Fail over to route 1
        ladder.record_failure();
        ladder.record_failure();
        assert_eq!(ladder.current_index(), 1);

        // Route 1 succeeds!
        ladder.record_success();
        assert_eq!(ladder.current_index(), 1);
        assert_eq!(ladder.consecutive_failures(), 0);
        assert_eq!(
            ladder.active_route().map(|r| r.kind),
            Some(TransportRouteKind::DirectWorkersDev)
        );

        // A single reconnect failure preserves sticky route 1
        assert!(!ladder.record_failure());
        assert_eq!(ladder.current_index(), 1);
        assert_eq!(ladder.consecutive_failures(), 1);

        // Second failure moves to route 2
        assert!(ladder.record_failure());
        assert_eq!(ladder.current_index(), 2);
    }

    #[test]
    fn collects_non_secret_transport_evidence() {
        let evidence = collect_transport_evidence(
            Some("https://herdr.example.com"),
            Some("https://backend.workers.dev"),
        );
        assert_eq!(evidence.mcp_origin, "custom-domain");
        assert_eq!(evidence.link_upstream, "workers.dev");
        assert_eq!(evidence.link_transport, "direct");
        assert_eq!(evidence.relay, "none");
        assert_eq!(evidence.pool_source, "embedded");
    }
}
