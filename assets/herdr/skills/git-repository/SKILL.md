---
name: git-repository
description: Read deterministic Git facts and manage branch/worktree lifecycle evidence through herdr_git plus exact native lifecycle methods when required.
---

# Git Repository

Own: `herdr_git`.

Use `herdr_git` directly for status, diff, and log; do not delegate simple Git queries. Prefer bounded facts: branch/HEAD, dirty counts, changed paths, focused diff, and relevant history.

For branch/worktree lifecycle beyond `herdr_git`, discover the exact native method once with `herdr_methods`, then call it with explicit repository/worktree identity.

A development worktree represents an active mutation lane. Before create/rebase/merge/remove/reclaim, verify repository root, branch/HEAD, dirty state, active ownership, and branch disposition. Preserve dirty, unmerged, active, or ownership-unclear worktrees.

Capture enough before/after facts to prove integration affected the intended branch without discarding unrelated changes. Verify final HEAD/status and relevant diff/log relation.

`~/.config/herdr-mcp/releases/**` is runtime-generation state and never participates in development-worktree cleanup.
