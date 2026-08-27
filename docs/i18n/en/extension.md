# Browser Extension: keep Web AI work continuous

The browser extension is the continuity layer of herdr-mcp.

MCP solves:

```text
ChatGPT / Web AI → local Herdr workstation
```

The extension solves:

```text
local Herdr workstation → browser conversation
```

Together they make browser-based AI suitable for development tasks that last tens of minutes or hours.

Chrome display name: **herdr → Web wake**.

The extension is not another agent runtime or a second orchestration platform. It maintains the connection between a browser conversation, local Herdr workspace and MCP runtime.

## Two capability lines

| Line | Problem | Typical use |
|---|---|---|
| A. Browser continuity | The browser turn ended while local work continues | progress, settled wake, recovery, conversation handoff |
| B. JSON → MCP bridge | A Web AI site has no native MCP Connector | z.ai / DeepSeek calling local Herdr tools |

They share trusted local transport but solve different problems.

## Security architecture

The extension does not store the Herdr bearer in page JavaScript, the service worker or ordinary browser storage.

Primary path:

```text
content script
    ↓
Chrome Extension Service Worker
    ↓ Native Messaging
native host
    ↓ local Unix socket (0600)
herdr-mcp runtime
```

The browser owns page interaction. The native host owns the trusted local bridge. herdr-mcp remains responsible for tool schemas and permission gates.

Extension traffic does not need to traverse Cloudflare Edge. Public OAuth identity and trusted local IPC are separate security boundaries.

## Install and use it for the first time

Primary path (no git clone required):

1. Download `herdr-mcp-extension-<version>.zip` (and the matching `.sha256` sidecar) from the same GitHub Release that published the Rust binaries. The zip is a Release asset only; it is **not** listed in `release-manifest.json`, so the binary updater schema stays unchanged.
2. Verify the sidecar, then extract into the managed directory:

```bash
mkdir -p ~/.config/herdr-mcp/extension
unzip herdr-mcp-extension-<version>.zip -d ~/.config/herdr-mcp/extension
# top-level files such as manifest.json must land directly under that directory
```

3. In Chrome: `chrome://extensions` → enable Developer mode → **Load unpacked** → select `~/.config/herdr-mcp/extension`.
4. Install the Native Messaging host (resolves the managed path, or `HERDR_EXTENSION_PATH` if set):

```bash
herdr-mcp native-host install
# equivalent during migration:
# bin/herdr-extension-host install
```

5. Open a supported Web AI site.
6. Use the popup/HUD to bind a Herdr workspace. On ChatGPT this can be done from the root page, a Project home, or a concrete conversation; a Project does not need a `/c/<id>` first.

Developer checkout of `extension/` remains available for local work, but it is not the primary end-user path. Do not treat cloning this repository as the install method.

The binding unit is a workspace, not a single agent, because real development commonly looks like:

```text
workspace
 ├─ coding agent
 ├─ test process
 ├─ server/log pane
 └─ review agent
```

Continuity should represent the whole work area.

For ChatGPT Projects, the persistent binding is keyed by stable `project_id`, not by one conversation id. A Project-home binding can therefore exist before any chat is created. The currently active Project `/c/<id>` is only the **delivery target** (`active_conv_key`) for progress/continue messages. Opening a sibling conversation in the background does not move the binding; activating that tab changes only the delivery target. A binding created on `https://chatgpt.com/` is provisional and tab-scoped until that tab first enters a concrete Project or conversation.

## HUD controls

The bottom HUD exposes current scope state and supported actions:

- runtime/workspace status;
- manual continue;
- Herdr monitoring;
- lightweight LLM analysis;
- manual handoff where supported;
- Auto on/off.

Automation defaults off. It only becomes active after the user enables it for the relevant Project or conversation scope.

When Auto is on, conflicting manual progression actions are locked so manual and automatic control paths do not advance the same conversation concurrently. Manual handoff is the deliberate exception: where supported, it can start with Auto on or off, pauses source automatic wakes during transfer, and makes the target inherit the source Auto state.

## A. Browser continuity

### Progress

While agents are working in the bound Herdr workspace, the extension observes state and output changes.

It does not blindly send a message on every timer tick. It checks frequently, sends when meaningful new progress exists, and can use a longer fallback interval when nothing new was produced.

### Settled

When the workspace finishes its active work, the extension wakes the Web planner once so it can re-inspect results.

Settled means local work stopped. It does not mean tests passed, code should be committed, or the task is complete. The Web planner still verifies those facts.

### Recovery

Browser failure modes include partial replies, disconnected streams, explicit send-timeout errors and stale DOM where the ChatGPT server already has a newer message.

Recovery is evidence-first:

```text
observe browser problem
 ↓
check server-side conversation state when possible
 ↓
determine whether the request already advanced
 ↓
synchronize the view
 ↓
retry or handoff only when safe
```

The extension does not simply resend the task because tool mutations may already have happened.

### Handoff / rollover

Long conversations eventually accumulate context pressure. Continuity can create a compact semantic handoff packet and move work into a fresh conversation.

```text
old conversation
 ↓
compact handoff packet
 ↓
new conversation
 ↓
verify seed
 ↓
switch active delivery target
```

For ChatGPT Projects, the Project/workspace binding and `continuity_id` remain stable throughout rollover. The old `active_conv_key` remains authoritative until the new conversation and seed are verified; only then does the extension switch the delivery target. Conversation-scoped sites such as z.ai still move their binding after confirmation. The handoff records established work history; the new conversation still re-checks live Herdr, Git and runtime state before mutation.

See [Auto-continue, recovery and handoff](extension-wake.md) for the current state machine.

## B. JSON → MCP bridge

Web AI sites without a native custom MCP Connector can use a compatibility protocol:

```text
Web model
 ↓ JSON tool call
extension
 ↓ Native Messaging
local MCP runtime
 ↓
Herdr tools
```

The bridge can read the local tool catalog, execute `tools/call`, return results to the same conversation and continue a bounded tool loop until the model produces a normal answer.

This is a browser-side protocol adapter, not a claim that the target site natively implements MCP.

See [JSON → MCP](extension-bridge.md).

## Automation boundaries differ by site

| Capability | ChatGPT | z.ai / DeepSeek |
|---|---|---|
| workspace binding | yes | yes |
| progress / settled wake | yes | yes |
| ChatGPT-specific stale-view recovery | yes where supported | not applicable |
| ChatGPT Project automatic rollover | yes where supported | not applicable |
| JSON → MCP bridge | normally unnecessary | yes |

ChatGPT Projects can use Project-scoped automation when globally permitted in Options. Normal ChatGPT conversations and supported non-ChatGPT sites use conversation-scoped preferences.

Site adapters intentionally expose only capabilities that can be implemented safely on that product. They do not pretend every Web AI has the same conversation APIs as ChatGPT.

## Browser/local-network permissions

Modern Chrome versions can gate loopback/local-application access separately from normal host permissions. If the Options connection test or HUD reports that local access is blocked, allow the extension to access applications on the device in Chrome's extension site settings.

The extension uses bounded connection attempts and reports this condition instead of loading forever.

`host_permissions` still includes `<all_urls>` for GA. Narrowing to the four content-script origins plus loopback would cover ChatGPT / Claude / z.ai / DeepSeek scripting and tab reload recovery, but the optional LLM-judge feature fetches a user-configured OpenAI-compatible base URL from the service worker, and Options also allow a non-default `herdrMcpUrl`. Those hosts are not known at install time, so `<all_urls>` remains until an optional-permissions UX can replace it. This is not a Chrome Web Store distribution claim.

## What the extension does not do

It does not:

- replace ChatGPT reasoning;
- replace Herdr agents;
- expose the local MCP server publicly;
- store a high-privilege workstation bearer in browser state;
- bypass browser, ChatGPT Workspace or operating-system permission controls;
- turn every supported website into an identical automation environment.

Its job is narrower and more useful: keep a long-running AI development workflow connected across browser turns.

## Next reading

- [Browser Continuity](browser-continuity.md) — why the return channel exists
- [Auto-continue, recovery and handoff](extension-wake.md) — continuity mechanics
- [JSON → MCP](extension-bridge.md) — local-tool compatibility for z.ai / DeepSeek
- [Troubleshooting](troubleshooting.md) — permission, binding and recovery failures
