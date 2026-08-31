# Browser extension: continuity, Control Center, and the experimental local bridge

The herdr-mcp browser extension is an **optional browser layer** on top of a working MCP Connector. It is not a second agent runtime and it is not required for the first workstation connection.

It owns three browser-side problems:

| Surface | Problem it solves | Detailed document |
| --- | --- | --- |
| Continuity | How local completion returns to, recovers, or hands off the correct Web conversation | [Browser Continuity](browser-continuity.md) |
| Control Center | How Chrome Side Panel observes workspaces, panes, and agents and manages binding / pinned target | [Browser Control Center](browser-control-center.md) |
| JSON → MCP bridge | How a Web AI without a native MCP Connector can use local tools through a bounded JSON protocol | [JSON → MCP bridge](browser-json-mcp-bridge.md) |

Queue beside the ChatGPT composer is a browser interaction primitive: it waits for the current reply to finish, then sends an explicit next-turn user instruction. It does not interrupt generation.

See [Browser extension privacy policy](privacy.md) for data handling and permissions.

## Installation identities: STORE / STANDALONE / DEV

Extension identity is independent from the Runtime DEV/PROD plane:

| Channel | Purpose | Chromium identity |
| --- | --- | --- |
| **STORE** | default for ordinary users | fixed Chrome Web Store identity, updated by the store |
| **STANDALONE** | v0.4.3+ GitHub/manual independent distribution | fixed non-Store identity; moving the install directory does not change the ID |
| **DEV** | source development | Load unpacked from repo/worktree `extension/`; ID is path-derived |

Stable v0.4.2 only has STORE/DEV Native Host ownership. STANDALONE requires a v0.4.3+ runtime that actually implements that contract. A path-derived DEV build must not masquerade as standalone.

Default to the [official Herdr Chrome Web Store extension](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp). Use STANDALONE only when Store distribution is not appropriate and the installed runtime explicitly supports it. DEV is for source development only.

After choosing a channel, verify:

```bash
herdr-mcp native-host status
```

The active channel, extension identity, Native Host, and current runtime generation must agree. STORE updates through Chrome Web Store, STANDALONE through formal independent packages, and DEV through an explicit developer Reload. Refresh long-lived Web pages after an extension update so they receive the current content script.

## Entrypoints and state objects

| Concept / entrypoint | One responsibility |
| --- | --- |
| Toolbar icon | open the Side Panel Control Center |
| HUD | compact current-page status, Auto, manual continue/handoff |
| Control Center | workspace binding, Pinned Target, local observation and human control |
| Queue | send the next explicit user message after the current reply ends |
| Workspace Binding | which long-lived workspace owns this Project / conversation |
| Pinned Target | which pane / agent the next human control explicitly targets |
| Herdr Focus | the pane currently viewed in Herdr UI; it must not silently replace binding or pinned target |

Why these states are separate, and how recovery/handoff works, belongs to [Browser Continuity](browser-continuity.md) and [Browser Control Center](browser-control-center.md) as their respective SSOTs. This overview intentionally does not repeat those implementation details.

## Local security boundary

The extension does not place the Herdr bearer in page JavaScript, the service worker, or browser storage:

```text
page content script / Side Panel
          ↓
Chrome Extension Service Worker
          ↓ Native Messaging
local Host
          ↓ Unix socket (0600)
herdr-mcp Rust runtime
```

The browser owns interaction and visualization; Native Host is the trusted local bridge; the runtime still owns tool schemas, managed-root checks, permissions, and mutation boundaries. Public OAuth/MCP and local Native Messaging are separate trust boundaries.

## First use

1. Make sure Runtime + ChatGPT Connector already work.
2. Choose STORE / STANDALONE / DEV and verify `herdr-mcp native-host status`.
3. Open a supported Web page and the Side Panel.
4. Bind the page to the intended workspace.
5. Keep Auto off while you verify status, Pinned Target, and manual controls.
6. Enable scoped Continuity automation only when you actually need unattended long-running work.

The z.ai / DeepSeek JSON → MCP integrations are experimental and disabled by default; enable them explicitly in Herdr experimental settings.

## Release and maintenance boundary

STORE / STANDALONE / DEV identities may coexist, but the managed Native Messaging manifest has one active owner. `contracts/browser-extension-store.json` is the machine-readable SSOT for Store identity; v0.4.3 uses `contracts/browser-extension-standalone.json` for Standalone; DEV remains path-derived.

After `native-host use store` / `use standalone` / `use dev`, refresh supported pages that were already open. Extension versions evolve independently from the Rust runtime; only a new Native Host identity/channel contract requires matching runtime support.

Maintainer release details live in `docs/_wip/browser-extension-development-and-store-release.md` and `AGENTS.md`.
