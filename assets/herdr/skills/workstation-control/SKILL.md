---
name: workstation-control
description: Control live Herdr workspaces, panes, agents, incremental state, and native long-tail methods through herdr_methods, herdr_inspect, herdr_call, and herdr_since.
---

# Workstation Control

Own: `herdr_methods`, `herdr_inspect`, `herdr_call`, `herdr_since`.

## State and targets

Use `herdr_inspect` for a fresh workspace/pane/agent baseline, then reuse explicit IDs. Prefer `herdr_since(cursor)` for incremental changes. If boot identity changes or the cursor resets, discard stale incremental assumptions and resynchronize.

UI/terminal focus is observational; mutations still require the intended explicit workspace/pane/project identity. Reconnect with read-only observations before deciding whether any uncertain mutation may be retried.

## Native methods

Discover an unknown real Herdr method/schema once with `herdr_methods(query)`, then reuse the known live schema.

`herdr_call` has a strict split:

```text
herdr_mcp.* -> herdr-mcp local registry; unknown local method fails closed
other       -> live Herdr schema validation -> Herdr socket passthrough
```

Never assume `herdr_mcp.*` exists in Herdr core and never forward an unknown local method to its socket.

Verify control operations with the cheapest relevant live observation; request success alone does not prove the intended state transition.
