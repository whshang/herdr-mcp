---
name: agent-dispatch
description: Select, submit, and verify useful independent coding-agent work from live worker capability/status facts while preserving explicit targets and quality requirements.
---

# Agent Dispatch

Own: `herdr_prompt`. Combine this policy with live facts from `herdr_inspect`/`herdr_since`; never encode a permanent agent/model ranking.

## Selection

1. Delegate only when independent reasoning, implementation, review, test analysis, or a capability-specific task has real value.
2. Filter live candidates by auto-dispatch policy, project/cwd, status, lane ownership, mutation conflict, and capabilities the task actually requires.
3. Select the lowest known cost/latency candidate that still meets required quality. Unknown capability fields stay unknown.
4. Submit one bounded task with explicit ownership and validation boundary, then verify delivery through prompt evidence plus live state.

A user-specified agent/model/pane target has priority and is never silently replaced. A busy preferred worker may fall back only to a reliably equivalent compatible worker; do not silently lower capability or quality.

Generic same-project reasoning may use an idle allowed worker even when optional model/edit/vision traits are unknown. A task that actually requires one of those traits must fail closed until the capability is verified.

## Boundaries and evidence

Reject blocked workers, project mismatch, conflicting mutation ownership, destructive production/runtime mutation, and middle-manager delegation. Do not invent work because a worker is idle.

Keep bounded `DispatchDecision` evidence: task profile, selected target, matched known facts, relevant rejection reason, ownership scope, and validation boundary. Uncertain submission is observed before any retry.
