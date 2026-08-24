# JSON → MCP bridge

Audience: people using DeepSeek / z.ai web (which has no MCP Connector) to hit the local herdr-mcp via the agreed JSON.

Overview and both tracks: [extension.md](./extension.md). Progress push-back: [extension-wake.md](./extension-wake.md).

## Goal

**Peer** with track A: on sites without a connector, the assistant's tool-call JSON → the extension `POST http://127.0.0.1:8772/mcp` → results backfilled into the same conversation.

First batch: `chat.deepseek.com`, `chat.z.ai`.

## Current state

| Capability | Status |
|---|---|
| SpeaksJSON extracts `{"tool":"...","args":{}}` | **done** (DeepSeek / z.ai content scripts) |
| background `tools/call` + result backfill into the same conversation | **not done** |
| Options allowlist | **not done** |
| permission-card auto-allow (chatgpt) | auxiliary capability of the other track; folded into Project automation and requires per-Project automation enabled in Options plus current Project HUD `Auto on` |

## Three stages

### A — Protocol skeleton

```json
{"tool":"herdr_inspect","args":{}}
```

Streaming unterminated output is not parsed; after completion, extract → background calls MCP → backfill.  
Default read-only allowlist: `herdr_inspect` / `herdr_methods` / `herdr_since` / `herdr_fs_read|list|grep`.

### B — Capability surface

Options turn on `herdr_exec`, file writes, `herdr_prompt`, etc.; default stays off.

### C — Full MCP surface

Align with the ChatGPT default 18 tools (local only, never through the public internet).

## Cooperation with track A

DeepSeek can simultaneously: track B calls MCP to dispatch work + track A binds the same pane for progress/done push-back.  
The two switches are independent.

## Implementation order (closed loop not yet open)

The parsing layer already lives in `extension/content/webmcp/speaks-json.js`. Still missing:

1. background `mcpCall(tool, args)` (`POST http://127.0.0.1:8772/mcp`)
2. SpeaksJSON serially executes and backfills after completion
3. Options allowlist
4. `extension_smoke` adds a bridge case