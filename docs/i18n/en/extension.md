# Browser extension: continuity, Control Center, and the local bridge

The herdr-mcp browser extension is not a second agent runtime. It adds browser-side long-running workflow features to an already working herdr-mcp setup: conversation continuity, a Chrome Side Panel Control Center, workspace binding, and queued next-turn messages.

**Finish the runtime + ChatGPT Connector first, then install the browser extension.** The extension is not required for the first ChatGPT-to-workstation connection.

Data handling and permission use are documented in the [Browser extension privacy policy](privacy.md).

## Installation identities: STORE / STANDALONE / DEV

Browser-extension identity is separate from the Runtime DEV/PROD model:

| Channel | Intended use | Chromium identity |
| --- | --- | --- |
| **STORE** | default ordinary-user install | fixed Chrome Web Store identity + Store updates |
| **STANDALONE** | v0.4.3+ GitHub/manual independent distribution | fixed non-Store identity; moving the unpacked package does not change the ID |
| **DEV** | source development | Load unpacked from repo/worktree `extension/`; path-derived ID |

Current stable v0.4.2 Native Host supports Store/DEV ownership only. v0.4.3 adds standalone; v0.4.2 is not repacked or retagged to retrofit it. Do not use a path-derived DEV build as a substitute for standalone.

STORE installs the [official Herdr Chrome Web Store build](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp). STANDALONE uses only a v0.4.3+ fixed-identity release package. DEV is for contributor/extension development and is loaded from an explicit repo/worktree path.

After installing/selecting a channel, run:

```bash
herdr-mcp native-host status
```

Require the active channel/extension identity to match the selected build and the Native Host runtime to match the active runtime generation. Then open ChatGPT or another supported site. z.ai and DeepSeek remain experimental and disabled by default until enabled in **Herdr Settings → Experimental features** and the page is reloaded. Open Browser Control Center from the Herdr toolbar icon, confirm the intended Project/conversation, then bind the Herdr workspace.

## Updates

- **STORE** updates through Chrome Web Store.
- **STANDALONE** updates through the GitHub/independent release surface with a new fixed-identity package; unpacking to a different path does not create a new browser identity.
- **DEV** follows the loaded repo/worktree source and is explicitly Reloaded by the developer.

If a long-open ChatGPT tab still runs an older content script after an extension update, refresh that web page so the new content script is injected.

Browser-extension versions and Rust-runtime versions have independent release cadence. Pure UI, DOM, or browser-compatibility fixes do not require a new Rust runtime release, but a new Native Host identity/channel contract must be used with a runtime that actually implements it.

## Three product surfaces

| Surface | Problem it solves | Main entry |
|---|---|---|
| Continuity | Resume, recover, or hand off the correct web conversation after local work changes | Page HUD |
| Control Center | Observe workspaces / panes / agents, bind the active page, and pin an explicit target | Chrome Side Panel |
| JSON → MCP bridge | Experimental bounded local bridge for Web AI without a native MCP Connector; disabled by default | z.ai / DeepSeek page |

The **Queue** control beside the ChatGPT composer means “send this user intent as the next turn after the current reply settles.” It does not interrupt the live reply.

## Entry-point ownership

| Entry | Responsibility |
|---|---|
| Toolbar icon | Open Chrome Side Panel Browser Control Center |
| HUD | Compact page state, Auto, manual continue, Herdr state extraction, optional LLM judgement |
| Control Center | Page identity, workspace binding, handoff, workspace / pane / agent observation, Pinned Target |
| Queue | Preserve a clear next-turn user message while the current turn is still running |
| Options | Language, continuity timing, optional LLM judge, other low-frequency settings |

Binding / unbinding and local Herdr controls live in Control Center. Manual handoff lives in the compact HUD because it acts on the current web conversation; the HUD still does not duplicate Side Panel binding or local-control UI.

## Security architecture

The extension does not place a Herdr bearer in page scripts, the service worker, or browser storage.

```text
page content script / Side Panel
          ↓
Chrome Extension Service Worker
          ↓ Native Messaging
local host
          ↓ Unix socket (0600)
herdr-mcp Rust runtime
```

Therefore:

- the browser owns page interaction and visualization;
- the Native Messaging host owns the trusted local bridge;
- herdr-mcp runtime still owns tool, permission, and mutation boundaries;
- Cloudflare Edge is not required for the extension to read local runtime state;
- public OAuth/MCP and local Native Messaging are separate security boundaries.

## Binding, Pinned Target, and Focus

### Workspace Binding

Associates a web Project / conversation with a local Herdr workspace, for example:

```text
ChatGPT Project → Herdr workspace wD7
```

The binding target is a workspace, not an individual agent.

For a bound ChatGPT Project, manually starting a fresh conversation does not require copying an internal continuity ID. A plain prior-work intent such as “continue” is resolved through the durable Continuity Journal; automatic resume is allowed only for a unique stable-identity match, otherwise Herdr asks for confirmation. See [Browser continuity](browser-continuity.md) for the exact search / confirm / resume rules.

### Pinned Target

A Control Center-only explicit target for the next human control action, for example:

```text
wD7:p2 / pi
```

Pinned Target does not silently follow Herdr focus changes.

### Herdr Focus

The pane currently viewed in the Herdr UI. It may change frequently and must not silently replace a workspace binding or Pinned Target.

## First use

1. Confirm `herdr-mcp doctor` is healthy.
2. Select/install STORE, STANDALONE, or DEV according to the channel rules above.
3. Use only Native Host commands that the installed runtime actually supports, then verify `herdr-mcp native-host status`.
4. Open the intended ChatGPT Project / conversation.
5. Open Browser Control Center.
6. Bind the page from the matching workspace row.
7. Keep Auto off initially and verify state first.
8. Enable automatic continuity only after the manual path is understood.

## Automatic continuity

The extension keeps bounded state for conversation / Project binding, completion, and recovery. Its job is to restore a long-running workflow, not to create infinite refresh or resubmit loops.

Core rules:

- detection does not depend on the page being scrolled to the bottom;
- normal next-turn messages are not forced while the assistant is still responding;
- HTTP 429 causes backoff, not retry/reload storms;
- when the page is stale but the server is ahead, prefer a safe reload that reconciles the existing result instead of resubmitting the original task;
- exhausted recovery budget becomes an explicit failure / rollover recommendation.

See [Browser continuity](browser-continuity.md) and [wake / recovery](browser-continuity.md).

## Development, Standalone, and Store publishing

DEV source loading, fixed-identity STANDALONE packages, and Chrome Web Store publication are three separate extension distribution paths. The extension version lifecycle remains independent from the Rust runtime, while Native Host channel contracts must match the capabilities of the runtime actually installed.

Maintainers should use:

- `contracts/browser-extension-store.json` as the single machine-readable Chrome Web Store identity SSOT; Rust consumes and validates this contract instead of hard-coding the Store ID;
- `contracts/browser-extension-standalone.json` as the v0.4.3 Standalone fixed-identity SSOT; the package injects only the public manifest key while the DEV source `extension/manifest.json` remains unkeyed;
- `herdr-mcp native-host dev enable [PATH]` to register and activate one unpacked Dev identity (`PATH` defaults to `./extension`);
- `herdr-mcp native-host use store` / `use standalone` / `use dev` to switch the one active/default browser owner without uninstalling sibling extension identities; `use standalone` is a v0.4.3+ command and must not be assumed on v0.4.2;
- `herdr-mcp native-host dev disable` to forget the Dev identity and return Store to active ownership;
- `HERDR_EXTENSION_PATH=/path/to/unpacked/extension herdr-mcp native-host install` only as a compatibility form for older maintainer workflows;
- `native-host dev enable` configures the Dev identity, Native Host trust, and active owner; it does **not** silently install an unpacked extension into branded Chrome. Chrome 137+ removed `--load-extension` from branded builds and 139+ also removed `--disable-extensions-except`, so after cloning use `chrome://extensions` → Developer mode → **Load unpacked** and select `extension/`. Automated smoke uses Chrome for Testing or Chromium. Chrome 146+ CfT uses `~/Library/Application Support/Google/ChromeForTesting/NativeMessagingHosts/`; `0.4.2` manages it as an optional target when that browser directory exists.
- `docs/_wip/browser-extension-development-and-store-release.md` for the Store workflow;
- the extension validation and release ownership rules in `AGENTS.md`.

STORE / STANDALONE / DEV may coexist as different Chrome extension identities, but the managed Native Messaging manifest always carries one exact `allowed_origins` entry: the currently active owner. Inactive builds stay installed but enter standby instead of opening the local shared stream or operational HUD. Switching the active owner also revokes an already-open old Native Messaging request/stream through the Rust host's managed-origin fence, so the previous build cannot keep local control through a persistent connection. Only DEV is path-derived; if its unpacked directory moves, register the DEV origin again.

After `native-host use store`, `use standalone`, or `use dev`, refresh already-open supported Web AI tabs. Page ownership is decided when the content script is injected, so a refresh lets the newly active build claim the page while inactive siblings exit before registering page listeners or HUD/Queue UI. Use `use standalone` only with a v0.4.3+ runtime that actually advertises it; v0.4.2 remains Store/DEV only.

STORE remains the default ordinary-user path. STANDALONE is a supported independent distribution path, not a development build.

## Related docs

- [Browser Control Center](browser-control-center.md)
- [Browser continuity](browser-continuity.md)
- [Wake / recovery](browser-continuity.md)
- [Browser bridge](extension.md)
- [Installation](install.md)

## Advanced local bridge architecture

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
