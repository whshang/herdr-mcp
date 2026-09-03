---
name: development-orchestration
description: Compose Herdr tools and domain Skills into dependency-aware development lanes with explicit ownership, validation, and safe worktree/workspace reclamation.
---

# Development Orchestration

Compositional Skill; owns no additional public tool.

## Topology

```text
dependent mutation                     -> serial
independent read-only investigation    -> parallel read wave/panes
independent non-overlapping mutation   -> isolated lane when useful
shared files or shared runtime/state   -> single mutation lane
```

Optimize for correct ownership and dependencies, not maximum parallelism.

Complex tasks may benefit from multiple independent lanes when decomposition is clean and expected latency/quality gains exceed orchestration overhead. This is an opportunity for the Web planner to consider, never a requirement triggered by task size or idle-worker count.

For multiple lines, reason with a lightweight lane: `objective`, `project_root`, optional branch/worktree, owner, `file_scope`, dependencies, and validation. Keep overall orchestration and cross-lane reconciliation with the Web planner.

## Mandatory invariants

- Ground the task through `workstation-control` before decomposing work; its device/workspace/history resolution rules remain authoritative.
- Apply the minimum-entity rule. Reuse an existing safe workspace/tab/pane/worktree/branch/process before creating another. A new entity must have a current functional reason: independent mutation ownership, isolation from unrelated dirty work, a genuinely long-running process, or an explicit user topology requirement.
- Parallel mutation requires independent units, non-overlapping file/runtime ownership, and no shared mutable runtime/state. Shared ownership serializes.
- Every delegated or long-running lane gets a bounded progress checkpoint. Before integration, use `herdr_since`, `herdr_exec_read`, Git diff/status, or the lane's explicit output boundary to verify useful progress. If evidence stalls or diverges from the objective, correct scope, stop/cancel, or reassign the lane before creating more work.
- Mutation completion requires Git/diff evidence plus the relevant existing tests/checks and real boundary verification. Add a new regression test only when behavior changed and existing coverage cannot detect that regression; keep it scoped to the changed behavior.
- Multi-lane mutation is cross-audited before integration. Release/production-risk changes also require an independent review when a compatible reviewer is available; otherwise the Web planner performs the independent diff/boundary review and records that reviewer availability was the constraint.
- After validation, perform one bounded completion resource sweep for resources created by the current planner. Reclaim only when no worker is active, no mutation outcome is uncertain, changes are clean or safely preserved, and branch disposition is known. A planner-created local branch may be removed only after its commits are merged/reachable from the intended integration target or it was explicitly abandoned.

## Advisory heuristics

- Split into multiple lanes only when expected latency/quality gains exceed orchestration and integration cost. Two clean independent lanes are usually better than many small lanes.
- Use an independent reviewer for ordinary single-lane work when risk, architectural impact, or ambiguity justifies the extra pass; do not spend an Agent merely to repeat deterministic checks.
- Prefer progress correction over replacement churn. Re-prompt a lane with a tighter objective when its ownership is still valid; stop or reassign it when the evidence shows the lane is pursuing the wrong solution.

## Worktrees and ownership

Create/reuse a worktree only for independent mutation, isolation from unrelated dirty work, or an explicit user requirement. Read-only investigation, grep, review, and ordinary tests stay in an existing safe checkout/pane. Never touch another task's dirty worktree.

Treat workspaces and panes as bounded reusable resources. Reuse an existing compatible workspace/pane before creating another; one canonical `herdr-mcp:utility` pane per workspace is normally sufficient. Duplicate utility panes or finished temporary lanes are resource-pressure evidence for the planner, not a reason to run an autonomous cleanup daemon.

Parallel mutations require non-overlapping file/runtime ownership. Shared files serialize unless ownership is deliberately transferred. Delegated workers receive one bounded objective and do not dispatch other workers.

## Validation and reclamation

After implementation, run the smallest relevant validation wave plus Git evidence, then reconcile all lanes before integration. For non-trivial implementation, bug fixes, reliability/refactor work, or releases, also load the `engineering-robustness` reference and apply its regression, sibling-path, and state-plane completion gates.

Reclaim a lane only when no worker is active, no mutation outcome is uncertain, changes are clean or preserved, and branch disposition is known. Reconcile Herdr workspace, Git worktree, and planner-created branch state separately. Runtime release generations are outside development-worktree cleanup.
