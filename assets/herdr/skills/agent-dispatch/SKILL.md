---
name: agent-dispatch
description: Decide when independent coding-agent reasoning is useful and safely select, target, submit, and verify a live compatible worker through herdr_prompt and current Herdr capability/status facts.
---

# Agent Dispatch

Primarily own:

```text
herdr_prompt
```

Combine this policy with live workstation/capability facts from `herdr_inspect` or `herdr_since`. Do not encode a permanent worker/model ranking in this Skill.

## Decision order

1. If a deterministic native tool can complete the task correctly, use it directly.
2. Delegate only when independent reasoning, implementation, review, testing analysis, or a capability-specific task has real value.
3. Read current worker status and reliable capability metadata.
4. Filter candidates by automatic-dispatch policy, project/cwd compatibility, status, lane ownership, mutation conflict, and every capability the task truly requires.
5. Select the lowest-cost/latency candidate that still meets the required quality according to reliable facts. Unknown capability fields stay unknown; do not invent them.
6. Submit one self-contained task with explicit target, ownership scope, and validation boundary.
7. Verify delivery with prompt evidence plus live `since`/`inspect` state. An uncertain submission is never blindly repeated.

## Target rules

An explicit user-specified agent/model/pane target has priority. Do not silently replace it.

If a preferred worker is busy, an equivalent compatible worker may be selected automatically when reliable facts support equivalence. When only a material quality/capability downgrade is available, do not silently downgrade.

Reject blocked workers, project/cwd mismatches, conflicting mutation ownership, and tasks requiring capabilities that are known to be absent. Treat unknown facts conservatively when they are required for safety or correctness.

## Delegation boundaries

Good delegation targets include independent review, bounded implementation, non-conflicting validation, and clearly isolated mutation lanes.

Do not manufacture work merely because a worker is idle. Do not auto-delegate destructive production/runtime mutation. Do not let a worker become a middle manager for other workers; the Web planner retains orchestration ownership.

## Evidence

Keep a bounded `DispatchDecision`: task profile, selected target, known matched facts, rejected blockers when useful, ownership scope, reason, and validation boundary. Completion requires both delivery evidence and the task's own file/Git/test/review evidence.
