# Architecture — herdr vs herdr-mcp

Audience: contributors choosing what belongs in MCP vs herdr native API.

## Two processes

| Process | Role |
|---|---|
| **herdr** | Local terminal multiplexer + agent runtime. Large Unix-socket API (`herdr api schema`, ~90 methods). |
| **herdr-mcp** | HTTP MCP façade (Streamable HTTP + OAuth) so **remote** clients can drive herdr and the workstation. |

herdr-mcp does **not** re-wrap every herdr method as an MCP tool. That burns context and duplicates the native schema.

## Default tool surface (11)

| Layer | Tools | Notes |
|---|---|---|
| Passthrough | `herdr_methods`, `herdr_call` | Reflect + call native socket methods |
| Remote orchestration | `herdr_inspect`, `herdr_since`, `herdr_prompt` | One-shot / resume / deliver prompt for chat-shaped clients |
| Remote workstation | `herdr_fs_*`, `herdr_exec` | **Not** herdr features — remote clients have no disk |

`HERDR_MCP_ALL_TOOLS=1` adds advanced/deprecated lifecycle tools (`herdr_wait`, `herdr_reap`, sessions, …). Prefer keeping them off for ChatGPT context size.

## Design rules

1. **One correct path for current product** — no speculative config for imaginary second clients.
2. **Mutations** stay under managed git roots; optional `HERDR_MCP_READONLY` / `HERDR_MCP_WRITE_ROOTS`.
3. **Delivery uncertainty** — after transport failure, do not blind-retry non-idempotent prompts; verify with inspect/since.
4. **Version is a cache key** — bump `src/version.ts` + `package.json` when tools or handshake semantics change.

## Transports

- MCP: `POST/GET/DELETE` on `/mcp` (and issuer-root `/` alias for ChatGPT probes)
- Auth: OAuth JWT (connectors) or static `HERDR_MCP_TOKEN` (Cursor/curl)
- Push (extension): `GET /push/events` SSE — same Bearer as MCP

## Related docs

- [chatgpt-connector.md](./chatgpt-connector.md) — ChatGPT OAuth + schema pitfalls
- [extension-wake.md](./extension-wake.md) — browser extension (wake ≠ MCP)
- [README.md](../README.md) — operator quick start
