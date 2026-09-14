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

Archive a session this task created:

```sh
herdr-mcp webchat archive --session-ref SESSION_REF --expected-generation N --idempotency-key KEY
```

Continuation / handoff contract:

```text
continuity_id
  -> herdr_mcp.browser_handoff.prepare { continuity_id, source_url }   (read-only private method)
     -> automatic_delivery.params == { source_url, message, work_chain_id }
     -> manual_delivery.copy_prompt == the same message, byte-for-byte
  -> herdr_mcp.browser_session.create (params passed unchanged)
  -> the target conversation's first step is continuity.resume <continuity_id>
```

- Reuse the existing `continuity_id`; never create a second continuity chain and never treat the page as task-state authority.
- Automatic delivery and Copy Prompt must stay byte-identical; never rewrite, encode, obfuscate, switch transport, or recursively wrap a rejected payload.

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
- Pre-delivery rejection with no execution evidence: retry the original arguments at most once with the same key, then expose the Copy Prompt.
- Report the exact `session_ref` and `delivery_state`; never present "no error" or scrollback as delivery evidence.

## Current boundaries (do not document or emulate past these)

- Not supported today (runtime returns `code: "unsupported"`): `browser_space.create`, `browser_space.open`, `browser_message.append`, `browser_composer.set_reasoning`, `browser_composer.set_apps`, and `dispatch.submit` with `reasoning_effort` or `required_apps`.
- Supported private methods with **no CLI wrapper**: `browser_session.open`, `browser_dispatch.stop`, `browser_handoff.prepare`, `browser_endpoint.inspect`, `browser_space.inspect`. There is no `herdr-mcp webchat handoff` subcommand, and `webchat create` cannot pass `source_url`.
- A CLI-only agent continuing the same task should compose the message so that `continuity.resume <continuity_id>` is the first instruction, reuse the existing chain, and pass `--work-chain-id` when it already exists. Do not claim that message is the canonical handoff packet.
- `code: "caller_grant_missing"` means you are not on the trusted local path (for example a raw TCP MCP client); grants cannot be asserted over TCP. Use the CLI.
- `ego-browser` is development/UAT infrastructure, never a user dependency.

Always re-check the installed runtime before relying on a capability.
