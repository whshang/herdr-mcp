---
name: development-orchestration
description: Compose Herdr tools and domain Skills into dependency-aware serial or parallel development lanes with explicit ownership, validation, reconciliation, and safe worktree/workspace reclamation.
---

# Development Orchestration

This is a compositional Skill. It owns no additional public MCP tool.

## Topology before concurrency

Choose the topology from dependencies and ownership:

```text
dependent mutation
  -> serial

independent read-only investigation
  -> parallel read wave or panes

independent non-overlapping mutation
  -> separate branch/worktree lane only when isolation is useful

shared files or shared runtime/production state
  -> single mutation lane or explicit serialized ownership
```

The goal is correct topology, not maximum parallelism.

## Lane model

Reason with a lightweight lane descriptor when multiple lines exist:

```text
objective
project_root
branch/worktree when needed
owner_agent when delegated
file_scope
upstream dependencies
validation
```

Do not create a persistent Task Center for this model. The Web planner retains the overall plan and cross-lane reconciliation responsibility.

## Worktree decisions

Create or reuse a worktree only for independent mutation, isolation from unrelated dirty work, or an explicit user requirement. Read-only investigation, grep, review, and ordinary test execution stay in an existing safe checkout/pane.

Before a new worktree, reconcile existing lanes and reuse a suitable clean one when ownership is clear. Never touch another task's dirty worktree.

## Parallel work

Parallelize independent reads freely when the client permits. Parallel mutations require non-overlapping file/runtime ownership and an explicit lane boundary. Shared files serialize unless ownership is transferred deliberately.

Delegated workers receive a bounded objective and file/validation ownership. Workers do not dispatch other workers or take over global orchestration.

## Validation and reconciliation

After implementation, form a validation wave from the smallest relevant checks, Git diff/status, and any required runtime/client smoke. Reconcile results across lanes before integration.

A lane can be reclaimed only when no worker is active, no mutation outcome is uncertain, changes are clean or preserved, and branch disposition is known. Reconcile Herdr workspace state and Git worktree state separately; close/remove only the resources that deterministic evidence says are complete.

Production runtime generations are outside development worktree cleanup.
