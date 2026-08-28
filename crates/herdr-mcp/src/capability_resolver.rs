use crate::agent_visibility::AgentVisibility;
use crate::capability_inventory::AgentCapabilityRecord;
use serde_json::Value;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorkerCapability {
    pub agent_id: String,
    pub kind: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub supports_code_edit: Option<bool>,
    pub supports_shell: Option<bool>,
    pub supports_vision: Option<bool>,
    pub reasoning_tier: Option<u8>,
    pub latency_tier: Option<u8>,
    pub cost_tier: Option<u8>,
    pub context_tier: Option<u8>,
    pub interactive_only: Option<bool>,
    pub can_run_headless: Option<bool>,
    pub allowed_for_auto_dispatch: bool,
    pub current_status: String,
    pub current_project: Option<String>,
    pub cwd: Option<String>,
    pub pane_id: Option<String>,
    pub workspace_id: Option<String>,
    pub interactive_ready: Option<bool>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CapabilitySnapshot {
    pub source: String,
    pub revision: Option<u64>,
    pub workers: Vec<WorkerCapability>,
    pub hidden_workers: usize,
}

#[cfg(test)]
pub fn project_capabilities(snapshot: &Value, visibility: &AgentVisibility) -> CapabilitySnapshot {
    project_capabilities_with_inventory(snapshot, visibility, &[])
}

pub fn project_capabilities_with_inventory(
    snapshot: &Value,
    visibility: &AgentVisibility,
    inventory: &[AgentCapabilityRecord],
) -> CapabilitySnapshot {
    let agents = snapshot
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total_agents = agents.len();
    let agents = agents
        .into_iter()
        .filter(|agent| {
            visibility.is_visible(
                agent.get("name").and_then(Value::as_str),
                agent
                    .get("agent")
                    .or_else(|| agent.get("kind"))
                    .and_then(Value::as_str),
            )
        })
        .collect::<Vec<_>>();
    let hidden_workers = total_agents.saturating_sub(agents.len());
    let revision = agents
        .iter()
        .filter_map(|agent| agent.get("state_change_seq").and_then(Value::as_u64))
        .max();
    let workers = agents
        .iter()
        .filter_map(|agent| worker_from_agent(agent, visibility, inventory))
        .collect();
    CapabilitySnapshot {
        source: if inventory.is_empty() {
            "herdr:event-cache".to_owned()
        } else {
            "herdr:event-cache+capability-inventory".to_owned()
        },
        revision,
        workers,
        hidden_workers,
    }
}

fn worker_from_agent(
    agent: &Value,
    visibility: &AgentVisibility,
    inventory: &[AgentCapabilityRecord],
) -> Option<WorkerCapability> {
    let kind = agent
        .get("agent")
        .or_else(|| agent.get("kind"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let name = agent.get("name").and_then(Value::as_str).map(str::to_owned);
    let agent_id = name.clone().or_else(|| kind.clone())?;
    let status = agent
        .get("agent_status")
        .or_else(|| agent.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let cwd = agent.get("cwd").and_then(Value::as_str).map(str::to_owned);
    let scanned = kind
        .as_deref()
        .and_then(|kind| inventory.iter().find(|record| record.agent == kind));
    Some(WorkerCapability {
        agent_id,
        kind: kind.clone(),
        provider: scanned
            .and_then(|record| record.provider.as_ref().map(|value| value.value.clone())),
        model: scanned.and_then(|record| record.model.as_ref().map(|value| value.value.clone())),
        profile: scanned
            .and_then(|record| record.profile.as_ref().map(|value| value.value.clone())),
        supports_code_edit: scanned
            .and_then(|record| record.supports_code_edit.as_ref().map(|value| value.value)),
        supports_shell: scanned
            .and_then(|record| record.supports_shell.as_ref().map(|value| value.value)),
        supports_vision: scanned
            .and_then(|record| record.supports_vision.as_ref().map(|value| value.value)),
        reasoning_tier: scanned
            .and_then(|record| record.reasoning_tier.as_ref().map(|value| value.value)),
        latency_tier: scanned
            .and_then(|record| record.latency_tier.as_ref().map(|value| value.value)),
        cost_tier: scanned.and_then(|record| record.cost_tier.as_ref().map(|value| value.value)),
        context_tier: scanned
            .and_then(|record| record.context_tier.as_ref().map(|value| value.value)),
        interactive_only: scanned
            .and_then(|record| record.interactive_only.as_ref().map(|value| value.value)),
        can_run_headless: scanned
            .and_then(|record| record.can_run_headless.as_ref().map(|value| value.value)),
        allowed_for_auto_dispatch: visibility.is_visible(name.as_deref(), kind.as_deref()),
        current_status: status,
        current_project: cwd.clone(),
        cwd,
        pane_id: agent
            .get("pane_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        workspace_id: agent
            .get("workspace_id")
            .or_else(|| agent.get("workspace"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        interactive_ready: agent.get("interactive_ready").and_then(Value::as_bool),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_inventory::{Evidence, INVENTORY_SCHEMA_VERSION, ProbeLevel};
    use serde_json::json;

    fn record(agent: &str) -> AgentCapabilityRecord {
        let evidence = |value| Evidence {
            value,
            source: "test_probe".to_owned(),
            authority: "reported".to_owned(),
            observed_at_ms: 1,
            detail: None,
        };
        AgentCapabilityRecord {
            schema_version: INVENTORY_SCHEMA_VERSION,
            agent: agent.to_owned(),
            manifest_version: Some("1".to_owned()),
            manifest_source: Some("bundled".to_owned()),
            manifest_source_kind: Some("bundled".to_owned()),
            binary_path: Some(format!("/bin/{agent}")),
            herdr_startable: None,
            executable_available: None,
            available_for_start: None,
            binary_version: None,
            provider: None,
            model: None,
            profile: None,
            supports_code_edit: Some(evidence(true)),
            supports_shell: None,
            supports_vision: None,
            reasoning_tier: None,
            latency_tier: None,
            cost_tier: None,
            context_tier: None,
            interactive_only: None,
            can_run_headless: Some(evidence(true)),
            probe_level: ProbeLevel::Deep,
            probe_adapter_version: 1,
            fingerprint: format!("sha256:{agent}"),
            observed_at_ms: 1,
        }
    }

    #[test]
    fn resolver_merges_static_evidence_without_overriding_live_state() {
        let visibility = AgentVisibility::Allow(["pi".to_owned()].into_iter().collect());
        let snapshot = project_capabilities_with_inventory(
            &json!({
                "agents": [{
                    "agent": "pi",
                    "name": "worker",
                    "agent_status": "working",
                    "cwd": "/repo",
                    "pane_id": "w1:p1",
                    "workspace_id": "w1",
                    "interactive_ready": true,
                    "state_change_seq": 7
                }]
            }),
            &visibility,
            &[record("pi")],
        );
        let worker = &snapshot.workers[0];
        assert_eq!(snapshot.source, "herdr:event-cache+capability-inventory");
        assert_eq!(worker.supports_code_edit, Some(true));
        assert_eq!(worker.can_run_headless, Some(true));
        assert_eq!(worker.current_status, "working");
        assert_eq!(worker.current_project.as_deref(), Some("/repo"));
        assert_eq!(worker.interactive_ready, Some(true));
    }

    #[test]
    fn resolver_keeps_unverified_traits_unknown_without_inventory() {
        let visibility = AgentVisibility::Allow(["pi".to_owned()].into_iter().collect());
        let snapshot = project_capabilities(
            &json!({
                "agents": [{
                    "agent": "pi",
                    "agent_status": "idle",
                    "cwd": "/repo"
                }]
            }),
            &visibility,
        );
        let worker = &snapshot.workers[0];
        assert!(worker.provider.is_none());
        assert!(worker.model.is_none());
        assert!(worker.supports_code_edit.is_none());
        assert!(worker.can_run_headless.is_none());
    }
}
