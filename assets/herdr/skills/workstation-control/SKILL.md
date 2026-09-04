---
name: workstation-control
description: Control live Herdr workspaces, panes, agents, incremental state, and native long-tail methods through herdr_methods, herdr_inspect, herdr_call, and herdr_since.
---

# Workstation Control

Own: `herdr_methods`, `herdr_inspect`, `herdr_call`, `herdr_since`.

## State and targets

Use `herdr_inspect` for a fresh workspace/pane/agent baseline, then reuse explicit IDs. Prefer `herdr_since(cursor)` for incremental changes. If boot identity changes or the cursor resets, discard stale incremental assumptions and resynchronize.

UI/terminal focus is observational; mutations still require the intended explicit workspace/pane/project identity. Reconnect with read-only observations before deciding whether any uncertain mutation may be retried.

## Discussion grounding across devices, workspaces, and history

Before discussing or planning prior project work when more than one device/workspace/history could plausibly match, resolve context in this order:

```text
device -> project/workspace -> continuity/history -> live Git/runtime -> requirements/planning
```

- Preserve an explicit device selector or device-aware opaque ref already supplied by the conversation. If no device is explicit, use only deterministic routing evidence; when multiple devices remain plausible, fail closed/confirm instead of choosing by name similarity, last activity, or UI focus.
- On the selected device, match the intended project root/workspace from live `herdr_inspect` evidence. Prefer explicit workspace/project identity. Do not treat a similarly named workspace, the focused workspace, or the newest workspace as equivalent.
- Resolve continuity only after device/project/workspace identity is stable enough to constrain the search. Explicit continuity ref wins; exact conversation identity is next; then search with stable project/workspace/conversation facts. Query text is distinguishing evidence, never sole auto-selection authority.
- A resumed journal is historical evidence. Refresh live Git/runtime/Agent state on the resolved device/workspace before relying on it for implementation decisions.
- Only after these identities are grounded should requirement grilling, architecture discussion, task decomposition, or mutation begin. Facts discoverable from the selected device/repository are the planner's job to read, not questions for the user.

For Edge connectivity failures, consume structured recovery metadata when present. `workstation_offline` / `workstation_reconnecting` should expose `retryable=true`, `delivery_state=not_delivered`, `retry_after_ms`, and `recovery={action:"retry_read_only_probe",probe_tool:"herdr_inspect",max_attempts:3,backoff_ms:[5000,10000,20000],...}`. Follow that bounded read-only probe schedule rather than inferring a retry policy from prose. A mutation may be reissued after recovery only when the failed result explicitly proves `not_delivered`; `delivery_unknown`, `delivered`, or a missing delivery state requires live evidence before replay.

## Native methods

Discover an unknown real Herdr method/schema once with `herdr_methods(query)`, then reuse the known live schema.

`herdr_call` has a strict split:

```text
herdr_mcp.* -> herdr-mcp local registry; unknown local method fails closed
other       -> live Herdr schema validation -> Herdr socket passthrough
```

Never assume `herdr_mcp.*` exists in Herdr core and never forward an unknown local method to its socket.

Verify control operations with the cheapest relevant live observation; request success alone does not prove the intended state transition.

## Device management and pairing

Discover registered devices and their status with `herdr_devices`.

Fleet administration belongs to enrolled devices and Worker operator credentials. Devices enrolled in the same Worker have no owner/member hierarchy. An explicitly approved Connector/WebChat is an ordinary MCP principal, not a fleet-administration channel; approval never grants permission to pair/revoke devices or administer other principals. A pre-v0.4.6 OAuth token that never went through explicit approval remains ordinary MCP compatibility only until an operator explicitly revokes its client grant.

When the user asks to add, pair, configure, or generate a setup link for a new computer, create the short-lived pairing from any already-enrolled computer with `herdr-mcp worker pair`. Never run `worker pair` on the fresh computer to discover whether a fleet exists. If this is the first Worker and no enrolled device exists, complete first-Worker Cloudflare bootstrap instead of attempting pairing. Use the default 600-second TTL unless the user requests a shorter value. Present the pairing address, the one-time 6-digit verification code, and the exact `expires_at` time together. Also give the copyable `herdr-mcp worker connect "<pairing-address>"` command. Treat the code as short-lived enrollment data; never persist it to Git, shell history, ordinary logs, or unattended automation, and enter it only at the new computer's no-echo prompt.

To permanently revoke an enrolled computer, first use `herdr_devices` to identify the exact immutable `device_id`, then run `herdr-mcp worker revoke <device-id> --confirm` on any already-enrolled computer. Never substitute a display name or pane/workspace ref. Revocation permanently fences that credential and identity; re-enrollment requires a new pairing and new device identity.

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

After resume, re-inspect live Herdr/runtime/Git facts on the resolved device/workspace before mutation. The journal records historical working context; it does not authorize stale branches, worktrees, processes, or runtime assumptions.
