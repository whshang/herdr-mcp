# ChatGPT Connector

Audience: people wiring herdr-mcp to ChatGPT, and proxies changing OAuth / Streamable HTTP.

Related entry points: [README.md](../../../README.md), `src/oauth.ts`, `src/server.ts`.

## What "connected" actually means

ChatGPT has two layers:

1. **OAuth / installed connector** — the plugin is visible in settings.
2. **Tool schema registration** — `tools/list` succeeds, and ChatGPT accepts every `inputSchema`.

It can happen that the plugin is installed in settings, but the current conversation has **0 tools**. The common cause is failure at (2), or an old conversation still holding an old tool snapshot. **Start a new conversation** after reconnecting. Installing 2–3 times repeatedly is usually caching, not necessarily a dead service.

## Public URL

- Resource URL ChatGPT uses: `{HERDR_MCP_BASE_URL}/mcp`
- Issuer / OAuth discovery: `{HERDR_MCP_BASE_URL}` (do not put a `/mcp` suffix in the environment variable)
- Default public surface: the Cloudflare Worker's `workers.dev`, no personal domain required
- When you already have a Cloudflare zone, binding a Custom Domain as a long-lived stable origin is recommended, but not a prerequisite
- Quick Tunnel / direct `cloudflared` is a legacy-migration or troubleshooting path only, no longer the default for new installs
- Changing the public origin must update the OAuth issuer/resource in sync; a normal runtime upgrade should not change this stable address

## Tool permission cards: "Allow ChatGPT to use herdr?"

This is **ChatGPT's own web approval UI**, not a herdr-mcp server-side switch.

| What you want | Reality |
|---|---|
| Server-side "auto-allow everything" | **Not possible.** The Connector web UI has no stable `require_approval: never`; that is a Responses API developer parameter, not a chatgpt.com setting |
| Click "Always allow" once and it lasts forever | Community feedback is unstable; sometimes sessions get lost / OAuth re-runs |
| Click "Allow" fewer times | Install this repo's browser extension; on **chatgpt.com** tabs it auto-clicks "Allow" on in-page permission cards (see below) |

Extension behavior (`extension/`, content script ≥ 0.1.3):

- detects in-page "Allow / Deny" tool permission cards (incl. `data-testid=tool-action-buttons`)
- **stays observing on chatgpt.com** (no longer tied only to the herdr→web "wake" 90-second window)
- clicks only visible, usable buttons whose copy is clearly allow-class; clicks on the same card only when a deny button is present (fail-closed)
- **cannot click**: native browser permission bars, non-DOM system dialogs

What the server can do is honestly annotate (e.g. `readOnlyHint`); ChatGPT currently often ignores it and may still ask about read-only tools as if they were writes.

When a card still pops on every tool call: verify the extension is loaded, the current tab is `chatgpt.com`, and the content-script version is ≥ 0.1.3 (old tabs refresh automatically after the extension reloads).

## OAuth (CIMD)

ChatGPT prefers a **Client ID Metadata Document** (`https://chatgpt.com/oauth/.../client.json`), not classic DCR secrets.

Must pass:

| Step | Expected |
|---|---|
| Protected-resource metadata | `/.well-known/oauth-protected-resource` and `.../mcp` |
| AS metadata | `/.well-known/oauth-authorization-server` (incl. the `/mcp` variant) |
| OpenID | `/.well-known/openid-configuration` (ChatGPT probes it; a 404 once broke the connection outright) |
| Authorize | PKCE auto-approval redirect |
| Token | `authorization_code` + optional `private_key_jwt`; fetch the ChatGPT JWKS |
| Access token | JWT with `aud` = resource URL |

Do not paste the static Bearer into the ChatGPT connector UI. The static token is for Cursor / curl.

Seen in production: `client_assertion` retrieval of JWKS timed out → token `400`; a retry usually works.

## MCP wire (UA `openai-mcp`)

| Rule | Reason |
|---|---|
| Fully **stateless** — no `Mcp-Session-Id` is sent | a stale sid after restart → client `-32600 Session terminated` |
| Ignore unknown sids | same as above |
| `server/discover` must succeed after OAuth | returning `-32601` sticks before `initialize` |
| discover list: **SDK version first**, keep `2026-07-28` | discovery completes; production still prefers `2025-11-25` |
| Request header `Mcp-Protocol-Version: 2026-*` → rewrite to `2025-11-25` (both `req.headers` **and** `rawHeaders`) | Hono builds the Web Request from `rawHeaders`; changing only headers is a no-op → SDK `400 Unsupported protocol version` |
| `initialize` / `tools/list` → **SSE** | the all-JSON change (0.3.6) once produced OAuth OK + initialize OK but no further `tools/list` |
| `tools/call` → JSON is fine | more stable for large payloads through tunnels |
| One-shot transport closes only on `res` finish/close | grabbing the close inside `finally` races the SDK's `_closed` → `404/-32001` |
| Do not use `-32600` for JSON-RPC auth failures | ChatGPT renders it as "Session terminated" |

ChatGPT recognition: UA `openai-mcp`, or the OAuth JWT's `client_id` / `sub` inside `chatgpt.com`.

## Schemas ChatGPT drops wholesale

One bad tool can make the **entire tool table** disappear while the connector still looks installed.

Avoid in `inputSchema`:

- `propertyNames`
- `additionalProperties: {}` (empty object; `z.record` in Zod often looks like this — use boolean `true`, a typed schema, or don't use free-form objects)
- `exclusiveMinimum` (Zod `.positive()` → use `.min(1)`)

`herdr_call.params` is advertised externally as **string** (JSON object text). At runtime, preprocess still accepts real objects.

Bump `SERVER_VERSION` / `package.json` when the tool surface or handshake changes, forcing clients to re-run `tools/list`.

**Do not trust only a cached tool count.** The runtime version comes from `/.well-known/mcp.json` / `initialize.serverInfo.version`; the current production catalog is contract epoch 2 with **18 tools including `herdr_skill`**. If ChatGPT still shows the old epoch-1 17-tool snapshot, the conversation/Connector cache is stale rather than the server intentionally hiding the skill. If fields lag (e.g. missing `herdr_fs_write.overwrite`, no `inspect.exec_sessions`):

1. confirm `mcp.json`'s `version` is the current build
2. refresh / reconnect the connector in ChatGPT
3. **start a new conversation** (old conversations lock the old `tools/list` snapshot)

Stale input fields (especially `overwrite`) cause "can create files, cannot overwrite per contract". Start a current session with `herdr_inspect`, read `herdr_skill` once, then continue with direct tools / `herdr_prompt` as needed.

## "TaskGroup" / omp is down but it says it cannot read files

Cross-check (healthy since 0.3.10+; control-plane blips may still occur intermittently on 0.3.16+ sites):

| Tool | Expected |
|---|---|
| `herdr_fs_list` / `herdr_fs_read` / `herdr_fs_grep` | `ok: true` under managed git roots |
| `herdr_exec` | prefer the utility pane for `exit_code` + `output` (`backend:utility_pane`); may be `backend:local_fallback` when TaskGroup hits before delivery; needs `confirm_busy` when the same project is working. If `delivery_uncertain`: **do not** re-send the same command, look at the pane first |
| `herdr_call` `agent.start` | a second start in the same pane can `error` — that is herdr, not fs |
| `herdr_prompt` / `herdr_call` `agent.prompt` | control-plane TaskGroup → `failure: herdr_internal` + `failure_phase: control_plane_taskgroup` (≥0.3.22, no longer just bare `UNKNOWN`); status wait timeout → `agent_status_wait_timeout`. Omit `wait` by default, include an `idempotency_key`; `herdr_since` first, then decide about re-delivery |

If all of the above pass, TaskGroup is **not** a herdr-mcp file-channel failure. Common cases:

1. "reading the project" went through `herdr_prompt` / agent tools instead of `herdr_fs_*`
2. the pane agent crashed; logs say `call=agent.*`, not `tool=herdr_fs_*`
3. `agent.start` on an occupied pane again

Access logs carry `call=<method>` on `herdr_call` (method name only). Prefer `herdr_fs_*` + `herdr_exec` for reading content.

## Orchestration: web plans, local stays cheap

| Priority | Approach | Local agent API |
|---|---|---|
| 0 | `herdr_inspect`; call `herdr_skill` once per session when the current catalog has it | none consumed |
| 1 | `herdr_fs_*` / `herdr_exec` to read/edit/search/run | none consumed |
| 2 | `herdr_prompt` → cheap/fast worker (pi, flash…), self-contained tasks | only cheap models burned |
| default forbidden | `herdr_prompt` → Claude/OMP/main to then conduct other panes | expensive model at scale |

The web model continues scheduling itself with `herdr_since` / `herdr_inspect`; the plugin's "Continue" should also push the web to re-query state instead of handing planning back to the local main agent.

## Troubleshooting checklist

1. `herdr-mcp logs -f` during the connect
2. expect: authorize → token `200` → (optional discover) → initialize → `notifications/initialized` → **`tools/list`**
3. token `200` but no initialize: discovery end / token shape / client abort
4. `tools/list` `200` but the conversation has 0 tools: schema rejected or **old conversation snapshot** → start a new conversation
5. is `/.well-known/mcp.json`'s `version` the build you think is running?
6. "cannot read files": confirm the failing call is `herdr_fs_*` / `herdr_exec`, not `herdr_call call=agent.*`
7. permission cards keep popping: is the extension on chatgpt.com, content script ≥ 0.1.3?

## The conversation stalls after ChatGPT dispatches work

The Connector only solves "ChatGPT → herdr". If a tool quickly returns "submitted" while the agent keeps running in its pane, the web conversation usually no longer auto-calls `herdr_since` / continues on its own.

Closing the loop requires the browser extension: bind that chatgpt session ↔ the working workspace; while the agent is **working**, push progress on "new summary only + 20-minute fallback"; when **settled**, inject a continue prompt and submit. See [extension-wake.md](./extension-wake.md).

Without a bound extension, this is not an MCP fault; the push-back loop is missing.

## Acceptance (real ChatGPT)

Do not rely on curl alone. Two consecutive conversation rounds with none of: Session terminated, session 400/404, `network_error`, `invalid_mcp_response`. Start a new conversation after every reconnect. When dispatching long tasks: the extension is bound to this conversation ↔ the working **workspace**, progress while working, and the web should auto-show a continue prompt after settled.