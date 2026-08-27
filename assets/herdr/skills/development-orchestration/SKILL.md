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

For multiple lines, reason with a lightweight lane: `objective`, `project_root`, optional branch/worktree, owner, `file_scope`, dependencies, and validation. Keep overall orchestration and cross-lane reconciliation with the Web planner.

## Worktrees and ownership

Create/reuse a worktree only for independent mutation, isolation from unrelated dirty work, or an explicit user requirement. Read-only investigation, grep, review, and ordinary tests stay in an existing safe checkout/pane. Never touch another task's dirty worktree.

Parallel mutations require non-overlapping file/runtime ownership. Shared files serialize unless ownership is deliberately transferred. Delegated workers receive one bounded objective and do not dispatch other workers.

## Validation and reclamation

After implementation, run the smallest relevant validation wave plus Git evidence, then reconcile all lanes before integration.

Reclaim a lane only when no worker is active, no mutation outcome is uncertain, changes are clean or preserved, and branch disposition is known. Reconcile Herdr workspace and Git worktree state separately. Runtime release generations are outside development-worktree cleanup.
