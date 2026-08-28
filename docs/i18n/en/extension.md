# Browser extension: continuity, Control Center, and the local bridge

The herdr-mcp browser extension is not a generic web-clicking bot and it is not a second agent runtime.

It connects the Web conversation, the real local Herdr worksite, and the MCP runtime into a long-lived browser workflow that can be observed and recovered.

With MCP alone, the primary direction is:

```text
Web AI → local workstation
```

The browser extension adds a return path from the workstation to the conversation plus a Side Panel that observes local Herdr state directly:

```text
local Herdr → browser extension → Web conversation
                         ↘ Control Center
```

## Three product surfaces

The current extension is easiest to understand as three responsibilities.

| Surface | Problem it solves | Main entry point |
|---|---|---|
| Continuity | How does the correct Web conversation continue, recover, or hand off after local work changes? | In-page HUD |
| Control Center | What workspaces / panes / agents exist locally, and which pane is the explicit human target? | Chrome Side Panel |
| JSON → MCP bridge | How can a Web AI without a native MCP Connector call local Herdr tools? | Inside z.ai / DeepSeek |

The **Queue** button beside the ChatGPT composer is a related interaction capability: it records “send this user intent after the current reply” without interrupting the live assistant turn.

## What each entry point is for

| Entry point | Responsibility |
|---|---|
| Toolbar action | Open the Browser Control Center directly in Chrome Side Panel |
| HUD | Compact current-page status (Web + Herdr + aggregate binding counts), Auto, manual continue, Herdr status extraction, and LLM judge |
| Control Center | Active-tab Project / conversation binding and handoff, live workspaces / panes / agents, explicit target pinning, reads, and future-action previews |
| Queue | Save the next ChatGPT user intent while the current reply is still running |
| Options | Language, continuity timing, optional LLM judge, and other low-frequency configuration |

The toolbar action goes straight to Control Center. Binding / unbinding and manual handoff have one UI path in Control Center; the HUD does not have a drawer. The HUD keeps only high-frequency status, Auto, and three preset conversation actions. Timing and model configuration stay in Options.

## Security architecture

The extension does not place the Herdr bearer in page scripts, the service worker, or browser storage.

Primary local path:

```text
page content script / Side Panel
          ↓
Chrome Extension Service Worker
          ↓ Native Messaging
local Host
          ↓ Unix socket (0600)
herdr-mcp Rust runtime
```

This keeps responsibilities separate:

- the browser owns page interaction and presentation;
- the Native host owns the trusted local bridge;
- herdr-mcp runtime retains tool, permission, and mutation boundaries;
- Cloudflare Edge is not required for browser-extension access to local state.

Public OAuth/MCP and local Native Messaging are different security boundaries.

## Installation and first use

### Primary end-user path

Users do not need to clone the repository.

1. From the same GitHub Release that publishes the current Rust binary, download:

```text
herdr-mcp-extension-<version>.zip
herdr-mcp-extension-<version>.zip.sha256
```

The extension zip is a Release asset but is intentionally not part of the binary updater's `release-manifest.json` contract.

2. Verify the sidecar and extract into a stable managed directory:

```bash
mkdir -p ~/.config/herdr-mcp/extension
unzip herdr-mcp-extension-<version>.zip -d ~/.config/herdr-mcp/extension
# manifest.json must live directly in this directory
```

3. Open:

```text
chrome://extensions
```

Enable Developer mode → **Load unpacked** → choose:

```text
~/.config/herdr-mcp/extension
```

4. Install the Native Messaging host:

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

During migration the repository script remains available:

```bash
bin/herdr-extension-host install
bin/herdr-extension-host status
```

5. Open ChatGPT, z.ai, DeepSeek, or another currently supported page.
6. Click the Herdr toolbar icon and confirm **Browser Control Center** opens directly in Chrome Side Panel.
7. In **Browser Control Center**, confirm the active tab is recognized as the intended Project / conversation, then bind it directly from the matching workspace row below; binding state and live workspace state share that row.
8. Confirm the local runtime is reachable and live workspace / pane state is visible. Switching tabs should update both the Current page card and workspace binding highlight without changing an explicit Pinned Target.

### Developer path

Developers can load the repository's:

```text
<repo>/extension/
```

An unpacked extension identity depends on its absolute load path, while the Native Messaging host restricts the allowed extension origin. Do not casually move between worktree paths and assume an old Native Messaging registration still matches.

The end-user path remains the stable managed extension directory rather than cloning the repository.

## Binding, Pinned Target, and Focus

The browser product now has three distinct kinds of “where”.

### Workspace Binding

A Workspace Binding says which local work context a Web Project / conversation belongs to.

For example:

```text
ChatGPT Project → Herdr workspace wD7
```

The binding is a workspace identity, not one agent.

A real development worksite may contain:

```text
workspace
 ├─ coding agent
 ├─ tests
 ├─ server
 └─ reviewer
```

### Pinned Target

Pinned Target belongs to Control Center. It says exactly which pane / agent a future human control is about, for example:

```text
wD7:p2 / pi
```

Pinned Target does not follow Herdr focus changes automatically.

### Herdr Focus

Herdr Focus is simply the pane the human is currently viewing in the Herdr UI.

Focus may change frequently, but it must not silently replace a binding or pinned target.

See [Browser Control Center](browser-control-center.md).

## Continuity: keep the right Web conversation moving

Browser continuity covers:

- workspace binding;
- working / progress / settled push-back;
- ChatGPT stale-view, disconnect, and send-timeout recovery;
- bounded page-health self-recovery;
- long-conversation handoff / rollover;
- Project- or conversation-scoped Auto preferences.

### ChatGPT Project binding

A ChatGPT Project binding uses the stable `project_id`, not one conversation id.

A concrete `/c/<id>` is the current delivery target `active_conv_key`. Only a genuinely active tab may take over that target; opening a sibling conversation in the background does not steal it.

During handoff the Project binding and `continuity_id` stay stable. The active target changes only after the new conversation and seed are confirmed.

### Automation defaults off

Every new scope starts with Auto off.

Once Auto is explicitly enabled, the extension may run the subset of progress, settled, LLM continue, recovery, handoff, or in-page permission behavior supported by that site and scope.

Turning Auto off does not delete the binding and does not stop observation.

See [Auto-continue, recovery and handoff](extension-wake.md) for the state machine.

## Queue: add the next instruction without interrupting the current reply

While ChatGPT is generating, users often think of another requirement:

- “also run the smoke test”;
- “do not publish this yet”;
- “check one more edge case”.

Sending immediately can interrupt the live turn or race with tool work already in progress.

The extension therefore adds **Queue** next to ChatGPT's native composer actions.

Behavior:

1. Write the additional requirement in the composer.
2. Click Queue.
3. The text enters a durable queue for the current conversation.
4. The current assistant turn continues uninterrupted.
5. After the turn settles, queued content is handled before generic LLM auto-continue.
6. Multiple queued entries preserve order and merge with blank lines into one next user turn.
7. Only an acknowledged delivered batch is removed.
8. A `turn-in-progress` or other blocked delivery keeps the pending content instead of dropping it.

Additional bounds:

- the queue is size- and length-bounded;
- right-click Queue to clear the current conversation queue;
- clicking with an empty composer can retry a pending batch;
- a successful handoff migrates pending queued content to the target conversation without reordering it.

Queue represents **the user's next-turn intent**. It is not a background shell-command queue and it does not invoke a Herdr mutation directly.

## Control Center: observe local truth before acting

Clicking the Herdr toolbar icon opens **Browser Control Center** directly in Chrome Side Panel.

The current Control Center can:

- display live workspace / pane lifecycle;
- show agent working / idle / done / blocked state;
- distinguish terminal-only panes;
- show current Herdr focus;
- pin an explicit pane / agent target;
- revalidate target identity after reconnect;
- fail closed when a target becomes stale;
- `Inspect state`;
- `Read output tail` with bounded output;
- build risk-classified preview descriptors for **Prompt Agent / Steer Session / Herdr API / Terminal Input**.

### Mutation controls remain preview-only

The panel says this directly:

> Live state · preview-only controls

Prompt Agent, Steer Session, Herdr API, and Terminal Input do not execute mutations in the current release.

That is the Browser Control Plane Phase A boundary, not a missing click handler.

See [Browser Control Center](browser-control-center.md).

## JSON → MCP bridge

For pages such as z.ai and DeepSeek that do not expose a ChatGPT-like native MCP Connector, the extension can provide a bounded compatibility path:

```text
Web model
 ↓ JSON tool call
extension
 ↓ Native Messaging
local MCP
 ↓
Herdr tools
```

It can:

- obtain local `tools/list`;
- execute `tools/call`;
- return tool results to the page;
- continue controlled tool rounds until the assistant returns a normal answer;
- fold internal protocol messages to reduce visual noise.

This is an explicit page-protocol adapter rather than pretending the site has native MCP.

See [JSON → MCP bridge](extension-bridge.md).

## Site capabilities are intentionally different

Do not pretend ChatGPT-specific recovery exists everywhere.

| Capability | ChatGPT | z.ai / DeepSeek | Claude |
|---|---|---|---|
| workspace binding | supported | supported | basic support |
| progress / settled | supported | supported | depends on current adapter |
| ChatGPT stale-view / send-timeout recovery | supported | not applicable | not applicable |
| Project-scoped binding / rollover | supported | not applicable | not applicable |
| conversation handoff | Project supported | persisted `/c/<id>` supported | not applicable |
| Queue | supported | not applicable | not applicable |
| JSON → MCP bridge | not needed | supported | not the same path |
| Control Center | browser-level, shared local Herdr state | browser-level | browser-level |

The current manifest, adapters, and tests remain the authority for exact capability support.

## Options and low-frequency configuration

Options owns:

- en / zh / ja UI language;
- local runtime URL for compatibility / diagnostics;
- progress and fallback timing;
- settled / progress message templates;
- optional post-turn LLM judge;
- the global ChatGPT Project-automation gate.

An optional LLM judge API key stays in local browser storage. It is not a Herdr bearer and must not be committed to the repository.

## Local-network and browser permissions

Recent Chrome versions may expose loopback / local-device access separately from ordinary host permissions.

Native Messaging is the current trusted primary path, while some diagnostic or compatibility paths may still surface a loopback permission prompt.

If Control Center / Options / HUD cannot reach the local runtime, check in this order:

1. herdr-mcp runtime;
2. Native Messaging host;
3. whether the extension was reloaded;
4. browser local-device / loopback permission.

Do not copy `HERDR_MCP_TOKEN` into extension storage as a normal fix.

The GA manifest currently retains `<all_urls>` in `host_permissions`. In addition to the fixed content-script sites, the optional LLM judge can target a user-configured OpenAI-compatible base URL that is unknown at install time, and Options retains compatibility with a non-default local runtime URL. Narrowing permissions requires a productized optional-permission UX rather than pretending the fixed content-script hosts cover all current network behavior.

This is not a statement that the extension is already published in Chrome Web Store. Store publication remains a separate maintainer plan.

## What the extension does not do

The extension does not:

- replace ChatGPT / Web AI reasoning;
- replace Herdr agents;
- enable arbitrary terminal mutation just because a Side Panel exists;
- treat Herdr focus as a mutation target;
- guess a replacement after a target becomes stale;
- expose a high-privilege local bearer to web pages;
- require a public Herdr control port for browser continuity;
- bypass organization, browser, or OS permissions.

Its job is **long-lived connectivity, observable state, explicit control boundaries, and browser-side recovery**.

## Read next

- [Browser continuity](browser-continuity.md)
- [Browser Control Center](browser-control-center.md)
- [Auto-continue, recovery and handoff](extension-wake.md)
- [JSON → MCP bridge](extension-bridge.md)
- [Troubleshooting](troubleshooting.md)
