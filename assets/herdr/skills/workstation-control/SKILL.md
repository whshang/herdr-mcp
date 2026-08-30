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

## Conversation continuity recovery

When a fresh or uncertain conversation contains prior-work intent such as “continue”, “resume”, “接着”, or “继续上次”, search durable continuity before asking the user for an internal ID. Generic continuity wording is a trigger only; it is never enough evidence to choose a chain.

Use the existing `herdr_call` surface; this adds no public MCP tool:

```text
herdr_call(method="continuity.resume", params={"continuity_id":"hc:..."})
herdr_call(method="continuity.resolve", params={"conversation_id":"..."})
herdr_call(method="continuity.search", params={"project_id":"...","workspace_id":"...","query":"distinguishing terms"})
```

Resolution order is explicit continuity reference -> exact conversation resolve -> search with stable project/workspace/conversation identity -> optional user-supplied distinguishing text. `continuity.search` may return bounded title/workspace/update-time and recent-turn excerpts for confirmation.

- `resolution=unique_exact` with `auto_resume_safe=true`: exactly one active chain matched at least one stable identity hint; resume that ID automatically.
- `resolution=confirmation_required`: never choose by recency or textual similarity. Present the bounded candidates and ask which prior work chain the user means, then resume exactly the confirmed ID. A query-only match remains confirmation-required even when it returns one candidate.
- `resolution=none`: do not invent an ID; ask for a distinguishing detail or treat the request as fresh work when the user confirms that intent.

After resume, re-inspect live Herdr/runtime/Git facts before mutation. The journal records historical working context; it does not authorize stale branches, worktrees, processes, or runtime assumptions.
