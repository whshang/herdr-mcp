---
name: agent-dispatch
description: Advise, submit, and verify useful independent coding-agent work from live worker capability/status facts while preserving Web-planner choice, explicit targets, and quality requirements. Also covers host-side pre-delivery rejection, where direct Herdr tools stay primary and one bounded retry that is still blocked before reaching Herdr may fall back to an existing compatible local Agent via herdr_prompt.
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

## Host-side pre-delivery rejection

Direct Herdr tools stay the primary path. Classify a host rejection as **pre-delivery** only when the response carries none of Herdr's normal execution evidence (`op_id`, `session_id`, `backend`, `pane_id`, `exit_code`, `device_id`); never attribute it to the Herdr runtime, transport, filesystem, or remote service.

1. Allow one bounded, identical retry — same intent, and the same idempotency key for a mutation — rather than changing transport, encoding, command shape, or tool power.
2. When that retry is still rejected before delivery and the objective is genuinely suitable for autonomous local work, one **existing compatible local Agent** may act as the alternate execution subject through `herdr_prompt`. A user-specified target still wins, and the substitute must not silently lower capability or quality.
3. Send a high-level task contract instead of the rejected payload: desired outcome, allowed scope, relevant project/context, required verification, and no further delegation. Never copy, encode, obfuscate, or mechanically rephrase the rejected shell/API payload to get it past the host filter, never escalate to a stronger tool to force a rejected request through, and never bypass, disable, or evade host safety checks.
4. For a mutation, take this fallback only when the original rejection proves pre-delivery, keep every existing user authorization/confirmation and irreversible-action boundary unchanged, and do not treat Agent execution as implicit approval or an expanded scope.
5. Verify the real target state afterward through the safest available read path; an Agent report is a claim, not verification.
6. If `herdr_prompt` is itself rejected before Herdr delivery, the fallback chain stops: report that both direct execution and Agent delegation were blocked host-side instead of re-wrapping the same intent.
