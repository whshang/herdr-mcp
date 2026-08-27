---
name: git-repository
description: Read and reason about deterministic repository status, diff, log, branch, and worktree facts through herdr_git and native Git lifecycle methods when needed.
---

# Git Repository

Own this public tool:

```text
herdr_git
```

## Deterministic Git facts

Use `herdr_git` directly for status, diff, and log. Do not delegate simple Git queries to a coding agent.

Prefer compact facts first: branch/HEAD, dirty counts, changed paths, focused diff, and bounded history. Narrow to a specific path when a repository-wide diff is larger than the decision requires.

## Branch and worktree lifecycle

When branch/worktree operations are required beyond `herdr_git`, discover the exact live native method once through `herdr_methods`, then use `herdr_call` with explicit repository/worktree identity.

A worktree represents an active independent mutation lane. Read-only investigation, review, grep, and ordinary tests do not require a new worktree.

Before creating, rebasing, merging, removing, or reclaiming a worktree, verify:

- repository root and current branch/HEAD;
- dirty state;
- active agent/lane ownership;
- whether the branch is merged, still active, or explicitly abandoned.

Preserve dirty, unmerged, active, or ownership-unclear worktrees.

## Merge/rebase evidence

Capture enough before/after Git facts to prove the operation affected the intended branch and did not discard unrelated changes. After integration, verify HEAD, status, and the relevant diff/log relationship.

Runtime release generations under `~/.config/herdr-mcp/releases/**` are not development worktrees and never participate in development worktree cleanup.
