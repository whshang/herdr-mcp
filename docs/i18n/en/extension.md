# Browser extension

*Continuity, Browser Control Center, and the experimental local bridge.*

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
| **STANDALONE** | GitHub/manual independent distribution | fixed non-Store identity; moving the install directory does not change the ID |
| **DEV** | source development | Load unpacked from repo/worktree `extension/`; ID is path-derived |

Current runtimes support STORE / STANDALONE / DEV Native Host ownership. STANDALONE uses a fixed non-Store identity; a path-derived DEV build must not masquerade as standalone. Older runtimes may expose fewer channels, so inspect the installed runtime before changing ownership.

Default to the [official Herdr Chrome Web Store extension](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp). Use STANDALONE only when Store distribution is not appropriate and the installed runtime explicitly supports it. DEV is for source development only.

A current runtime can materialize a Load-unpacked STANDALONE copy directly from the GitHub repository without cloning the source tree:

```bash
herdr-mcp extension standalone install
herdr-mcp native-host use standalone
```

A release runtime defaults to its immutable compile-time source commit; only a development build without a verifiable build commit falls back to `main`. Use `herdr-mcp extension standalone install --ref main` when development explicitly needs the latest repository source.

Runtimes that advertise `--path` may choose the Chrome-facing Load-unpacked path explicitly:

```bash
herdr-mcp extension standalone install --ref <release-tag-or-commit> --path ~/Documents/herdr-mcp/extension
```

`--path` is optional and defaults to `~/Documents/herdr-mcp/extension`. A custom path must resolve below the user's HOME. The managed copy always stays at `~/.config/herdr-mcp/extensions/standalone/current`, and the selected path is a stable symlink to it, so changing `--path` does not change the standalone extension identity. An explicitly requested path that is already occupied fails closed instead of being overwritten; a default-path conflict is left untouched and the installer falls back to the managed path. Run `herdr-mcp extension standalone status` and use its `chrome.load_unpacked_path` value when automation needs the exact directory to select in `chrome://extensions` → Developer mode → Load unpacked; future updates reuse the same path. That value is the Chrome-facing path recorded by the last install, read from `~/.config` state, so it stays stable even when macOS permissions prevent inspecting `~/Documents`; `user_visible_path.status` reports that alias inspection as `unverified` without changing `chrome.load_unpacked_path`.

The installer resolves the requested ref to an immutable commit SHA and downloads only Git-tracked files below that commit's `extension/` tree. Every file remains byte-identical to the repository source except `manifest.json`, where Herdr injects the public `key` required for the fixed STANDALONE extension ID. The repo/worktree DEV manifest is never modified. `herdr-mcp extension standalone status` reports the installed commit, version, ID, and path.

The extension package workflow (`.github/workflows/extension-store.yml`) also emits a manual package alongside the Store ZIP. `herdr-mcp-extension-X.Y.Z.zip` is the Chrome Web Store upload package; `herdr-mcp-extension-standalone-X.Y.Z.zip` and its `.sha256` sidecar are for manual **Load unpacked**. Only the standalone ZIP carries the public fixed manifest `key` that preserves the STANDALONE Chromium ID required by Native Messaging, so never load the Store ZIP manually. Verify it with `shasum -a 256 -c herdr-mcp-extension-standalone-X.Y.Z.zip.sha256` and extract it into a clean directory instead of overlaying an older version.

On macOS, also run `herdr-mcp doctor` after installing or updating STANDALONE. `standalone status` describes the managed files on disk; `doctor` additionally reads only the Herdr extension's exact entry in Google Chrome profile preferences and compares Chrome's active Load-unpacked path with the managed path above. It accepts either the managed `~/.config/herdr-mcp/extensions/standalone/current` directory or the Chrome-facing path recorded by the last install. That recorded path is read from `~/.config` state and compared as a stable string, so changing `--path`, or macOS denying read access to `~/Documents`, does not produce a false `drift`. If it reports `WARN standalone-extension-load state=drift`, the same fixed extension ID is being loaded from another directory, commonly an older Downloads or development copy. Open `chrome://extensions`, find the Herdr extension, and load/reload it from the `expected` path reported by `doctor`. Do not switch Native Host channels, delete extension data, or copy credentials merely to repair this path mismatch. The check is diagnostic only: it never rewrites Chrome configuration or reloads tabs.

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

Settings keep the common path short: language, the ChatGPT Project Auto capability gate, and supported WebChat site access stay visible. Experimental sites, Page Assist, local connection details, continuity templates, and timing controls are collapsed until needed. Workspace binding and WebChat Control remain in the Side Panel/HUD where their current-page context is visible.

For semantic Auto, the extension has no provider credential or endpoint settings. It calls the local Herdr Runtime, which owns one semantic route pool with exactly two configuration layers: mode-`0600` `config.json` for the workstation and the authenticated Cloudflare Worker route pool shared across enrolled workstations. Local routes take precedence for each typed/chat capability. Both layers use the same `name / protocol / url / model / api_key` route object schema; `decision` and `decision-vercel` imply typed evaluation, while `openai-chat` implies chat; shell/process environment is not a semantic-provider configuration source. Ordinary Auto uses typed evaluation routes (Jev) -> chat routes (LLM) -> bounded script fallback; Goal-aware Auto can use Jev as an advisory semantic prior for the existing LLM Goal Supervisor. Typed and chat routes share the same bounded rotation, deadline, cooldown, and failover policy. Existing `config.toml` migrates once to JSON. The script fallback keeps basic Auto available without a semantic provider, while Work Memory/TODO evidence and deterministic safety guards remain authoritative.

1.0 browser support covers ChatGPT, Claude, and Grok. Their Project / Project-chat surfaces share workspace binding and WebChat Control; a global new-chat/home surface keeps WebChat Control but has no workspace binding until a Project or conversation scope exists. ChatGPT additionally supports conversation `create`, archive, and self-handoff. Claude and Grok support signed-in session dispatch, settled results, and reload recovery. Gemini stays opt-in experimental and is outside the 1.0 acceptance boundary.

The z.ai / DeepSeek JSON → MCP integrations are experimental and disabled by default; enable them explicitly in Herdr experimental settings.

## Release and maintenance boundary

STORE / STANDALONE / DEV identities may coexist, but the managed Native Messaging manifest has one active owner. `contracts/browser-extension-store.json` is the machine-readable SSOT for Store identity; `contracts/browser-extension-standalone.json` is the SSOT for Standalone; DEV remains path-derived.

After `native-host use store` / `use standalone` / `use dev`, refresh supported pages that were already open. Extension versions evolve independently from the Rust runtime; only a new Native Host identity/channel contract requires matching runtime support.

Maintainer release details live in `docs/_wip/browser-extension-development-and-store-release.md` and `AGENTS.md`.
