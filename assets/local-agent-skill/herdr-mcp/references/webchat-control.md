# WebChat / ChatGPT browser control

Use this when the task involves a **web conversation**: create a new ChatGPT/WebChat chat, continue in another conversation, dispatch to a Web AI, resume a browser conversation, hand off the current task, or check whether a session is still alive.

Web AI is not the only caller of Herdr's browser control plane. A local coding agent uses the same plane through the `herdr-mcp` CLI. Never replace it with Playwright, Selenium, AppleScript, DOM injection, or a second browser automation stack: those paths cannot preserve identity, idempotency, or delivery evidence.

Full manual: `docs/i18n/en/local-agent-webchat-control.md` (zh-CN and ja variants beside it).

## Capability discovery first

```sh
herdr-mcp webchat endpoints --limit 5
herdr-mcp webchat resources [--endpoint-ref REF] [--provider chatgpt] [--kind account|space|session] [--parent-ref REF] [--limit N]
herdr-mcp webchat inspect RESOURCE_REF
```

- `endpoint_ref` must report `consent.webchat_control: true`, otherwise the browser is not drivable.
- Hierarchy is account → `space` (ChatGPT Project) → `session` (conversation); `parent_ref` gives the edge.
- `resource_ref`, `endpoint_ref`, and `observation_generation` are the only valid identities. Never synthesize, transform, or guess a ref, and never reuse one from another machine or Project.
- Pass the observed generation as `--expected-generation`.
- `herdr-mcp webchat resources` reports `actuation_evaluated: false` / `actuation_reason: not_evaluated_by_list`; a list result is not a consent or health signal. The `actuation_available` value from `inspect` describes that inspect call, not a mutation preflight.

## Workflows

Find an existing session:

```sh
herdr-mcp webchat resources --kind space
herdr-mcp webchat resources --kind session --parent-ref SPACE_REF
herdr-mcp webchat inspect SESSION_REF
```

Create a conversation inside an existing Project:

```sh
herdr-mcp webchat create \
  --endpoint-ref ENDPOINT_REF --provider chatgpt --account-ref ACCOUNT_REF \
  --space-ref SPACE_REF --display-label LABEL --message MESSAGE \
  --expected-generation N --idempotency-key KEY [--work-chain-id WORK_CHAIN_ID]
```

Dispatch and observe:

```sh
herdr-mcp webchat send --session-ref SESSION_REF --message MESSAGE \
  --expected-generation N --idempotency-key KEY [--work-chain-id WORK_CHAIN_ID]
herdr-mcp webchat dispatch-status DISPATCH_ID
```

Open an existing session without submitting a message:

```sh
herdr-mcp webchat open --session-ref SESSION_REF --expected-generation N --idempotency-key KEY
```

Archive a session this task created:

```sh
herdr-mcp webchat archive --session-ref SESSION_REF --expected-generation N --idempotency-key KEY
```

Reconcile provider archive state without clicking Archive:

```sh
herdr-mcp webchat archive-status --session-ref SESSION_REF --expected-generation N
```

`archive-status` is read-only provider readback and returns `archive_state=archived|active|unknown`. After an `archive` result of `uncertain`, never replay the archive mutation blindly. Reopen the exact registered session if needed, run `archive-status`, and act only on that evidence. If it reports `archived`, a later `archive` convergence call is safe for view cleanup because the browser adapter first rechecks provider state and returns applied without a second Archive click. If it reports `active`, the previous uncertain attempt did not leave the provider archived; a new archive mutation is then a new evidence-backed attempt. If it reports `unknown`, stop and keep the outcome unresolved.

Continuation / handoff contract:

```sh
herdr-mcp webchat handoff --continuity-id HC --source-url URL \
  [--objective TEXT] [--work-chain-id ID] [--handoff-id ID] \
  [--idempotency-key KEY] [--prepare-only]
```

`webchat handoff` is the canonical continuation path for local agents: it requests the canonical packet from the existing `herdr_mcp.browser_handoff.prepare`, then hands that packet's own `automatic_delivery.params` to the source-anchored `browser_session.create` unchanged.

```text
continuity_id
  -> herdr_mcp.browser_handoff.prepare { continuity_id, source_url }   (read-only private method)
     -> automatic_delivery.params == { source_url, message, work_chain_id }
     -> manual_delivery.copy_prompt == the same message, byte-for-byte
  -> herdr_mcp.browser_session.create (params passed unchanged, CLI-added idempotency key)
  -> the target conversation's first step is continuity.resume <continuity_id>
```

- Output is the canonical packet plus `automatic_delivery{attempted, completed, delivery_state, reason, replayed, session_ref, dispatch_id, result}` and an `instruction`.
- `--source-url` is the audit anchor and the only route input: the runtime resolves endpoint/account/Project from the registered conversation. Pass the exact conversation URL.
- Reuse the existing `continuity_id`; never create a second continuity chain and never treat the page as task-state authority.
- Automatic delivery and Copy Prompt must stay byte-identical; never compose your own continuation prompt and call it canonical.
- Without `--idempotency-key` the CLI reuses the canonical `handoff_id`, so re-running the same command is the same logical handoff. Never pass a new key to retry.
- `automatic_delivery.completed=false` means automatic delivery is not confirmed complete; interpret `delivery_state` rather than collapsing states. `not_applied`, `browser_offline`, `resource_unavailable`, or a missing source route may use the prepared manual path only when the returned evidence proves no delivery occurred. `uncertain` requires `webchat dispatch-status` / resource re-observation and must not expose or submit the Copy Prompt until reconciliation proves the first delivery did not apply.

## Mutation safety

| State | Action |
| --- | --- |
| `applied` | Durable; continue with the dispatch/evidence identity |
| `not_applied` | Nothing delivered; a retry is a deliberate decision, not a loop |
| `uncertain` | Re-observe (`inspect`, `dispatch-status`) first; never blind-retry |
| `browser_offline` / `resource_unavailable` | Wait and re-observe; never rotate the idempotency key to probe liveness |
| `rejected` | Host refused it; do not rewrite the payload to work around it |
| `stopped` | Deliberate outcome, not a failure |

- One intended mutation = one idempotency key. A replay returns the recorded dispatch (`replayed: true`) instead of acting twice.
- If automatic delivery returns no Herdr execution or result fields, no workstation execution can be inferred from that outcome; use the prepared Copy Prompt or re-observe the exact dispatch when one exists. If Herdr reports uncertain delivery, reconcile it before any later mutation.
- Report the exact `session_ref` and `delivery_state`; never present "no error" or scrollback as delivery evidence.

## Current boundaries (do not document or emulate past these)

- Not supported today (runtime returns `code: "unsupported"`): `browser_space.create`, `browser_space.open`, `browser_message.append`, `browser_composer.set_reasoning`, `browser_composer.set_apps`, and `dispatch.submit` with `reasoning_effort` or `required_apps`.
- Supported private methods with **no CLI wrapper**: `browser_dispatch.stop`, `browser_endpoint.inspect`, `browser_space.inspect`, and the trusted-local read-only source resolver used internally by the CLI. `browser_session.open` is available through `herdr-mcp webchat open`; `browser_handoff.prepare` is reached through `herdr-mcp webchat handoff`. `webchat create --source-url URL` is available for exact source-window affinity and requires only the canonical source URL plus the normal message/idempotency inputs; the trusted local CLI resolves the latest exact endpoint/account route and attaches that grant internally, while Runtime resolves and validates the same canonical route again before mutation.
- `code: "caller_grant_missing"` means you are not on the trusted local path (for example a raw TCP MCP client); grants cannot be asserted over TCP. Use the CLI.
- `ego-browser` is development/UAT infrastructure, never a user dependency.

Always re-check the installed runtime before relying on a capability.
