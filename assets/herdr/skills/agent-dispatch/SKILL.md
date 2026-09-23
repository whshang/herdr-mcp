---
name: agent-dispatch
description: Advise, submit, and verify useful independent coding-agent work from live worker capability/status facts while preserving Web-planner choice, explicit targets, and quality requirements.
---

# Agent Dispatch

Own: `herdr_prompt`. Combine this policy with live facts from `herdr_inspect`/`herdr_since`; never encode a permanent agent/model ranking or infer a worker's role from its name/kind.

Call shape: target one explicit live Agent name or pane identity and use one stable `idempotency_key` for the intended prompt submission. Normal orchestration is asynchronous; retain the returned task/dispatch identity instead of blocking on a global Agent state. After timeout or transport failure, retry only when delivery evidence says `not_delivered` or live state proves the prompt did not apply.

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

If native workers are unavailable or unsuitable, a configured headless external harness may be used only as a bounded fallback for a self-contained task. Give it an explicit checkpoint and verify Git/files/tests afterward; process exit alone is not completion evidence. Interactive fallback remains human-operated.

`agent_not_found`, `unknown_agent`, or an unbound pane is a **pre-dispatch condition**, not a human boundary. When prompt evidence says `delivery_state=not_delivered` and `requires_human=false`, do not stop. Re-read `herdr_inspect`, use deterministic `startable_candidates` / `agent_lifecycle`, and choose a compatible Agent kind. Reuse a pane only when this task already owns and has verified that free shell pane; otherwise create a task-owned pane with `pane.split`, start the Agent with `agent.start`, wait for it to become ready, then re-prompt with the same idempotency key. `agent:null` proves only that no Agent is bound; it never proves planner ownership. Reclaim only panes this task created, and only after the worker is terminal, delivery is certain, and its output/Git evidence is captured.

When `startable_candidates.evidence_gap.present=true`, an installed Agent exists but a required capability is not yet verified. Keep the capability gate fail-closed, run the advertised capability refresh, and re-plan; never rewrite “unverified” as “Agent unavailable” or “Herdr unavailable”.

## External host outcome

A response with no Herdr execution identity or result fields is not evidence that the workstation, child process, filesystem, or remote service executed anything. When Herdr result fields are present, use those fields to describe the local operation. Host policy is external to Agent Dispatch and does not change worker selection or task ownership.
