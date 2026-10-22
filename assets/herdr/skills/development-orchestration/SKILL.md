---
name: development-orchestration
description: Compose Herdr tools and domain Skills into dependency-aware development lanes with explicit ownership, validation, and safe worktree/workspace reclamation.
---

# Development Orchestration

Compositional Skill; owns no additional public tool.

Five-beat execution cadence: `如无必要勿增实体 → 规划拆分多线并行 → 进度监控方向纠偏 → 交叉审计回归验证 → 分支清理窗格回收`. The Required/Advisory rules below define its exact semantics.

## Topology

```text
dependent mutation                     -> serial
independent read-only investigation    -> parallel read wave/panes
independent non-overlapping mutation   -> isolated lane when useful
shared files or shared runtime/state   -> single mutation lane
```

Optimize for correct ownership and dependencies, not maximum parallelism.

The Web planner is the main brain: it decomposes, owns branches/worktrees/panes, merges, integrates, verifies, and cleans up, and delegates non-trivial implementation (more than roughly one file or ~30 changed lines, or work needing iterative test/fix rounds) to a compatible local coder agent in its own worktree. Small deterministic edits, reads, git/branch management, and final verification may stay direct; this is a scheduling heuristic, not a gate. Multiple independent lanes are worth it only when decomposition is clean and the expected latency/quality gain exceeds orchestration overhead — never because the task is large or a worker is idle.

For multiple lines, reason with a lightweight lane: `objective`, `project_root`, optional branch/worktree, owner, `file_scope`, dependencies, and validation. Keep overall orchestration and cross-lane reconciliation with the Web planner.

## Mandatory invariants

- Ground the task through `workstation-control` before decomposing work; its device/workspace/history resolution rules remain authoritative.
- Apply the minimum-entity rule. Reuse an existing safe workspace/tab/pane/worktree/branch/process before creating another. A new entity must have a current functional reason: independent mutation ownership, isolation from unrelated dirty work, a genuinely long-running process, or an explicit user topology requirement.
- Parallel mutation requires independent units, non-overlapping file/runtime ownership, and no shared mutable runtime/state. Shared ownership serializes.
- Every delegated or long-running lane gets a bounded progress checkpoint. Before integration, use `herdr_since`, `herdr_exec_read`, Git diff/status, or the lane's explicit output boundary to verify useful progress. If evidence stalls or diverges from the objective, correct scope, stop/cancel, or reassign the lane before creating more work.
- Mutation completion requires Git/diff evidence plus the relevant existing tests/checks and real boundary verification. Add a new regression test only when behavior changed and existing coverage cannot detect that regression; keep it scoped to the changed behavior.
- Multi-lane mutation is cross-audited before integration. A reviewer runs in a fresh, read-only context on the latest diff (re-fetch the head) and returns an explicit `PASS`/`BLOCK` with actionable findings only. The planner still reads the full diff and validation output before integration; a verdict alone is not acceptance. Release/production-risk changes also require an independent review when a compatible reviewer is available; otherwise the planner performs the independent diff/boundary review and records that reviewer availability was the constraint.
- After validation, perform one bounded completion resource sweep for resources created by the current planner. Reclaim only when no worker is active, no mutation outcome is uncertain, changes are clean or safely preserved, and branch disposition is known. A planner-created local branch may be removed only after its commits are merged/reachable from the intended integration target or it was explicitly abandoned.

## Advisory heuristics

- Use an independent reviewer for ordinary single-lane work when risk, architectural impact, or ambiguity justifies the extra pass; do not spend an Agent merely to repeat deterministic checks.
- Prefer progress correction over replacement churn. Re-prompt a lane with a tighter objective when its ownership is still valid; stop or reassign it when the evidence shows the lane is pursuing the wrong solution.

## Supervision triggers

On a mechanical trigger — never self-judged confidence — stop iterating and consult an independent advisor with a clean, read-only context: the same lane used 3 repair rounds; two consecutive gate rounds each reporting new failures; a reviewer `BLOCK` twice on the same lane; or an evidence-confirmed stall. The advisor card has three parts: mechanical facts, the planner's current judgment, and the answer the planner hopes to hear, offered so it can be challenged. The advisor may recommend abandoning the approach. Never auto-redispatch a lane whose outcome is uncertain.

## Worktrees and ownership

Create/reuse a worktree only for independent mutation, isolation from unrelated dirty work, or an explicit user requirement. Read-only investigation, grep, review, and ordinary tests stay in an existing safe checkout/pane. Never touch another task's dirty worktree.

Treat workspaces and panes as bounded reusable resources. Reuse an existing compatible workspace/pane before creating another; one canonical `herdr-mcp:utility` pane per workspace is normally sufficient. Duplicate utility panes or finished temporary lanes are resource-pressure evidence for the planner, not a reason to run an autonomous cleanup daemon.

Parallel mutations require non-overlapping file/runtime ownership. Shared files serialize unless ownership is deliberately transferred. Delegated workers receive one bounded objective and do not dispatch other workers.

## Validation and reclamation

After implementation, run the smallest relevant validation wave plus Git evidence, then reconcile all lanes before integration. For non-trivial implementation, bug fixes, reliability/refactor work, or releases, also load the `engineering-robustness` reference and apply its regression, sibling-path, and state-plane completion gates.

Reclaim a lane only when no worker is active, no mutation outcome is uncertain, changes are clean or preserved, and branch disposition is known. Reconcile Herdr workspace, Git worktree, and planner-created branch state separately. Runtime release generations are outside development-worktree cleanup.

Record errors, fixes, and validation evidence in the project's existing work log (for example `HANDOFF.md` mistakes or a project findings file). A lesson that can be enforced mechanically becomes a test, lint, or script; demonstrate that a new gate fails on a known-bad input before trusting it.

## Herdr-MCP development-only retrospective

When the target project is Herdr-MCP itself, finish non-trivial development/debug/release work with one bounded call-pattern review after the user's acceptance checks are complete. Report at most three concrete latency/reliability improvements supported by this task's actual calls. Do not start another optimization lane merely because the retrospective found an idea, and do not load this retrospective for ordinary Herdr-MCP use outside repository development.
