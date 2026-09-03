//! Provider-neutral Link transport ladder and route selection.
//!
//! Implements the Link transport policy specified in `docs/_wip/v0.4.5-release-plan.md`:
//! 1. direct Custom Domain when configured/healthy
//! 2. direct workers.dev
//! 3. validated explicit/system local proxy (existing HTTP/SOCKS implementation)
//! 4. sticky shared Relay endpoints
//! 5. next healthy relay after bounded transport failures
//!
//! Explicit `HERDR_LINK_PROXY` authority: when an explicit `HERDR_LINK_PROXY` is
//! configured and validated, it is attempted first before direct connections.
//! Discovered system/env proxies (`HTTPS_PROXY`, `ALL_PROXY`, macOS system)
//! remain fallback routes after direct attempts.
//!
//! Critical invariants:
//! - Transport changes never replay an in-flight mutation.
//! - Ordinary socket drops use existing reconnect backoff before failover.
//! - A successful route is sticky for the healthy connection.
//! - Status/doctor exposes non-secret selected transport/proxy/relay/failover evidence.
//! - Invalid or non-workers.dev relay candidates fail closed and cannot become a generic proxy.
//! - No operator/private upstreams. Embedded Relay defaults are limited to exact
//!   public endpoints that passed the v0.4.5 mainland-China qualification UAT.
//! - Route exhaustion saturates on the last candidate without tight cycling back to direct.

use url::Url;

use super::proxy::{LinkProxyResolution, ProxySource, ResolvedProxy, resolve_link_proxy_detailed};
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
    #[serde(default = "default_relay_weight")]
    pub weight: u32,
    pub failure_domain: String,
    pub enabled: bool,
}

pub const RELAY_POLICY_SLUG: &str = "fallback-only-no-custom-domain";
pub const RELAY_SELECTION_SLUG: &str = "stable-weighted-per-device";
pub const RELAY_POLICY_DESCRIPTION: &str = "shared Relay is an outbound fallback only: it is used only when no Custom Domain is configured and direct workers.dev plus any local proxy path are unavailable";

const FNV1A64_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV1A64_PRIME: u64 = 0x100000001b3;

const fn default_relay_weight() -> u32 {
    1
}

fn stable_relay_hash(parts: &[&str]) -> u64 {
    let mut hash = FNV1A64_OFFSET_BASIS;
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            hash ^= 0xff;
            hash = hash.wrapping_mul(FNV1A64_PRIME);
        }
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV1A64_PRIME);
        }
    }
    hash
}

fn order_relay_candidates(
    relay_pool: &[RelayEndpoint],
    workstation_id: &str,
) -> Vec<RelayEndpoint> {
    let mut candidates = relay_pool
        .iter()
        .filter(|relay| relay.enabled && relay.weight > 0)
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut ordered = Vec::with_capacity(candidates.len());
    let mut start = 0;
    while start < candidates.len() {
        let priority = candidates[start].priority;
        let mut end = start + 1;
        while end < candidates.len() && candidates[end].priority == priority {
            end += 1;
        }

        let mut tier = candidates[start..end].to_vec();
        tier.sort_by(|left, right| left.id.cmp(&right.id));
        let total_weight = tier
            .iter()
            .map(|relay| u64::from(relay.weight))
            .sum::<u64>();
        if total_weight > 0 {
            let priority_text = priority.to_string();
            let slot = stable_relay_hash(&[workstation_id, &priority_text]) % total_weight;
            let mut cumulative = 0_u64;
            let primary_index = tier
                .iter()
                .position(|relay| {
                    cumulative = cumulative.saturating_add(u64::from(relay.weight));
                    slot < cumulative
                })
                .unwrap_or(0);
            let primary = tier.remove(primary_index);
            tier.sort_by(|left, right| {
                stable_relay_hash(&[workstation_id, &right.id])
                    .cmp(&stable_relay_hash(&[workstation_id, &left.id]))
                    .then_with(|| left.id.cmp(&right.id))
            });
            ordered.push(primary);
            ordered.extend(tier);
        }

        start = end;
    }
    ordered
}

/// Qualified baseline Relay candidates for fresh installs.
///
/// These exact public endpoints passed the v0.4.5 mainland-China no-proxy UAT.
/// A fresh runtime therefore has a bounded fallback when no newer signed Relay
/// Pool manifest is present in cache. A valid cached remote manifest replaces
/// this baseline; it does not merge with it.
pub fn default_embedded_relays() -> Vec<RelayEndpoint> {
    vec![
        RelayEndpoint {
            id: "deno".to_owned(),
            url: "wss://relay.herdr-mcp.deno.net/v1".to_owned(),
            priority: 200,
            weight: 100,
            failure_domain: "deno-deploy".to_owned(),
            enabled: true,
        },
        RelayEndpoint {
            id: "supabase".to_owned(),
            url: "wss://sppeaueojvcxifimozqx.supabase.co/functions/v1/herdr-relay/v1".to_owned(),
            priority: 200,
            weight: 1,
            failure_domain: "supabase-ap-southeast-1".to_owned(),
            enabled: true,
        },
    ]
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

fn is_loopback_or_test_host(host: &str) -> bool {
    host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host.ends_with(".test")
        || host.ends_with(".local")
}

fn extract_origin_host(raw: &str) -> Result<String, LadderError> {
    let url = Url::parse(raw).map_err(|_| LadderError::InvalidOrigin(raw.to_owned()))?;
    let host = url
        .host_str()
        .ok_or_else(|| LadderError::InvalidOrigin(raw.to_owned()))?
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err(LadderError::InvalidOrigin(raw.to_owned()));
    }
    match url.scheme() {
        "https" | "wss" => Ok(host),
        "http" | "ws" if is_loopback_or_test_host(&host) => Ok(host),
        _ => Err(LadderError::InsecureScheme(raw.to_owned())),
    }
}

fn normalize_origin_to_ws_base(raw: &str) -> Result<String, LadderError> {
    let mut url = Url::parse(raw).map_err(|_| LadderError::InvalidOrigin(raw.to_owned()))?;
    let host = url
        .host_str()
        .ok_or_else(|| LadderError::InvalidOrigin(raw.to_owned()))?
        .to_ascii_lowercase();
    let scheme = match url.scheme() {
        "https" | "wss" => "wss",
        "http" | "ws" if is_loopback_or_test_host(&host) => "ws",
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
/// - If explicit `HERDR_LINK_PROXY` is resolved, local proxy is route 0 (authoritative).
/// - Otherwise:
///   1. Direct Custom Domain when configured
///   2. Direct workers.dev
///   3. Validated system/env local proxy (targeting workers.dev upstream)
///   4. Shared Relay endpoints only when no Custom Domain exists. Equal-priority
///      providers use stable per-workstation weighted selection, then bounded failover.
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

    // Build local proxy route if proxy is configured
    // Proxy target prefers backing link upstream (workers.dev) over custom domain
    let local_proxy_route = if let Some(resolved_proxy) = &proxy {
        let target = workers_dev.or(custom_domain);
        if let Some(target_origin) = target {
            let base_url = normalize_origin_to_ws_base(target_origin)?;
            let ws_url = build_edge_url(&base_url, workstation_id)
                .map_err(|_| LadderError::InvalidUrl(base_url))?;
            Some(TransportRoute {
                kind: TransportRouteKind::LocalProxy,
                endpoint_url: ws_url,
                proxy: Some(resolved_proxy.clone()),
                relay_id: None,
            })
        } else {
            None
        }
    } else {
        None
    };

    let is_explicit_herdr_proxy = proxy
        .as_ref()
        .is_some_and(|p| p.source == ProxySource::HerdrLinkProxy);

    let mut routes = Vec::new();

    // Explicit HERDR_LINK_PROXY is authoritative and attempted first
    if is_explicit_herdr_proxy && let Some(p_route) = &local_proxy_route {
        routes.push(p_route.clone());
    }

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

    // 3. Fallback local proxy (when not explicit HERDR_LINK_PROXY)
    if !is_explicit_herdr_proxy && let Some(p_route) = local_proxy_route {
        routes.push(p_route);
    }

    // 4. & 5. Sticky shared Relay endpoints & pool failovers. Relay is an
    // emergency egress fallback, not a replacement for an operator-owned
    // Custom Domain.
    if custom_domain.is_none()
        && let Some(wd) = workers_dev
    {
        let host = extract_origin_host(wd)?;
        if validate_workers_dev_host(&host).is_ok() {
            for relay in order_relay_candidates(relay_pool, workstation_id) {
                if let Ok(relay_url) = build_relay_edge_url(&relay.url, &host, workstation_id) {
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

    /// Select the first route of `kind` before the ladder has gone online.
    ///
    /// This is deliberately a one-time initial-selection seam for diagnostics
    /// and provider UAT. It never changes an already-active sticky route and it
    /// never manufactures a route that is absent from the validated ladder.
    pub fn select_initial_route_kind(&mut self, kind: TransportRouteKind) -> bool {
        if self.active_route.is_some() {
            return false;
        }
        let Some(index) = self.routes.iter().position(|route| route.kind == kind) else {
            return false;
        };
        self.current_index = index;
        self.consecutive_failures = 0;
        true
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
    /// the ladder (saturating at the final route upon exhaustion) and returns `true`.
    /// Otherwise returns `false` (stays on the current route for standard reconnect backoff).
    pub fn record_failure(&mut self) -> bool {
        if self.routes.is_empty() {
            return false;
        }
        self.consecutive_failures += 1;
        if self.consecutive_failures >= self.max_failures_per_route {
            if self.current_index + 1 < self.routes.len() {
                self.current_index += 1;
                self.consecutive_failures = 0;
                true
            } else {
                // Route exhaustion: saturate on the final route without tight wrap-around
                self.consecutive_failures = self.max_failures_per_route;
                false
            }
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TransportEvidence {
    pub mcp_origin: String,
    pub link_upstream: String,
    pub live_transport: String,
    pub configured_preferred_transport: String,
    pub proxy_source: String,
    pub relay: String,
    pub relay_policy: String,
    pub relay_selection: String,
    pub pool_source: String,
    pub failover_ready: bool,
    pub candidate_count: usize,
}

/// Collect non-secret transport ladder evidence for `status` and `doctor`.
///
/// Reports `live_transport: unknown` when querying outside the live daemon
/// rather than manufacturing a false direct/relay claim from static config.
pub fn collect_transport_evidence(
    edge_public_origin: Option<&str>,
    link_upstream_origin: Option<&str>,
) -> TransportEvidence {
    collect_transport_evidence_with_pool(
        edge_public_origin,
        link_upstream_origin,
        &default_embedded_relays(),
        "embedded",
    )
}

pub fn collect_transport_evidence_with_pool(
    edge_public_origin: Option<&str>,
    link_upstream_origin: Option<&str>,
    relay_pool: &[RelayEndpoint],
    pool_source: &str,
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

    let edge_candidate = link_upstream_origin
        .or(edge_public_origin)
        .unwrap_or("wss://unconfigured.local/ws");

    let routes = build_ladder_routes(
        edge_candidate,
        edge_public_origin,
        link_upstream_origin,
        "probe-device",
        resolved_proxy,
        relay_pool,
    )
    .unwrap_or_default();

    let candidate_count = routes.len();
    let first_route = routes.first();
    let configured_preferred_transport = first_route
        .map(|r| r.kind.category_str().to_owned())
        .unwrap_or_else(|| "none".to_owned());
    let failover_ready = candidate_count > 1;

    TransportEvidence {
        mcp_origin,
        link_upstream,
        live_transport: "unknown".to_owned(),
        configured_preferred_transport,
        proxy_source,
        relay: "unknown".to_owned(),
        relay_policy: RELAY_POLICY_SLUG.to_owned(),
        relay_selection: RELAY_SELECTION_SLUG.to_owned(),
        pool_source: pool_source.to_owned(),
        failover_ready,
        candidate_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::proxy::ProxySource;

    #[test]
    fn default_embedded_relays_match_the_qualified_mainland_pool() {
        let pool = default_embedded_relays();
        assert_eq!(pool.len(), 2);
        assert_eq!(pool[0].id, "deno");
        assert_eq!(pool[0].url, "wss://relay.herdr-mcp.deno.net/v1");
        assert_eq!(pool[0].priority, 200);
        assert_eq!(pool[0].weight, 100);
        assert_eq!(pool[0].failure_domain, "deno-deploy");
        assert!(pool[0].enabled);
        assert_eq!(pool[1].id, "supabase");
        assert_eq!(
            pool[1].url,
            "wss://sppeaueojvcxifimozqx.supabase.co/functions/v1/herdr-relay/v1"
        );
        assert_eq!(pool[1].priority, 200);
        assert_eq!(pool[1].weight, 1);
        assert_eq!(pool[1].failure_domain, "supabase-ap-southeast-1");
        assert!(pool[1].enabled);
    }

    fn relay_fixture(id: &str, weight: u32) -> RelayEndpoint {
        RelayEndpoint {
            id: id.to_owned(),
            url: format!("wss://{id}.relay.test/v1"),
            priority: 200,
            weight,
            failure_domain: id.to_owned(),
            enabled: true,
        }
    }

    #[test]
    fn relay_is_not_offered_when_custom_domain_exists() {
        let relays = vec![relay_fixture("deno", 100), relay_fixture("supabase", 1)];
        let routes = build_ladder_routes(
            "wss://herdr.example.test/ws",
            Some("https://herdr.example.test"),
            Some("https://my-worker.workers.dev"),
            "dev_test",
            None,
            &relays,
        )
        .unwrap();

        assert!(
            routes
                .iter()
                .all(|route| route.kind != TransportRouteKind::SharedRelay)
        );
    }

    #[test]
    fn equal_priority_relays_use_stable_weighted_per_device_selection() {
        let relays = vec![relay_fixture("deno", 9), relay_fixture("supabase", 1)];
        let mut deno = 0;
        let mut supabase = 0;

        for index in 0..1000 {
            let workstation_id = format!("dev_weighted_{index}");
            let routes = build_ladder_routes(
                "wss://my-worker.workers.dev/ws",
                Some("https://my-worker.workers.dev"),
                None,
                &workstation_id,
                None,
                &relays,
            )
            .unwrap();
            let relay_routes = routes
                .iter()
                .filter(|route| route.kind == TransportRouteKind::SharedRelay)
                .collect::<Vec<_>>();
            assert_eq!(relay_routes.len(), 2);
            match relay_routes[0].relay_id.as_deref() {
                Some("deno") => deno += 1,
                Some("supabase") => supabase += 1,
                other => panic!("unexpected relay primary {other:?}"),
            }

            let repeated = build_ladder_routes(
                "wss://my-worker.workers.dev/ws",
                Some("https://my-worker.workers.dev"),
                None,
                &workstation_id,
                None,
                &relays,
            )
            .unwrap();
            assert_eq!(routes, repeated, "selection must be sticky for one device");
        }

        assert!(deno > 800, "9:1 weight should keep most devices on Deno");
        assert!(supabase > 0, "secondary provider must receive some load");
        assert_eq!(deno + supabase, 1000);
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

        // Production plain http rejects on non-local domain
        let err_insecure_http = build_ladder_routes(
            "wss://my-worker.workers.dev/ws",
            Some("http://production-domain.com"),
            None,
            "dev_test",
            None,
            &[],
        );
        assert!(matches!(
            err_insecure_http,
            Err(LadderError::InsecureScheme(_))
        ));

        let ladder_err = TransportLadder::new(vec![], 2);
        assert!(matches!(ladder_err, Err(LadderError::NoAvailableRoutes)));
    }

    #[test]
    fn preserves_existing_edge_url_when_no_origins_configured() {
        let routes = build_ladder_routes(
            "wss://my-custom.domain.test/ws",
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
            "wss://my-custom.domain.test/ws/dev_test"
        );
        assert!(routes[0].proxy.is_none());
    }

    #[test]
    fn herdr_link_proxy_is_authoritative_first_route() {
        let explicit_proxy = Some(ResolvedProxy {
            url: "http://127.0.0.1:7890".to_owned(),
            source: ProxySource::HerdrLinkProxy,
        });

        let routes = build_ladder_routes(
            "wss://herdr.example.test/ws",
            Some("https://herdr.example.test"),
            Some("https://my-worker.workers.dev"),
            "dev_test",
            explicit_proxy,
            &[],
        )
        .unwrap();

        // HERDR_LINK_PROXY goes first!
        assert_eq!(routes.len(), 3);
        assert_eq!(routes[0].kind, TransportRouteKind::LocalProxy);
        assert_eq!(
            routes[0].endpoint_url,
            "wss://my-worker.workers.dev/ws/dev_test"
        );
        assert!(routes[0].proxy.is_some());

        assert_eq!(routes[1].kind, TransportRouteKind::DirectCustomDomain);
        assert_eq!(
            routes[1].endpoint_url,
            "wss://herdr.example.test/ws/dev_test"
        );

        assert_eq!(routes[2].kind, TransportRouteKind::DirectWorkersDev);
        assert_eq!(
            routes[2].endpoint_url,
            "wss://my-worker.workers.dev/ws/dev_test"
        );
    }

    #[test]
    fn system_or_env_proxy_is_fallback_after_direct_routes() {
        let env_proxy = Some(ResolvedProxy {
            url: "http://127.0.0.1:7890".to_owned(),
            source: ProxySource::HttpsProxy,
        });

        let routes = build_ladder_routes(
            "wss://herdr.example.test/ws",
            Some("https://herdr.example.test"),
            Some("https://my-worker.workers.dev"),
            "dev_test",
            env_proxy,
            &[],
        )
        .unwrap();

        // Direct routes first, then env proxy
        assert_eq!(routes.len(), 3);
        assert_eq!(routes[0].kind, TransportRouteKind::DirectCustomDomain);
        assert_eq!(routes[1].kind, TransportRouteKind::DirectWorkersDev);
        assert_eq!(routes[2].kind, TransportRouteKind::LocalProxy);
        assert_eq!(
            routes[2].endpoint_url,
            "wss://my-worker.workers.dev/ws/dev_test"
        );
    }

    #[test]
    fn local_proxy_targets_workers_dev_upstream_over_custom_domain() {
        let proxy = Some(ResolvedProxy {
            url: "http://127.0.0.1:7890".to_owned(),
            source: ProxySource::AllProxy,
        });

        let routes = build_ladder_routes(
            "wss://herdr.example.test/ws",
            Some("https://herdr.example.test"),
            Some("https://my-worker.workers.dev"),
            "dev_test",
            proxy,
            &[],
        )
        .unwrap();

        let proxy_route = routes
            .iter()
            .find(|r| r.kind == TransportRouteKind::LocalProxy)
            .expect("local proxy route must exist");
        assert_eq!(
            proxy_route.endpoint_url, "wss://my-worker.workers.dev/ws/dev_test",
            "local proxy route must target backing workers.dev origin"
        );
    }

    #[test]
    fn ladder_bounded_failover_advances_and_saturates_without_modulo_wrap() {
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

        // Failure 2 on route 1 -> triggers failover to route 2 (last route)
        assert!(ladder.record_failure());
        assert_eq!(ladder.current_index(), 2);
        assert_eq!(ladder.current_route().kind, TransportRouteKind::SharedRelay);

        // Failures on route 2 must saturate on route 2 without modulo-wrapping back to route 0
        assert!(!ladder.record_failure());
        assert_eq!(ladder.current_index(), 2);
        assert!(!ladder.record_failure());
        assert_eq!(
            ladder.current_index(),
            2,
            "route exhaustion must saturate on final route instead of cycling"
        );
    }

    #[test]
    fn initial_route_selection_can_start_on_validated_relay_but_never_retarget_online_route() {
        let routes = vec![
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
        assert!(ladder.select_initial_route_kind(TransportRouteKind::SharedRelay));
        assert_eq!(ladder.current_index(), 1);
        assert_eq!(ladder.current_route().kind, TransportRouteKind::SharedRelay);

        ladder.record_success();
        assert!(!ladder.select_initial_route_kind(TransportRouteKind::DirectWorkersDev));
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
            Some("https://herdr.example.test"),
            Some("https://backend.workers.dev"),
        );
        assert_eq!(evidence.mcp_origin, "custom-domain");
        assert_eq!(evidence.link_upstream, "workers.dev");
        assert_eq!(evidence.live_transport, "unknown");
        assert_eq!(evidence.configured_preferred_transport, "direct");
        assert_eq!(evidence.relay, "unknown");
        assert_eq!(evidence.pool_source, "embedded");
    }
}
