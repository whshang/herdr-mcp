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
    pub independent_units: Option<usize>,
    pub ownership_isolated: bool,
    pub shared_runtime_state: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorkerCandidate {
    pub agent_id: String,
    pub control_target: String,
    pub kind: Option<String>,
    pub provider: Option<String>,
    pub provider_source: Option<String>,
    pub model: Option<String>,
    pub model_source: Option<String>,
    pub current_status: String,
    pub current_project: Option<String>,
    pub workspace_id: Option<String>,
    pub reasoning_tier: Option<u8>,
    pub latency_tier: Option<u8>,
    pub cost_tier: Option<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorkerRejection {
    pub agent_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ParallelismAdvice {
    pub worth_considering: bool,
    pub max_useful_lanes: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DispatchAdvice {
    pub direct_tool: Option<String>,
    pub explicit_target: Option<String>,
    pub delegation_allowed: bool,
    pub reason: String,
    pub candidates: Vec<WorkerCandidate>,
    pub rejected: Vec<WorkerRejection>,
    pub parallelism: ParallelismAdvice,
}

/// Return evidence-backed planning material without selecting an unrequested
/// worker for the Web planner. Candidate order follows live snapshot order;
/// herdr-mcp does not encode a permanent agent/model ranking.
pub fn advise_dispatch(task: &TaskProfile, snapshot: &CapabilitySnapshot) -> DispatchAdvice {
    if let Some(tool) = &task.deterministic_tool {
        return DispatchAdvice {
            direct_tool: Some(tool.clone()),
            explicit_target: task.explicit_target.clone(),
            delegation_allowed: false,
            reason: "deterministic_native_tool_available".to_owned(),
            candidates: vec![],
            rejected: vec![],
            parallelism: no_parallelism("deterministic_tool_is_sufficient"),
        };
    }
    if task.destructive_production_mutation {
        return blocked_advice(
            task,
            snapshot,
            "destructive_production_mutation_not_auto_delegated",
        );
    }
    if task.delegates_other_workers {
        return blocked_advice(task, snapshot, "middle_manager_delegation_forbidden");
    }

    let mut rejected = Vec::new();
    let mut candidates = Vec::new();
    for worker in &snapshot.workers {
        if let Some(reason) = reject_reason(worker, task) {
            rejected.push(WorkerRejection {
                agent_id: worker.agent_id.clone(),
                reason: reason.to_owned(),
            });
        } else {
            candidates.push(candidate(worker));
        }
    }

    if let Some(explicit) = task.explicit_target.as_deref() {
        let matched = snapshot
            .workers
            .iter()
            .find(|worker| matches_target(worker, explicit));
        if matched.is_none() {
            rejected.push(WorkerRejection {
                agent_id: explicit.to_owned(),
                reason: "explicit_target_not_found".to_owned(),
            });
        }
        let explicit_compatible =
            matched.is_some_and(|worker| reject_reason(worker, task).is_none());
        return DispatchAdvice {
            direct_tool: None,
            explicit_target: Some(explicit.to_owned()),
            delegation_allowed: explicit_compatible,
            reason: if explicit_compatible {
                "explicit_user_target_available"
            } else {
                "explicit_target_unavailable_or_incompatible"
            }
            .to_owned(),
            candidates,
            rejected,
            parallelism: parallelism_advice(task),
        };
    }

    DispatchAdvice {
        direct_tool: None,
        explicit_target: None,
        delegation_allowed: !candidates.is_empty(),
        reason: if candidates.is_empty() {
            "no_compatible_live_worker"
        } else {
            "compatible_workers_available_planner_decides"
        }
        .to_owned(),
        candidates,
        rejected,
        parallelism: parallelism_advice(task),
    }
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

/// Compatibility helper for internal callers that still expect a decision.
/// Unrequested workers are no longer auto-selected; only an explicit compatible
/// target can resolve to `Worker`.
pub fn decide_dispatch(task: &TaskProfile, snapshot: &CapabilitySnapshot) -> DispatchDecision {
    let advice = advise_dispatch(task, snapshot);
    let action = if let Some(tool) = advice.direct_tool.clone() {
        DispatchAction::DirectTool(tool)
    } else if advice.delegation_allowed {
        match advice.explicit_target.as_deref() {
            Some(target) => snapshot
                .workers
                .iter()
                .find(|worker| {
                    matches_target(worker, target) && reject_reason(worker, task).is_none()
                })
                .map(|worker| DispatchAction::Worker(worker.agent_id.clone()))
                .unwrap_or(DispatchAction::NoDispatch),
            None => DispatchAction::NoDispatch,
        }
    } else {
        DispatchAction::NoDispatch
    };
    DispatchDecision {
        action,
        reason: advice.reason,
        rejected: advice
            .rejected
            .into_iter()
            .map(|item| format!("{}:{}", item.agent_id, item.reason))
            .collect(),
    }
}

fn candidate(worker: &WorkerCapability) -> WorkerCandidate {
    WorkerCandidate {
        agent_id: worker.agent_id.clone(),
        control_target: worker
            .pane_id
            .clone()
            .unwrap_or_else(|| worker.agent_id.clone()),
        kind: worker.kind.clone(),
        provider: worker.provider.clone(),
        provider_source: worker.provider_source.clone(),
        model: worker.model.clone(),
        model_source: worker.model_source.clone(),
        current_status: worker.current_status.clone(),
        current_project: worker.current_project.clone(),
        workspace_id: worker.workspace_id.clone(),
        reasoning_tier: worker.reasoning_tier,
        latency_tier: worker.latency_tier,
        cost_tier: worker.cost_tier,
    }
}

fn blocked_advice(
    task: &TaskProfile,
    snapshot: &CapabilitySnapshot,
    reason: &str,
) -> DispatchAdvice {
    DispatchAdvice {
        direct_tool: None,
        explicit_target: task.explicit_target.clone(),
        delegation_allowed: false,
        reason: reason.to_owned(),
        candidates: vec![],
        rejected: snapshot
            .workers
            .iter()
            .map(|worker| WorkerRejection {
                agent_id: worker.agent_id.clone(),
                reason: reason.to_owned(),
            })
            .collect(),
        parallelism: no_parallelism(reason),
    }
}

fn parallelism_advice(task: &TaskProfile) -> ParallelismAdvice {
    let Some(independent_units) = task.independent_units else {
        return no_parallelism("task_independence_unspecified");
    };
    if independent_units < 2 {
        return no_parallelism("fewer_than_two_independent_units");
    }
    if task.shared_runtime_state {
        return no_parallelism("shared_runtime_or_state_requires_serial_ownership");
    }
    if (task.requires_code_edit || task.requires_shell) && !task.ownership_isolated {
        return no_parallelism("mutation_ownership_not_isolated");
    }
    ParallelismAdvice {
        worth_considering: true,
        max_useful_lanes: independent_units,
        reason: "independent_units_may_benefit_from_parallel_lanes".to_owned(),
    }
}

fn no_parallelism(reason: &str) -> ParallelismAdvice {
    ParallelismAdvice {
        worth_considering: false,
        max_useful_lanes: 1,
        reason: reason.to_owned(),
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
            provider_source: None,
            model: None,
            model_source: None,
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
    fn suitable_task_exposes_compatible_worker_without_auto_selecting_it() {
        let task = TaskProfile {
            project_root: Some("/repo".to_owned()),
            requires_code_edit: true,
            ..TaskProfile::default()
        };
        let snapshot = snapshot(vec![worker("a", "idle")]);
        let advice = advise_dispatch(&task, &snapshot);
        assert!(advice.delegation_allowed);
        assert_eq!(advice.candidates[0].agent_id, "a");
        assert_eq!(advice.candidates[0].control_target, "pane-a");
        assert_eq!(
            decide_dispatch(&task, &snapshot).action,
            DispatchAction::NoDispatch
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
        let advice = advise_dispatch(&task, &live);
        assert!(advice.delegation_allowed);
        assert_eq!(advice.candidates[0].agent_id, "reviewer");
        assert_eq!(
            decide_dispatch(&task, &live).action,
            DispatchAction::NoDispatch
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
        let advice = advise_dispatch(&task, &idle_snapshot);
        assert!(advice.delegation_allowed);
        assert_eq!(advice.candidates[0].agent_id, "worker");
        assert_eq!(
            decide_dispatch(&task, &idle_snapshot).action,
            DispatchAction::NoDispatch
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
    fn busy_worker_is_rejected_and_idle_worker_remains_an_advisory_candidate() {
        let task = TaskProfile {
            project_root: Some("/repo".to_owned()),
            requires_code_edit: true,
            ..TaskProfile::default()
        };
        let snapshot = snapshot(vec![worker("a", "working"), worker("b", "idle")]);
        let advice = advise_dispatch(&task, &snapshot);
        assert!(advice.delegation_allowed);
        assert_eq!(advice.candidates[0].agent_id, "b");
        assert_eq!(advice.rejected[0].agent_id, "a");
        assert_eq!(
            decide_dispatch(&task, &snapshot).action,
            DispatchAction::NoDispatch
        );
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

    #[test]
    fn candidate_order_does_not_encode_name_or_cost_ranking() {
        let mut first = worker("zeta", "idle");
        first.cost_tier = Some(4);
        let mut second = worker("alpha", "idle");
        second.cost_tier = Some(1);
        let advice = advise_dispatch(&TaskProfile::default(), &snapshot(vec![first, second]));
        assert_eq!(advice.candidates[0].agent_id, "zeta");
        assert_eq!(advice.candidates[1].agent_id, "alpha");
        assert_eq!(advice.candidates[0].cost_tier, Some(4));
        assert_eq!(advice.candidates[1].cost_tier, Some(1));
    }

    #[test]
    fn parallelism_is_advisory_and_comes_from_task_structure() {
        let task = TaskProfile {
            requires_code_edit: true,
            independent_units: Some(3),
            ownership_isolated: true,
            ..TaskProfile::default()
        };
        let advice = advise_dispatch(&task, &snapshot(vec![worker("a", "idle")]));
        assert!(advice.parallelism.worth_considering);
        assert_eq!(advice.parallelism.max_useful_lanes, 3);
        assert_eq!(advice.candidates.len(), 1);
    }

    #[test]
    fn shared_state_or_unisolated_mutation_keeps_parallelism_serial() {
        let shared = TaskProfile {
            independent_units: Some(3),
            ownership_isolated: true,
            shared_runtime_state: true,
            ..TaskProfile::default()
        };
        assert!(
            !advise_dispatch(&shared, &snapshot(vec![]))
                .parallelism
                .worth_considering
        );

        let unisolated = TaskProfile {
            requires_code_edit: true,
            independent_units: Some(3),
            ..TaskProfile::default()
        };
        assert!(
            !advise_dispatch(&unisolated, &snapshot(vec![]))
                .parallelism
                .worth_considering
        );
    }

    #[test]
    fn unspecified_task_structure_does_not_invent_parallelism_evidence() {
        let advice = advise_dispatch(&TaskProfile::default(), &snapshot(vec![]));
        assert!(!advice.parallelism.worth_considering);
        assert_eq!(advice.parallelism.max_useful_lanes, 1);
        assert_eq!(advice.parallelism.reason, "task_independence_unspecified");
    }
}
