# Herdr 0.8.2 Git-discovery bootstrap reproduction

Date: 2026-08-29

This is bounded evidence for the Herdr upstream issue referenced by the v0.4.2 control-plane reliability gate. It intentionally uses isolated state and never mutates a saved user session.

## Result

With the same Herdr 0.8.2 macOS arm64 binary and a fresh isolated `XDG_CONFIG_HOME`:

| Case | Server readiness | `workspace create` | Cleanup |
| --- | --- | --- | --- |
| Git worktree cwd | ready after ~1s | exceeded 10s budget and had to be terminated | exact workspace PID and exact server PID reaped |
| fresh non-Git `/tmp` cwd | ready after ~1s | completed successfully; focused workspace returned | exact server PID terminated and reaped |

The A/B difference isolates the failure to the project/Git discovery path strongly enough to keep the herdr-mcp release fixture independent of Git metadata. It does not prove which Herdr Git sub-probe blocks internally.

## herdr-mcp mitigation

- Project/Git discovery performed by herdr-mcp remains bounded per workspace.
- Git children run in owned process groups and are synchronously reaped after timeout.
- The CI Herdr fixture creates its focused bootstrap workspace in an isolated non-Git directory while tests themselves continue to execute from the repository checkout.
- A Git metadata failure is treated as degraded project metadata, not transport health.

## Upstream behavior required

Herdr itself should bound every Git identity/metadata discovery operation during workspace creation and persisted-session restore. Failure in one workspace should degrade only that workspace's Git metadata and must not block creation/restoration of unrelated workspaces.

A future Herdr build is considered fixed only when the Git-worktree case above completes within a bounded budget without destructive session recovery.
