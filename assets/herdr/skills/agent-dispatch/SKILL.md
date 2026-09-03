---
name: agent-dispatch
description: Advise, submit, and verify useful independent coding-agent work from live worker capability/status facts while preserving Web-planner choice, explicit targets, and quality requirements.
---

# Agent Dispatch

Own: `herdr_prompt`. Combine this policy with live facts from `herdr_inspect`/`herdr_since`; never encode a permanent agent/model ranking or infer a worker's role from its name/kind.

## Selection

1. Delegation is optional. The Web planner decides whether independent reasoning, implementation, review, test analysis, or a capability-specific task has enough value to justify it.
2. Filter live candidates by auto-dispatch policy, project/cwd, status, lane ownership, mutation conflict, and capabilities the task actually requires.
3. Expose compatible candidates with known quality/cost/latency evidence to the Web planner. Unknown capability fields stay unknown; candidate availability never requires dispatch.
4. When the planner chooses delegation, submit one bounded task with explicit ownership and validation boundary, then verify delivery through prompt evidence plus live state.
5. Record a progress checkpoint for delegated work. On the next relevant planner turn and before integration, use `herdr_since` plus the lane's Git/output evidence to confirm progress; when evidence shows drift or a stall, tighten the prompt, stop the lane, or reassign it before starting another worker.

A user-specified agent/model/pane target has priority and is never silently replaced. A busy preferred worker may fall back only to a reliably equivalent compatible worker; do not silently lower capability or quality.

Generic same-project reasoning may consider an idle allowed worker even when optional model/edit/vision traits are unknown. A task that actually requires one of those traits must fail closed until the capability is verified.

## Boundaries and evidence

Reject blocked workers, project mismatch, conflicting mutation ownership, destructive production/runtime mutation, and middle-manager delegation. Do not invent work because a worker is idle.

Keep bounded `DispatchAdvice` evidence: task profile, direct deterministic option, compatible candidates, relevant rejection reasons, optional parallelism opportunity, ownership scope, and validation boundary. A selected target exists only after the Web planner chooses one. Uncertain submission is observed before any retry.
