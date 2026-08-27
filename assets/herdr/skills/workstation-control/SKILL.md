---
name: workstation-control
description: Control and inspect live Herdr workspaces, tabs, panes, agents, incremental events, and native long-tail methods through herdr_methods, herdr_inspect, herdr_call, and herdr_since.
---

# Workstation Control

Own these public tools:

```text
herdr_methods
herdr_inspect
herdr_call
herdr_since
```

## State model

- Treat live Herdr state as authoritative. A handoff or earlier inspect is only a prior observation.
- Use `herdr_inspect` when a task needs a fresh workspace/pane/agent/runtime baseline. Reuse explicit IDs from that result.
- After a baseline, prefer `herdr_since(cursor)` for incremental workspace/agent changes. If the boot identity changes or the cursor resets, discard stale incremental assumptions and resynchronize.
- UI focus and terminal focus are observations. They do not change the explicit control target supplied to a mutation.
- Reconnect with read-only observations. Never replay an uncertain mutation merely because the control connection failed.

## Native method discovery

Use `herdr_methods(query)` only when the true Herdr method/schema is unknown. Reuse a known live schema during the current task instead of rediscovering it before every call.

`herdr_call` has two namespaces:

```text
herdr_mcp.*
  -> validated herdr-mcp local method registry
  -> execute locally
  -> unknown local method fails closed

all other methods
  -> validate against the live Herdr schema
  -> passthrough to the Herdr socket
```

Never assume `herdr_mcp.*` exists in Herdr core, and never forward an unknown `herdr_mcp.*` method to the Herdr socket.

## Target discipline

Prefer exact `workspace_id`, `pane_id`, project root, and session identity over labels or implicit focus. Before mutating a target that may have changed, verify it still resolves to the intended project and ownership lane.

## Completion evidence

For control-plane operations, verify the resulting workspace/pane/agent state with the cheapest relevant live observation. A submitted command, prompt, or focus change is not complete solely because the request returned successfully.
