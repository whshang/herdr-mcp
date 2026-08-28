use crate::capability_resolver::{CapabilitySnapshot, WorkerCapability};

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct TaskProfile {
    pub deterministic_tool: Option<String>,
    pub project_root: Option<String>,
    pub explicit_target: Option<String>,
    pub requires_code_edit: bool,
    pub requires_shell: bool,
    pub requires_vision: bool,
    pub minimum_reasoning_tier: Option<u8>,
    pub destructive_production_mutation: bool,
    pub delegates_other_workers: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DispatchAction {
    DirectTool(String),
    Worker(String),
    NoDispatch,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DispatchDecision {
    pub action: DispatchAction,
    pub reason: String,
    pub rejected: Vec<String>,
}

pub fn decide_dispatch(task: &TaskProfile, snapshot: &CapabilitySnapshot) -> DispatchDecision {
    if let Some(tool) = &task.deterministic_tool {
        return DispatchDecision {
            action: DispatchAction::DirectTool(tool.clone()),
            reason: "deterministic_native_tool_available".to_owned(),
            rejected: vec![],
        };
    }
    if task.destructive_production_mutation {
        return no_dispatch("destructive_production_mutation_not_auto_delegated", vec![]);
    }
    if task.delegates_other_workers {
        return no_dispatch("middle_manager_delegation_forbidden", vec![]);
    }

    if let Some(explicit) = task.explicit_target.as_deref() {
        let Some(worker) = snapshot
            .workers
            .iter()
            .find(|worker| matches_target(worker, explicit))
        else {
            return no_dispatch("explicit_target_not_found", vec![explicit.to_owned()]);
        };
        return match reject_reason(worker, task) {
            Some(reason) => no_dispatch(
                "explicit_target_unavailable_or_incompatible",
                vec![format!("{}:{reason}", worker.agent_id)],
            ),
            None => DispatchDecision {
                action: DispatchAction::Worker(worker.agent_id.clone()),
                reason: "explicit_user_target_preserved".to_owned(),
                rejected: vec![],
            },
        };
    }

    let mut rejected = Vec::new();
    let mut eligible = Vec::new();
    for worker in &snapshot.workers {
        if let Some(reason) = reject_reason(worker, task) {
            rejected.push(format!("{}:{reason}", worker.agent_id));
        } else {
            eligible.push(worker);
        }
    }
    eligible.sort_by_key(|worker| {
        (
            worker.cost_tier.unwrap_or(u8::MAX),
            worker.latency_tier.unwrap_or(u8::MAX),
            worker.agent_id.clone(),
        )
    });
    match eligible.first() {
        Some(worker) => DispatchDecision {
            action: DispatchAction::Worker(worker.agent_id.clone()),
            reason: "compatible_live_worker_selected".to_owned(),
            rejected,
        },
        None => no_dispatch("no_compatible_live_worker", rejected),
    }
}

fn matches_target(worker: &WorkerCapability, target: &str) -> bool {
    worker.agent_id == target
        || worker.kind.as_deref() == Some(target)
        || worker.pane_id.as_deref() == Some(target)
}

fn reject_reason(worker: &WorkerCapability, task: &TaskProfile) -> Option<&'static str> {
    if !worker.allowed_for_auto_dispatch {
        return Some("auto_dispatch_not_allowed");
    }
    if matches!(
        worker.current_status.as_str(),
        "working" | "blocked" | "unknown"
    ) {
        return Some("worker_busy_or_blocked");
    }
    if let Some(project_root) = task.project_root.as_deref()
        && worker.current_project.as_deref() != Some(project_root)
    {
        return Some("project_mismatch");
    }
    if task.requires_code_edit && worker.supports_code_edit != Some(true) {
        return Some("code_edit_capability_not_verified");
    }
    if task.requires_shell && worker.supports_shell != Some(true) {
        return Some("shell_capability_not_verified");
    }
    if task.requires_vision && worker.supports_vision != Some(true) {
        return Some("vision_capability_not_verified");
    }
    if let Some(minimum) = task.minimum_reasoning_tier
        && worker.reasoning_tier.is_none_or(|actual| actual < minimum)
    {
        return Some("reasoning_quality_below_or_unknown");
    }
    None
}

fn no_dispatch(reason: &str, rejected: Vec<String>) -> DispatchDecision {
    DispatchDecision {
        action: DispatchAction::NoDispatch,
        reason: reason.to_owned(),
        rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_visibility::AgentVisibility;
    use crate::capability_resolver::{project_capabilities, project_capabilities_with_inventory};

    fn worker(id: &str, status: &str) -> WorkerCapability {
        WorkerCapability {
            agent_id: id.to_owned(),
            kind: Some("synthetic".to_owned()),
            provider: None,
            model: None,
            profile: None,
            supports_code_edit: Some(true),
            supports_shell: Some(true),
            supports_vision: Some(false),
            reasoning_tier: Some(2),
            latency_tier: Some(2),
            cost_tier: Some(2),
            context_tier: None,
            interactive_only: None,
            can_run_headless: None,
            allowed_for_auto_dispatch: true,
            current_status: status.to_owned(),
            current_project: Some("/repo".to_owned()),
            cwd: Some("/repo".to_owned()),
            pane_id: Some(format!("pane-{id}")),
            workspace_id: Some("w1".to_owned()),
            interactive_ready: Some(true),
        }
    }

    fn snapshot(workers: Vec<WorkerCapability>) -> CapabilitySnapshot {
        CapabilitySnapshot {
            source: "synthetic".to_owned(),
            revision: Some(1),
            workers,
            hidden_workers: 0,
        }
    }

    fn scanned_pi() -> crate::capability_inventory::AgentCapabilityRecord {
        use crate::capability_inventory::{Evidence, INVENTORY_SCHEMA_VERSION, ProbeLevel};
        let evidence = |value| Evidence {
            value,
            source: "test_probe".to_owned(),
            authority: "reported".to_owned(),
            observed_at_ms: 1,
            detail: None,
        };
        crate::capability_inventory::AgentCapabilityRecord {
            schema_version: INVENTORY_SCHEMA_VERSION,
            agent: "pi".to_owned(),
            manifest_version: Some("1".to_owned()),
            manifest_source: Some("bundled".to_owned()),
            manifest_source_kind: Some("bundled".to_owned()),
            binary_path: Some("/bin/pi".to_owned()),
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
            fingerprint: "sha256:test".to_owned(),
            observed_at_ms: 1,
        }
    }

    #[test]
    fn deterministic_task_chooses_direct_tool() {
        let task = TaskProfile {
            deterministic_tool: Some("herdr_git".to_owned()),
            ..TaskProfile::default()
        };
        assert_eq!(
            decide_dispatch(&task, &snapshot(vec![worker("a", "idle")])).action,
            DispatchAction::DirectTool("herdr_git".to_owned())
        );
    }

    #[test]
    fn suitable_task_selects_compatible_worker() {
        let task = TaskProfile {
            project_root: Some("/repo".to_owned()),
            requires_code_edit: true,
            ..TaskProfile::default()
        };
        assert_eq!(
            decide_dispatch(&task, &snapshot(vec![worker("a", "idle")])).action,
            DispatchAction::Worker("a".to_owned())
        );
    }

    #[test]
    fn unknown_optional_traits_still_allow_generic_same_project_reasoning() {
        let visibility = AgentVisibility::Allow(["pi".to_owned()].into_iter().collect());
        let live = project_capabilities(
            &serde_json::json!({
                "agents": [{
                    "agent": "pi",
                    "name": "reviewer",
                    "agent_status": "idle",
                    "cwd": "/repo",
                    "pane_id": "w1:p9",
                    "workspace_id": "w1",
                    "state_change_seq": 42
                }]
            }),
            &visibility,
        );
        assert_eq!(live.workers[0].supports_code_edit, None);
        assert_eq!(live.workers[0].model, None);

        let task = TaskProfile {
            project_root: Some("/repo".to_owned()),
            ..TaskProfile::default()
        };
        let decision = decide_dispatch(&task, &live);
        assert_eq!(
            decision.action,
            DispatchAction::Worker("reviewer".to_owned())
        );
    }

    #[test]
    fn scanned_capability_enriches_worker_but_live_status_remains_authoritative() {
        let visibility = AgentVisibility::Allow(["pi".to_owned()].into_iter().collect());
        let inventory = vec![scanned_pi()];
        let live = serde_json::json!({
            "agents": [{
                "agent": "pi",
                "name": "worker",
                "agent_status": "working",
                "cwd": "/repo",
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "state_change_seq": 7
            }]
        });
        let snapshot = project_capabilities_with_inventory(&live, &visibility, &inventory);
        assert_eq!(snapshot.source, "herdr:event-cache+capability-inventory");
        assert_eq!(snapshot.workers[0].supports_code_edit, Some(true));
        assert_eq!(snapshot.workers[0].can_run_headless, Some(true));
        assert_eq!(snapshot.workers[0].current_status, "working");
        let task = TaskProfile {
            project_root: Some("/repo".to_owned()),
            requires_code_edit: true,
            ..TaskProfile::default()
        };
        assert_eq!(
            decide_dispatch(&task, &snapshot).action,
            DispatchAction::NoDispatch
        );

        let mut idle_live = live.clone();
        idle_live["agents"][0]["agent_status"] = serde_json::Value::String("idle".to_owned());
        let idle_snapshot =
            project_capabilities_with_inventory(&idle_live, &visibility, &inventory);
        assert_eq!(idle_snapshot.workers[0].supports_code_edit, Some(true));
        assert_eq!(idle_snapshot.workers[0].current_status, "idle");
        assert_eq!(
            decide_dispatch(&task, &idle_snapshot).action,
            DispatchAction::Worker("worker".to_owned())
        );
    }

    #[test]
    fn explicit_target_is_preserved_and_never_silently_replaced() {
        let task = TaskProfile {
            project_root: Some("/repo".to_owned()),
            explicit_target: Some("b".to_owned()),
            ..TaskProfile::default()
        };
        let decision = decide_dispatch(
            &task,
            &snapshot(vec![worker("a", "idle"), worker("b", "idle")]),
        );
        assert_eq!(decision.action, DispatchAction::Worker("b".to_owned()));
    }

    #[test]
    fn busy_and_blocked_workers_are_rejected() {
        let task = TaskProfile {
            project_root: Some("/repo".to_owned()),
            ..TaskProfile::default()
        };
        let decision = decide_dispatch(
            &task,
            &snapshot(vec![worker("a", "working"), worker("b", "blocked")]),
        );
        assert_eq!(decision.action, DispatchAction::NoDispatch);
        assert_eq!(decision.rejected.len(), 2);
    }

    #[test]
    fn capability_mismatch_is_rejected() {
        let task = TaskProfile {
            project_root: Some("/repo".to_owned()),
            requires_vision: true,
            ..TaskProfile::default()
        };
        let decision = decide_dispatch(&task, &snapshot(vec![worker("a", "idle")]));
        assert_eq!(decision.action, DispatchAction::NoDispatch);
    }

    #[test]
    fn equivalent_idle_worker_is_used_when_another_worker_is_busy() {
        let task = TaskProfile {
            project_root: Some("/repo".to_owned()),
            requires_code_edit: true,
            ..TaskProfile::default()
        };
        let decision = decide_dispatch(
            &task,
            &snapshot(vec![worker("a", "working"), worker("b", "idle")]),
        );
        assert_eq!(decision.action, DispatchAction::Worker("b".to_owned()));
    }

    #[test]
    fn lower_quality_fallback_is_not_selected() {
        let mut strong = worker("strong", "working");
        strong.reasoning_tier = Some(3);
        let mut weak = worker("weak", "idle");
        weak.reasoning_tier = Some(1);
        let task = TaskProfile {
            project_root: Some("/repo".to_owned()),
            minimum_reasoning_tier: Some(3),
            ..TaskProfile::default()
        };
        let decision = decide_dispatch(&task, &snapshot(vec![strong, weak]));
        assert_eq!(decision.action, DispatchAction::NoDispatch);
    }

    #[test]
    fn destructive_production_and_middle_manager_tasks_are_not_auto_dispatched() {
        let production = TaskProfile {
            destructive_production_mutation: true,
            ..TaskProfile::default()
        };
        assert_eq!(
            decide_dispatch(&production, &snapshot(vec![worker("a", "idle")])).action,
            DispatchAction::NoDispatch
        );
        let manager = TaskProfile {
            delegates_other_workers: true,
            ..TaskProfile::default()
        };
        assert_eq!(
            decide_dispatch(&manager, &snapshot(vec![worker("a", "idle")])).action,
            DispatchAction::NoDispatch
        );
    }
}
