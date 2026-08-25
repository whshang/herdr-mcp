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

1. Build the project.
2. Install the Native Messaging host:

```bash
bin/herdr-extension-host install
```

3. Load `extension/` as an unpacked Chrome extension.
4. Open a supported Web AI site.
5. Use the popup/HUD to bind the current conversation to a Herdr workspace.

The binding unit is a workspace, not a single agent, because real development commonly looks like:

```text
workspace
 ├─ coding agent
 ├─ test process
 ├─ server/log pane
 └─ review agent
```

Continuity should represent the whole work area.

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
move workspace binding
```

The old binding remains authoritative until the new conversation and seed are verified. The handoff records established work history; the new conversation still re-checks live Herdr, Git and runtime state before mutation.

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
