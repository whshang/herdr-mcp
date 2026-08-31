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

See [Browser continuity](browser-continuity.md) and [wake / recovery](extension-wake.md).

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
- [Wake / recovery](extension-wake.md)
- [Browser bridge](extension-bridge.md)
- [Installation](install.md)
