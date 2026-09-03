//! Edge `/health` runtime-contract probe for Rust Link soak/install.
//!
//! The Edge public MCP contract may evolve independently from the workstation
//! runtime execution contract. New Edges publish `runtimeContractEpoch` /
//! `runtimeContractHash`; v0.4.2 Edges only publish `contractEpoch` /
//! `contractHash`, so parsing keeps that pair as a compatibility fallback.

use crate::link::daemon::{
    LEGACY_EPOCH1_CONTRACT_HASH, PUBLIC_CONTRACT_EPOCH, PUBLIC_CONTRACT_HASH,
};

/// Snapshot from Edge `GET /health` (non-secret fields only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeHealthContract {
    pub service: Option<String>,
    pub contract_epoch: u64,
    pub contract_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeContractError {
    TransportUnavailable(String),
    Message(String),
}

impl EdgeContractError {
    pub fn is_transport_unavailable(&self) -> bool {
        matches!(self, Self::TransportUnavailable(_))
    }
}

impl std::fmt::Display for EdgeContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TransportUnavailable(message) => write!(f, "{message}"),
            Self::Message(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for EdgeContractError {}

/// Map a Link WSS base URL (`wss://host/ws`) to the Edge HTTPS health URL.
pub fn health_url_from_edge_ws(edge_ws_url: &str) -> Result<String, EdgeContractError> {
    let parsed = url::Url::parse(edge_ws_url).map_err(|_| {
        EdgeContractError::Message("HERDR_EDGE_URL must be a valid wss:// or ws:// URL".to_owned())
    })?;
    let scheme = match parsed.scheme() {
        "wss" => "https",
        "ws" => "http",
        other => {
            return Err(EdgeContractError::Message(format!(
                "HERDR_EDGE_URL scheme must be wss:// or ws:// (got {other})"
            )));
        }
    };
    let host = parsed
        .host_str()
        .ok_or_else(|| EdgeContractError::Message("HERDR_EDGE_URL is missing a host".to_owned()))?;
    let port = match parsed.port() {
        Some(port) => format!(":{port}"),
        None => String::new(),
    };
    Ok(format!("{scheme}://{host}{port}/health"))
}

/// Parse Edge `/health` JSON into the runtime execution contract identity.
pub fn parse_edge_health_contract(body: &str) -> Result<EdgeHealthContract, EdgeContractError> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        EdgeContractError::Message(format!("Edge /health returned non-JSON: {error}"))
    })?;
    let epoch = value
        .get("runtimeContractEpoch")
        .or_else(|| value.get("contractEpoch"))
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            EdgeContractError::Message(
                "Edge /health missing numeric runtimeContractEpoch/contractEpoch".to_owned(),
            )
        })?;
    let hash = value
        .get("runtimeContractHash")
        .or_else(|| value.get("contractHash"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            EdgeContractError::Message(
                "Edge /health missing runtimeContractHash/contractHash".to_owned(),
            )
        })?
        .to_owned();
    let service = value
        .get("service")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Ok(EdgeHealthContract {
        service,
        contract_epoch: epoch,
        contract_hash: hash,
    })
}

/// Rust Link requires an Edge that accepts the current runtime execution contract.
pub fn rust_link_accepts_edge_contract(contract: &EdgeHealthContract) -> bool {
    contract.contract_epoch == PUBLIC_CONTRACT_EPOCH
        && contract.contract_hash == PUBLIC_CONTRACT_HASH
}

/// Human-readable refusal when Edge runtime compatibility is still epoch 1.
pub fn refuse_edge_for_rust_link(contract: &EdgeHealthContract) -> EdgeContractError {
    if contract.contract_epoch == 1 && contract.contract_hash == LEGACY_EPOCH1_CONTRACT_HASH {
        return EdgeContractError::Message(format!(
            "Edge runtime contract is still epoch 1 ({}); Rust link run requires runtime epoch 2 ({}). Point HERDR_EDGE_URL at a compatible Edge or deploy an Edge that accepts runtime epoch 2",
            contract.contract_hash, PUBLIC_CONTRACT_HASH
        ));
    }
    EdgeContractError::Message(format!(
        "Edge runtime contract epoch {} hash {} is incompatible with Rust link run (requires epoch {} hash {})",
        contract.contract_epoch,
        contract.contract_hash,
        PUBLIC_CONTRACT_EPOCH,
        PUBLIC_CONTRACT_HASH
    ))
}

/// Fetch Edge `/health` and refuse when the published contract is not Rust-ready.
pub fn probe_edge_contract_for_rust_link(
    edge_ws_url: &str,
) -> Result<EdgeHealthContract, EdgeContractError> {
    let health_url = health_url_from_edge_ws(edge_ws_url)?;
    let body = fetch_health_body(&health_url)?;
    let contract = parse_edge_health_contract(&body)?;
    if rust_link_accepts_edge_contract(&contract) {
        Ok(contract)
    } else {
        Err(refuse_edge_for_rust_link(&contract))
    }
}

fn fetch_health_body(health_url: &str) -> Result<String, EdgeContractError> {
    let output = std::process::Command::new("/usr/bin/curl")
        .args([
            "-fsS",
            "--max-time",
            "8",
            "-H",
            "accept: application/json",
            health_url,
        ])
        .output()
        .map_err(|error| {
            EdgeContractError::TransportUnavailable(format!("cannot probe Edge /health: {error}"))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = format!(
            "Edge /health probe failed for {health_url}: {}",
            stderr.trim()
        );
        // curl exit 22 means the HTTP endpoint was reachable but --fail
        // rejected the status. Reachable-but-unhealthy/protocol evidence must
        // never be bypassed by Relay. DNS/connect/TLS/reset/timeout failures
        // are transport-unavailable and may defer the final contract fence to
        // the authenticated Edge hello when a signed Relay route exists.
        return if output.status.code() == Some(22) {
            Err(EdgeContractError::Message(message))
        } else {
            Err(EdgeContractError::TransportUnavailable(message))
        };
    }
    String::from_utf8(output.stdout)
        .map_err(|_| EdgeContractError::Message("Edge /health returned non-UTF8 body".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_url_maps_wss_ws_base() {
        assert_eq!(
            health_url_from_edge_ws("wss://herdr-edge-prod.whshang.workers.dev/ws").unwrap(),
            "https://herdr-edge-prod.whshang.workers.dev/health"
        );
        assert_eq!(
            health_url_from_edge_ws("ws://127.0.0.1:8787/ws").unwrap(),
            "http://127.0.0.1:8787/health"
        );
    }

    #[test]
    fn parses_prod_shaped_health() {
        let body = r#"{"ok":true,"service":"herdr-edge-prod","contractEpoch":2,"contractHash":"sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8"}"#;
        let contract = parse_edge_health_contract(body).unwrap();
        assert!(rust_link_accepts_edge_contract(&contract));
        assert_eq!(contract.service.as_deref(), Some("herdr-edge-prod"));
    }

    #[test]
    fn prefers_runtime_contract_when_public_contract_has_advanced() {
        let body = r#"{"ok":true,"service":"herdr-edge-prod","contractEpoch":3,"contractHash":"sha256:public-v3","runtimeContractEpoch":2,"runtimeContractHash":"sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8"}"#;
        let contract = parse_edge_health_contract(body).unwrap();
        assert!(rust_link_accepts_edge_contract(&contract));
        assert_eq!(contract.contract_epoch, 2);
        assert_eq!(contract.contract_hash, PUBLIC_CONTRACT_HASH);
    }

    #[test]
    fn refuses_epoch1_dev_health() {
        let body = r#"{"ok":true,"service":"herdr-edge-dev","contractEpoch":1,"contractHash":"sha256:3f23083ae31b977dad21b1ec9d6919c49e1067a27f7b7eea7bdd021b54770c0d"}"#;
        let contract = parse_edge_health_contract(body).unwrap();
        assert!(!rust_link_accepts_edge_contract(&contract));
        let err = refuse_edge_for_rust_link(&contract).to_string();
        assert!(err.contains("epoch 1"));
        assert!(err.contains("runtime epoch 2"));
        assert!(err.contains("compatible Edge"));
    }

    #[test]
    fn only_transport_unavailability_is_relay_deferable() {
        assert!(
            EdgeContractError::TransportUnavailable("reset".to_owned()).is_transport_unavailable()
        );
        assert!(
            !EdgeContractError::Message("epoch mismatch".to_owned()).is_transport_unavailable()
        );
    }
}
