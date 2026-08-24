# JSON → MCP bridge

Audience: people using DeepSeek / z.ai web, which do not expose a ChatGPT-style MCP Connector, to drive the local herdr-mcp runtime safely through the browser extension.

Overview and continuity track: [extension.md](./extension.md). Progress push-back and handoff: [extension-wake.md](./extension-wake.md).

## Goal

Track B is a peer of track A: ordinary user tasks in `chat.deepseek.com` / `chat.z.ai` can use the local Herdr MCP without exposing the workstation token to page JavaScript or routing extension traffic through the public Worker/Tunnel.

```text
web task
  -> extension content bridge adds the Herdr tool protocol/catalog
  -> web model emits {"tool":"...","args":{...}}
  -> extension service worker POSTs tools/call to 127.0.0.1:8772/mcp
  -> TOOL_RESULT is returned to the same web conversation
  -> web model either calls another tool or answers normally
```

## Current state (0.1.52)

| Capability | Status |
|---|---|
| `tools/list` from local Herdr MCP | **available** |
| typed tool catalog injected into the web-model protocol | **available** |
| one or more JSON tool calls per assistant reply | **available** |
| controlled `tools/call` rounds + result backfill until a normal answer | **available** |
| parallel execution of independent calls in one batch | **available** |
| tool-result sanitization / large binary omission / size bound | **available** |
| no Herdr credential in page JavaScript or the service worker | **available** — current builds use Native Messaging plus mode-`0600` Unix IPC; bearer compatibility stays inside the native host/server for older versions |
| z.ai / DeepSeek conversation-scoped `Auto on/off` for Herdr progress/settled push-back | **available** when global automation is permitted |
| persisted z.ai `/c/<chat_id>` Manual handoff | **available** with `Auto off`; handoff control messages bypass this bridge |
| unfinished tool JSON recovery after refresh/reload | **available** — if the last real conversation message is assistant Herdr tool-call JSON and bridge context exists, execution resumes automatically |
| long JSON→MCP chains | **available** — round 12 is only a scheduler-yield checkpoint; completion means a normal non-tool assistant answer |

The bridge uses the live local `tools/list` catalog rather than maintaining a second hand-written allowlist. The extension still validates the calling site/conversation and keeps all MCP traffic on loopback. The local Herdr server remains the authoritative tool/permission boundary.

## Protocol

The content bridge gives the web model a typed catalog and requires tool-call replies to contain one JSON object per line:

```json
{"tool":"herdr_inspect","args":{}}
```

Independent calls may be emitted together and run concurrently; dependent calls remain sequential. A tool call is never treated as successful until the service worker returns its `TOOL_RESULT`.

Intermediate bridge messages are folded from the visible conversation where the site supports that behavior. Tool results are sanitized recursively, very large binary/base64 fields are omitted, and a result batch is capped before it is sent back to the web model.

Version 0.1.50 folds internal protocol messages on history load as well as during active runs. The fold bar is a sibling of the site's message root and toggles the whole message, so expanding a z.ai row cannot consume the flex-row width and squeeze the original content into a narrow vertical column.

## Security boundary

- The MV3 service worker sends bounded request/stream messages through Chrome Native Messaging. The native host talks to herdr-mcp over `~/.config/herdr-mcp/extension.sock` (mode `0600`), so neither the service worker nor page JavaScript receives a Herdr bearer. The host/server retain old-version bearer compatibility without exposing that credential surface to current browser code.
- MCP requests go only to the configured local Herdr endpoint, normally `http://127.0.0.1:8772/mcp`.
- Site identity and conversation identity are checked before bridge/automation operations are accepted.
- The bridge does not pretend DeepSeek or z.ai has a native OAuth MCP Connector.
- ChatGPT continues to use its Connector where appropriate; this JSON bridge is for sites without that integration.

## z.ai 1.1.88 compatibility

The current adapter treats `/` as the new-chat launcher and `/c/<chat_id>` as the stable persisted conversation identity. It uses current DOM/composer signals (`.user-message`, `.markdown-prose`, `#send-message-button`) while retaining compatible fallbacks.

A temporary binding or conversation-automation preference on the root launcher migrates once when the same tab first becomes `/c/<chat_id>`. Later navigation between existing `/c/A` and `/c/B` chats never drags a workspace binding or automation preference along.

## Cooperation with continuity / handoff

Track A can bind the same z.ai / DeepSeek conversation for Herdr progress/done push-back while Track B handles local MCP tool calls. The conversation-level `Auto on/off` controls automatic progress/settled push-back; it does not enable ChatGPT-only stale-view recovery, post-turn LLM decisions, or automatic rollover.

Persisted z.ai chats can use **Manual handoff** only with `Auto off`. The summary and seed are sent through the bridge's raw channel, so continuity-control text is not rewritten into a coding-agent task. Workspace bindings move only after the fresh z.ai chat has a new `/c/<chat_id>` and the seed marker is confirmed.
