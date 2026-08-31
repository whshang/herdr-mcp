# Browser Control Center: observe the real Herdr workspace from Chrome Side Panel

The Browser Control Center brings Herdr workspace, pane, terminal, and agent state into Chrome Side Panel.

It is not a shortcut for giving the browser unrestricted shell access. It solves a more fundamental problem:

> When Web AI, local agents, tests, and terminals are all active, how do you keep the real workstation state visible and make the next human control target explicit?

The current Control Center uses a compact layout: the header keeps only Herdr connectivity/running state and a few global actions; a supported active page is reduced to one line such as `ChatGPT · 3 bound`; workspace and pane rows are the primary UI. Click a pane to expand its actions inline. `Send instruction` executes through Herdr `agent.prompt`; `Adjust current task` appears only when native steer is advertised; a working Agent exposes `Stop task`; terminal-only panes expose a fenced `Run command` action through `pane.send_input + Enter`.

## How it differs from browser continuity

The extension now has two related but distinct operational surfaces:

| Surface | Main question | Entry point |
|---|---|---|
| HUD / Continuity | What is this page doing, what is Herdr doing, and should I run Auto or one of the three preset conversation actions? | Inside supported Web AI pages |
| Control Center | Which Project / conversation is the active tab, what is it bound to, what is happening locally, and which pane is the explicit target? | Chrome Side Panel |
| Options | What low-frequency timing / model / language settings should apply? | Control Center Settings |

The HUD is deliberately **not a second control panel**. It has no drawer, workspace picker, binding editor, timing form, or local Herdr mutation controls. It shows Web state, Herdr state, one compact binding badge (`🔗N`), Auto, the three preset progression actions, and Manual handoff because those actions operate on the current web conversation. Pane and Agent detail stays in the Control Center.

The Control Center owns **active-page identity, binding/unbinding, detailed workstation state, and explicit local target selection**. Manual handoff is intentionally owned by the in-page HUD.

They share the same Native Messaging and local-IPC trust path, but they are not one state machine.

## Open the Control Center

Click the Herdr extension icon in the browser toolbar. Chrome opens the **Browser Control Center** Side Panel directly; there is no intermediate extension Popup.

The panel follows the same extension language setting as Options and the in-page HUD. Current UI locales are:

- English;
- Simplified Chinese;
- Japanese.

## Current page follows the active browser tab

The top **Current page** card is the bridge between browser context and local state. It asks the existing binding authority for the active Chrome tab and shows:

- supported site;
- ChatGPT Project identity when present;
- conversation identity when present;
- how many workspaces are currently bound to that Project / conversation;

Bind / unbind is no longer duplicated inside the Current page card as chips plus a selector. The single path is the **binding toggle on each workspace row below**, so live state and page binding are read and changed in the same place.

Changing Chrome tabs or navigating the active tab updates this card from tab activation/navigation events; there is no fixed polling loop. Bound workspaces move to the front of the local workspace list and stay highlighted.

This does **not** retarget an explicit Pinned Target. Active-page binding answers “which local workspace belongs to this Web context”; Pinned Target answers “which exact pane would a future human control action address.”

## Where live state comes from

The Control Center does not poll the page DOM on a fixed interval.

Current data path:

```text
Herdr workspace / pane / agent state
        ↓
herdr-mcp Rust runtime
        ↓ local IPC / push events
Extension service worker
        ↓ one snapshot + incremental events
Chrome Side Panel
```

On first open or reconnect it takes one authoritative snapshot, then consumes incremental lifecycle events such as:

- `workspace_upsert` / `workspace_removed`;
- `pane_upsert` / `pane_removed`;
- agent working / settled changes.

When the Side Panel is hidden, rendering work is reduced. When it becomes visible again or the event stream reconnects, state is reconciled.

A real pane create/close should therefore appear as a lifecycle update rather than waiting for periodic UI polling.

## Workspace Binding, Pinned Target, and Herdr Focus are different identities

This distinction is the most important Control Center interaction contract.

| Concept | Meaning | Automatically follows Herdr focus? |
|---|---|---|
| Workspace Binding | Which local work context a Web Project / conversation belongs to | No |
| Pinned Target | Which pane / agent the next Control Center action explicitly targets | **No** |
| Herdr Focus | Which pane the human is currently looking at in Herdr | Yes |

For example, a ChatGPT Project may be bound to `wD7` while the Control Center explicitly pins `wD7:p2`.

If the human later focuses `wD7:p3` in Herdr, the Control Center must not silently retarget from `p2` to `p3`.

That prevents a dangerous class of mistakes: **the user thinks an action targets A while a focus change causes it to target B**.

## Workspace state and current-page binding share one list

The Control Center no longer splits “workspace status” and “current-page binding” into separate UI modules. Every workspace row now shows:

- workspace label / id;
- an aggregate workspace status dot;
- pane count and working count;
- whether the active page is bound to this workspace;
- the single **Bind / ✓ Bound** toggle.

Workspaces already bound to the active page move to the front and remain highlighted. Clicking the workspace body only expands or collapses its panes; clicking the binding toggle only binds or unbinds, so the two interactions do not trigger each other. Binding mutations are serialized in the UI to avoid ambiguous intermediate states from repeated clicks. Once a workspace disappears from Herdr's authoritative live snapshot, its page bindings are pruned automatically. Opening Control Center also performs a compensating reconciliation, so closed historical workspaces do not remain as offline rows or make different pages report different workspace counts.

Expanded rows continue to show pane-level detail:

- pane id;
- agent name or terminal-only state;
- working / idle / done / blocked status;
- current Herdr focus marker;
- cwd / project root;
- agent elapsed time;
- recent activity;
- a bounded recent summary or terminal title.

Initial expansion is bounded so a workstation with many projects does not open as an unreadable wall of rows. Switching browser tabs updates binding order and highlight without retargeting the Pinned Target.

## Pin an explicit target

Click a pane row to pin it.

The bottom panel then shows an explicit identity such as:

```text
Pinned target
wD7 / wD7:p2 / pi
working · revision ...
```

The pinned target is persisted in extension local state and revalidated after snapshots and reconnects.

### Why a target becomes stale

A pin fails closed when, for example:

- the pane is removed;
- the same pane id now belongs to a new agent session;
- the target revision changes in a way that cannot safely be treated as the same execution target.

The panel does not guess a replacement. The user must select a pane again before reads or action previews continue.

## Actions that really execute today

The panel now has four kinds of behavior rather than one blanket “preview-only” rule:

| Mode | Current behavior | Delivery semantics |
|---|---|---|
| Details | Executes a bounded read | Read-only |
| Recent output | Executes a bounded terminal-tail read | Read-only |
| Send instruction | **Executes** through the trusted extension-only local action route and existing Herdr `agent.prompt` reliability kernel | `submitted`, `queued`, `rejected`, `uncertain`, or `failed`, with an operation id/evidence when available |
| Steer current task | Shown only when the runtime explicitly advertises native steer for the pinned provider | Redirects the active task without stopping its current turn; never falls back to Agent Prompt |
| Stop task | Available only for an Agent that is currently working; confirms before sending literal `Ctrl+C` to that pane | Stops the current CLI turn/process; never presented as a provider interrupt |
| Herdr API | Preview only | No arbitrary Herdr mutation is executed from this UI |
| Run command | **Executes** only for terminal-only panes through `pane.send_input` plus `Enter` | Revalidates `target_revision` before mutation and never auto-retries uncertain delivery |

### Inspect state

`Inspect state` displays structured pane state while bounding potentially large recent output.

### Read output tail

`Read output tail` asks the local runtime for a bounded terminal tail. The request is deliberately limited (roughly 40 lines / 4096 characters), so a terminal that has run for hours cannot dump unbounded history into the Side Panel.

### Send instruction: reliable Agent Prompt, not terminal injection

`Send instruction` targets the pane whose inline action area is open and travels only through:

```text
Side Panel
  → extension service worker
  → Chrome Native Messaging
  → mode-0600 herdr-mcp Unix socket
  → POST /extension/control/action
  → existing durable agent.prompt operation
```

The HTTP route is deliberately unusable on ordinary TCP: even a caller with the normal herdr-mcp bearer receives `403`. This prevents the browser control mutation surface from becoming a public workstation API.

Every action carries a runtime-generated `target_revision`. Rust re-reads the live pane immediately before mutation. If the pane disappeared, the Agent/session behind the pane changed, or the runtime generation changed, the request returns `stale_target` without submitting anything.

Prompt also reuses the existing `agent.prompt` persistent idempotency record instead of inventing browser-only retry logic. The Side Panel creates an idempotency key and surfaces uncertain delivery explicitly. If the result is uncertain, inspect live state before retrying; do not blindly resend.

### Steer current task: never impersonate true steer

`Steer current task` is intentionally stricter than Prompt. The UI shows it only when `control_capabilities.steer.available` is true. It does **not** fall back to `agent.prompt` and then label the result as steer.

For Codex, provider-native same-turn `turn/steer` needs an authoritative mapping from the pinned Herdr pane to the active app-server control endpoint, `threadId`, and current `expectedTurnId`. The current Herdr pane/session metadata does not expose that mapping. Therefore a working Codex pane currently reports `session_not_resolved`; an idle Codex pane reports `no_active_turn`; other providers can report `unsupported_provider`.

A local `~/.codex/ipc/ipc.sock` file by itself is not sufficient evidence: a socket can be stale, can belong to a different client/session, and does not identify the target thread or expected active turn. Provider-native steer will only be enabled when those identities can be proven end to end.

`Stop task` is a separate, narrower local-control path. It is enabled only for an Agent in `working` state, asks for confirmation, and sends `pane.send_keys(["C-c"])`. It does not claim provider-level interrupt semantics and is not automatically retried after uncertain delivery; inspect the target state before sending another stop.

### Run command: terminal-only and fenced

A terminal-only pane exposes `Run command` when expanded. This is not arbitrary Herdr API access and it does not bypass target selection. The Side Panel carries the pane's `target_revision`; Rust re-reads the live pane immediately before mutation and only then calls:

```text
pane.send_input({ pane_id, text, keys: ["Enter"] })
```

If the pane disappeared, was replaced, or became an Agent pane, the action returns `stale_target` or `rejected`. IPC/network ambiguity returns `uncertain` and the UI does not automatically resend the command. The path is covered by an isolated real-terminal UAT that verifies the text plus `Enter` executes in the selected pane.

This is the direct resolution of the original Issue #57 ambiguity: **queued/prompted work and same-turn steering are separate outcomes, never aliases.**

## Reliability kernel: memory, request pressure, timeout recovery, and reload loops

Browser Control Plane reliability is not limited to action delivery. The extension already carries the page/runtime protections that were planned alongside the Side Panel work:

- **No fixed Side Panel polling loop.** The panel takes one snapshot and consumes incremental workspace/pane events.
- **One shared Herdr event stream.** Workspace observation does not create one network stream per binding.
- **State-fetch deduplication.** Concurrent freshness requests coalesce instead of multiplying `/push/state` traffic.
- **MutationObserver/render coalescing.** DOM bursts are folded into bounded UI work instead of triggering a render/action for every mutation.
- **Hidden-page suspension.** Expensive UI work is deferred while the surface is hidden and reconciled when visible again.
- **Bounded retained output.** Terminal/output tails are clipped, so long-running panes do not accumulate unbounded browser-side history.
- **UI pressure / heap signals.** The recovery layer observes mutation rate, timer drift, and JS heap pressure where the browser exposes it.
- **429 is backoff-only.** Rate-limit responses extend network backoff; they do not trigger a page reload storm.
- **Evidence-first reply recovery.** Send timeout/disconnected-stream recovery checks same-origin/server state before deciding whether a request needs retry or a view refresh.
- **Force reload is a last bounded recovery step.** Reload requests are sender-scoped, automation-gated, persisted before navigation, protected by a durable cooldown/budget, and concurrent requests elect one winner.
- **No reload loop.** A repeated request during cooldown is rejected, and Auto-off conversations cannot force a background reload.

These mechanisms are one reliability layer shared by continuity and Browser Control Plane. The action route does not add another polling loop, heartbeat, or retry daemon.

## Why arbitrary Herdr methods still stay preview-only

The difficult part of browser control is not writing bytes to a terminal. The hard part is preserving answers to questions such as:

- Is the target still the exact object the user selected?
- Did a failed request get delivered?
- Would retrying duplicate a mutation?
- Has the Agent session behind a pane changed?
- Can delivery phase survive browser reload or MV3 service-worker restart?

Prompt satisfies those contracts by reusing Rust target fencing and `agent.prompt` idempotency. Terminal control now exposes only one narrow `terminal_input -> pane.send_input + Enter` operation with the same target fencing and no automatic retry after uncertain delivery. Arbitrary Herdr method invocation still has a much wider effect surface and therefore remains fail-closed / preview-only.

## Runtime and event-stream states

The top status distinguishes:

- **Runtime unavailable** — there is no reliable current runtime snapshot;
- **Runtime healthy · event stream reconnecting** — existing state can be shown while the live event path reconnects.

After reconnect the panel refreshes an authoritative snapshot and resumes incremental events.

## Using Control Center, HUD, and Queue together

Treat the three surfaces as different layers:

```text
HUD
  current web conversation: status, Auto, Continue / Check Herdr / LLM decide
  ↓
Control Center · Current page + Workspaces
  current page ↔ workspace binding plus live workspace / pane / agent truth
  ↓
Control Center · Local Herdr target
  explicit pinned pane and local Herdr reads / action preview
  ↓
Queue (ChatGPT composer)
  add the next user intent without interrupting the current reply
```

A typical flow is:

1. Open Control Center and confirm the real workspace and working pane.
2. Pin a pane if needed and inspect its recent output.
3. Return to ChatGPT and let the Web planner perform real control through MCP / Herdr tools.
4. If a new requirement occurs while ChatGPT is still replying, use Queue instead of interrupting the live turn.
5. Queued content becomes the next user turn after the current reply settles.
6. For long work, the browser continuity engine maintains progress / settled / recovery / automatic handoff; the HUD exposes page-scoped status, Auto, the three preset progression actions, and Manual handoff, while binding and local Herdr control stay in the Side Panel.

## Local security model

The Control Center uses the existing trusted local path:

```text
Side Panel
   ↓ Extension service worker
Chrome Native Messaging host
   ↓ Unix socket (0600)
herdr-mcp Rust runtime
```

Opening the panel does not:

- expose a Herdr bearer to web pages;
- open a public workstation port;
- turn arbitrary pages into unrestricted shells;
- substitute Herdr focus for explicit target identity;
- continue mutation against a stale target.

## Current product boundary

The Control Center currently includes:

- a first-class Chrome Side Panel entry point;
- live workspace / pane lifecycle without fixed polling;
- agent status presentation;
- explicit pinned target;
- runtime-authoritative `target_revision` and stale-target fail-closed behavior;
- bounded state / output reads;
- executable trusted `Prompt Agent` with durable idempotency/outcome evidence;
- executable provider `Steer Session` request with honest capability outcomes and **no Prompt masquerade**;
- preview-only arbitrary Herdr API / raw terminal controls;
- shared memory/request-pressure/timeout/reload-loop protections;
- en / zh / ja UI.

Codex true same-turn steer remains gated on a verifiable pane → app-server endpoint → `threadId` → active `expectedTurnId` mapping. Until that primitive exists, `session_not_resolved` is the correct outcome rather than a hidden fallback.

## Related documentation

- [Browser extension overview](extension.md)
- [Browser continuity](browser-continuity.md)
- [Auto-continue, recovery and handoff](extension-wake.md)
- [JSON → MCP bridge](extension-bridge.md)
- [Troubleshooting](troubleshooting.md)
