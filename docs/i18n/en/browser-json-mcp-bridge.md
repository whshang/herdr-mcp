# Browser JSON → MCP bridge: local tools for Web AI without a native Connector

> **Role:** advanced reference for the experimental JSON → MCP compatibility bridge. Most users do not need this page.

ChatGPT can call herdr-mcp through a custom MCP Connector. Not every Web AI product exposes an equivalent integration point. z.ai and DeepSeek can reason in the browser, but they do not provide the same standard path for registering a local Herdr tool catalog.

The JSON → MCP bridge is a compatibility layer for that gap.

It does not pretend the target site natively supports MCP, and it does not expose workstation credentials to page JavaScript. The Web model emits constrained JSON tool requests; the extension and trusted local host perform the actual MCP call.

## End-to-end path

```text
user task
  ↓
z.ai / DeepSeek Web model
  │ constrained JSON tool call
  ▼
content bridge
  ↓
extension service worker
  ↓ Chrome Native Messaging
native host
  ↓ local Unix socket (0600)
herdr-mcp /mcp
  ↓
Herdr + files / Git / shell
  │
  └─ TOOL_RESULT back to the Web conversation
```

Cloudflare Edge is not part of this path.

## Why not call localhost directly from page JavaScript

A direct page → `127.0.0.1` design creates several problems:

- browser origin and local-network permission boundaries;
- risk of workstation bearer exposure to page or extension storage;
- arbitrary page scripts attempting to reuse a privileged local endpoint;
- no single control layer for conversation identity and event streams.

The primary architecture uses Chrome Native Messaging. Browser-side code sends constrained request/stream messages to a native host, which reaches herdr-mcp through a `0600` Unix socket.

This means:

- page JavaScript never sees the Herdr bearer;
- the extension service worker does not need to persist that bearer;
- the local runtime remains the authority for tool schemas and permission gates;
- public OAuth and local IPC remain separate trust boundaries.

## What the Web model sees

The bridge reads the live `tools/list` catalog from the local runtime and translates the relevant typed schemas into a protocol the Web model can follow.

A tool request looks like:

```json
{"tool":"herdr_inspect","args":{}}
```

or:

```json
{"tool":"herdr_git","args":{"root":"/path/to/project","action":"status"}}
```

The bridge validates the request, executes the real MCP `tools/call`, and returns a `TOOL_RESULT` to the same conversation. The Web model then either calls another tool or produces a normal answer.

## Bounded tool loop

The bridge does not turn the browser into an unlimited autonomous agent.

```text
assistant JSON calls
      ↓
validate
      ↓
execute MCP tools
      ↓
return TOOL_RESULT
      ↓
assistant reasons again
      ↓
JSON calls or normal answer
```

Independent calls in the same batch may run in parallel. Dependent steps stay sequential. A tool is only considered successful after the real MCP result returns.

## Result sanitization

MCP results can contain long terminal output, images/binary payloads, structured content or large base64 fields.

Before returning tool results to a Web model, the bridge applies recursive sanitization and size limits. Large binary/base64 content is omitted or summarized so one tool result does not consume the entire browser context.

This changes presentation, not the underlying tool truth.

## Folding protocol messages

JSON tool requests and TOOL_RESULT messages are useful machine coordination but noisy for human reading. Supported site adapters fold these internal messages so the conversation remains centered on user goals and meaningful progress.

Folding affects presentation only; it does not erase the underlying conversation messages.

## Conversation identity

The bridge must know exactly which chat owns a tool loop.

### z.ai

A stable `/c/<chat_id>` URL is the persistent conversation identity. Root `/` is a new-chat launch state. Temporary binding/Auto state may migrate once when that new chat first becomes `/c/<chat_id>`.

Switching later from `/c/A` to `/c/B` does not drag workspace bindings or automation preferences across chats.

### DeepSeek

State is likewise isolated by stable conversation identity extracted by the site adapter. Browser tab identity is not treated as a durable chat identifier.

## Recovering an unfinished JSON tool call after reload

A browser reload must not replay all historical JSON.

Recovery is only eligible when the last real conversation message still looks like an unfinished Herdr tool-call turn and prior bridge context proves that the message belongs to an active protocol sequence.

Mutating tools still obey herdr-mcp delivery/idempotency semantics. Unknown delivery is never a reason to execute the same mutation twice after a page refresh.

## Relationship to browser continuity

JSON → MCP and continuity share the extension and Native Messaging transport, but solve different problems.

| Capability | Direction | Purpose |
|---|---|---|
| JSON → MCP | browser → workstation | give Web AI without a Connector local tools |
| progress / settled | workstation → browser | resume after long local work |
| recovery / handoff | inside browser | recover stalled views or change long conversations |

A z.ai conversation can therefore use the JSON bridge for normal tools while also being bound to a Herdr workspace for progress/settled events.

z.ai / DeepSeek conversation Auto does not enable ChatGPT-specific stale-view recovery or automatic Project rollover.

## Why handoff control messages bypass the JSON task wrapper

Handoff summary and seed messages control the conversation itself; they are not coding tasks.

For z.ai, those messages use a raw path that bypasses the JSON tool wrapper. Otherwise a request such as “produce a handoff packet” could be reinterpreted as another Herdr coding task and create an incorrect recursive loop.

## Security boundary

The bridge follows several explicit rules:

- enabled only for supported sites;
- site and conversation identity checked before execution;
- tool catalog comes from the real local runtime rather than a drifting handwritten copy;
- MCP calls use trusted local IPC;
- browser code does not hold the Herdr bearer;
- herdr-mcp remains the final authority for managed roots, readonly and shell capability;
- extension traffic is not unnecessarily routed through public Cloudflare Edge;
- target sites are not represented as having official OAuth MCP support when they do not.

## When to use it

Use the bridge when you want a Web AI such as z.ai or DeepSeek to operate the same Herdr workstation and public 18-tool contract semantics without building another development backend.

If the client already provides a reliable native MCP Connector, prefer the native standard path. JSON → MCP is a compatibility layer, not a replacement for direct MCP integration.

## Validation

A minimal real UAT should prove that:

1. the bridge reads the current local `tools/list`;
2. the Web model produces a valid `herdr_inspect` request;
3. the native host executes the MCP tool;
4. TOOL_RESULT returns to the correct conversation;
5. the model can continue with another tool or a normal answer;
6. reload does not duplicate an already-completed mutation;
7. workspace binding and progress continuity can operate alongside the tool loop.

Selector details and version-by-version implementation history belong in tests and [CHANGELOG](../../../CHANGELOG.md). This page documents current behavior and security boundaries.
