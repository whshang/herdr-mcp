# ChatGPT Connector — hard-won requirements

Audience: operators wiring ChatGPT to herdr-mcp, and agents changing OAuth / Streamable HTTP.

Related entrypoints: [README.md](../README.md), `src/oauth.ts`, `src/server.ts`.

## What “connected” means

ChatGPT has **two** layers:

1. **OAuth / connector install** — plugin appears in settings.
2. **Tool schema registration** — `tools/list` succeeds *and* ChatGPT accepts every `inputSchema`.

A connector can show as installed with **0 tools** in the current chat if (2) failed or the chat still holds an old tool snapshot. **Start a new chat** after reconnecting. Re-installing 2–3 times is often ChatGPT cache, not a dead server.

## Public URL

- Resource URL ChatGPT uses: `{HERDR_MCP_BASE_URL}/mcp`
- Issuer / OAuth discovery: `{HERDR_MCP_BASE_URL}` (no `/mcp` suffix in env)
- Free default for others: Cloudflare Quick Tunnel `*.trycloudflare.com`
- After changing the public origin: restart herdr-mcp so JWT `iss` / `aud` match

## OAuth (CIMD)

ChatGPT prefers **Client ID Metadata Document** (`https://chatgpt.com/oauth/.../client.json`), not classic DCR secrets.

Must work:

| Step | Expect |
|---|---|
| Protected-resource metadata | `/.well-known/oauth-protected-resource` and `.../mcp` |
| AS metadata | `/.well-known/oauth-authorization-server` (+ path variants under `/mcp`) |
| OpenID | `/.well-known/openid-configuration` (ChatGPT probes this; 404 historically aborted connect) |
| Authorize | Auto-approve redirect with PKCE |
| Token | `authorization_code` + optional `private_key_jwt` (`client_assertion`); fetch ChatGPT JWKS |
| Access token | JWT, `aud` = resource URL |

Do **not** paste a static Bearer into the ChatGPT connector UI. Static token is for Cursor / curl.

Transient failure seen in production: `client_assertion` JWKS fetch timeout → token `400`; retry usually works.

## MCP wire (openai-mcp UA)

Observed UA: `openai-mcp/1.0.0`.

| Rule | Why |
|---|---|
| Fully **stateless** — never issue `Mcp-Session-Id` | Stale sid after restart → client `-32600 Session terminated` |
| Ignore / skip unknown sid | Same |
| `server/discover` must succeed post-OAuth | Returning `-32601` stopped ChatGPT before `initialize` |
| Advertise SDK versions **first**, keep `2026-07-28` in the list | Discover completes; prefer `2025-11-25` wire |
| Rewrite request header `Mcp-Protocol-Version: 2026-*` → `2025-11-25` in **both** `req.headers` and `rawHeaders` | Hono builds Web Request from `rawHeaders`; headers-only patch is a no-op → SDK `400 Unsupported protocol version` |
| `initialize` / `tools/list` → **SSE** (`text/event-stream`) | Forcing JSON on all POSTs (0.3.6) correlated with OAuth OK + initialize OK + **no** follow-up `tools/list` |
| `tools/call` → JSON OK | Long tool payloads through tunnels |
| Close throwaway transports on `res` finish/close only | Eager `finally` close races SDK `_closed` → `404/-32001` |
| Auth failure JSON-RPC code ≠ `-32600` | ChatGPT surfaces `-32600` as “Session terminated” |

Detect ChatGPT without relying on initialize `clientInfo`: UA `openai-mcp` **or** OAuth JWT `client_id` / `sub` on `chatgpt.com`.

## Tool schemas ChatGPT rejects

One bad tool can drop the **entire** catalog while the connector still looks installed.

Avoid in `inputSchema`:

- `propertyNames`
- `additionalProperties: {}` (empty object; Zod `z.record` / catchall often emits this — want boolean `true` or a typed schema, or avoid free-form objects)
- `exclusiveMinimum` (Zod `.positive()` → use `.min(1)` instead)

`herdr_call.params` is advertised as a **string** (JSON object text). Runtime still accepts a real object via preprocess.

Bump `SERVER_VERSION` / `package.json` / mcp.json identity when the tool surface or handshake changes so clients re-list.

## “TaskGroup” / omp exited while reading files

Observed ChatGPT narration: workspace control OK, but “file read / command channel” returns a server **TaskGroup** error; newly started `omp` does not stay up.

Cross-check on a healthy 0.3.9+ build:

| Tool | Expected |
|---|---|
| `herdr_fs_list` / `herdr_fs_read` / `herdr_fs_grep` | `ok: true` for paths under a managed git root |
| `herdr_exec` | exit code + output from a workspace utility pane |
| `herdr_call` `agent.start` | may `error` on a second start of the same pane — that is herdr, not fs |

If those four succeed locally with the same Bearer, the TaskGroup string is **not** a herdr-mcp filesystem bug. Typical causes:

1. ChatGPT routed “read the project” through `herdr_prompt` / an agent tool instead of `herdr_fs_*`
2. A pane agent crashed; logs show `call=agent.*` not `tool=herdr_fs_*`
3. A second `agent.start` on an occupied pane

Access logs now include `call=<method>` on `herdr_call` (method name only). Prefer `herdr_fs_*` + `herdr_exec` for content; use agents for coding work.

## Debugging checklist

1. `herdr-mcp logs -f` during connect
2. Expect: authorize → token `200` → (optional discover) → initialize → `notifications/initialized` → **`tools/list`**
3. If token `200` but no initialize: discovery / token shape / client abort
4. If `tools/list` `200` but chat shows 0 tools: schema rejection or **old chat snapshot** → new chat
5. Confirm `/.well-known/mcp.json` `version` matches the build you think is running
6. For “cannot read files”: confirm the failing call is `herdr_fs_*` / `herdr_exec`, not `herdr_call call=agent.*`

## Acceptance (real ChatGPT)

Not curl. Two consecutive rounds with zero of: Session terminated, session 400/404, `network_error`, `invalid_mcp_response`. New chat after every reconnect.
