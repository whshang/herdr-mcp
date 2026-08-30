# Browser extension: continuity, Control Center, and the local bridge

The herdr-mcp browser extension is not a second agent runtime. It adds browser-side long-running workflow features to an already working herdr-mcp setup: conversation continuity, a Chrome Side Panel Control Center, workspace binding, and queued next-turn messages.

**Finish the runtime + ChatGPT Connector first, then install the browser extension.** The extension is not required for the first ChatGPT-to-workstation connection.

Data handling and permission use are documented in the [Browser extension privacy policy](privacy.md).

## End-user installation: Chrome Web Store only

Normal users install the extension from Chrome Web Store and do not need a local extension build or repository checkout.

1. Open the [official Herdr Chrome Web Store item](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp).
2. Confirm the publisher/product identity, then choose the official Herdr extension.
3. Click **Add to Chrome**.
4. After installation, run:

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

On a fresh `0.4.1+` installation, the normal `native-host install` path uses the official Chrome Web Store extension identity; it does not require an unpacked extension directory or a source checkout. On `0.4.2+`, a maintainer may keep one unpacked Dev build installed alongside the Store build, but herdr-mcp still authorizes exactly one active Native Messaging origin at a time.

5. Open ChatGPT or another currently supported site. z.ai and DeepSeek are experimental and remain disabled by default until you enable them separately in **Herdr Settings → Experimental features**, then reload that site.
6. Click the Herdr toolbar icon and confirm the **Browser Control Center** opens in Chrome Side Panel.
7. Confirm the active page is recognized as the intended Project / conversation, then bind the Herdr workspace from the Control Center.

> The extension is currently entering its first Chrome Web Store publication flow. Until the listing is live, normal users should skip this optional step rather than install a local development build.

## Updates

After a Chrome Web Store installation, Chrome's normal Web Store update mechanism delivers new extension versions. Normal users do not need a local extension package or a repository checkout.


If a long-open ChatGPT tab still runs an older content script after Chrome updates the extension, refresh that web page so the new content script is injected.

Browser-extension versions and Rust-runtime versions have independent release cadence. Pure UI, DOM, or browser-compatibility fixes do not require a new Rust runtime release.

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
2. Install the extension from the Chrome Web Store.
3. Run `herdr-mcp native-host install`.
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

## Development and store publishing

Local extension builds, Chrome Web Store Developer Dashboard, package upload, Trusted Testers, listing assets, and review operations are **maintainer / extension-development workflows**, not end-user installation instructions.

Maintainers should use:

- `contracts/browser-extension-store.json` as the single machine-readable Chrome Web Store identity SSOT; Rust consumes and validates this contract instead of hard-coding the Store ID;
- `herdr-mcp native-host dev enable [PATH]` to register and activate one unpacked Dev identity (`PATH` defaults to `./extension`);
- `herdr-mcp native-host use store` / `herdr-mcp native-host use dev` to switch the one active/default browser owner without uninstalling the other Chrome extension;
- `herdr-mcp native-host dev disable` to forget the Dev identity and return Store to active ownership;
- `HERDR_EXTENSION_PATH=/path/to/unpacked/extension herdr-mcp native-host install` only as a compatibility form for older maintainer workflows;
- `native-host dev enable` configures the Dev identity, Native Host trust, and active owner; it does **not** silently install an unpacked extension into branded Chrome. Chrome 137+ removed `--load-extension` from branded builds and 139+ also removed `--disable-extensions-except`, so after cloning use `chrome://extensions` → Developer mode → **Load unpacked** and select `extension/`. Automated smoke uses Chrome for Testing or Chromium. Chrome 146+ CfT uses `~/Library/Application Support/Google/ChromeForTesting/NativeMessagingHosts/`; `0.4.2` manages it as an optional target when that browser directory exists.
- `docs/_wip/browser-extension-development-and-store-release.md` for the Store workflow;
- the extension validation and release ownership rules in `AGENTS.md`.

Store and Dev may coexist as Chrome extensions, but the managed Native Messaging manifest always carries one exact `allowed_origins` entry: the currently active owner. The inactive 0.1.76+ build stays installed but enters standby instead of opening the local shared stream or operational HUD. Switching the active owner also revokes an already-open old Native Messaging request/stream through the Rust host's managed-origin fence, so the previous build cannot keep local control through a persistent connection. If an unpacked directory is moved, run `dev enable` again because Chromium's unpacked identity is path-derived.

After `native-host use store` or `native-host use dev`, refresh already-open supported Web AI tabs. Page ownership is decided when the 0.1.76+ content script is injected, so a refresh lets the newly active build claim the page while the inactive sibling exits before registering page listeners or HUD/Queue UI. Safe simultaneous Store+Dev enablement therefore requires both browser builds to be 0.1.76+; the first 0.1.75 Store candidate remains a Store-only release candidate and is not rewritten in place.

Once the Store listing is public, end-user documentation keeps only the Chrome Web Store installation path.

## Related docs

- [Browser Control Center](browser-control-center.md)
- [Browser continuity](browser-continuity.md)
- [Wake / recovery](extension-wake.md)
- [Browser bridge](extension-bridge.md)
- [Installation](install.md)
