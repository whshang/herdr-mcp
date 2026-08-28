# Browser Control Center: observe the real Herdr workspace from Chrome Side Panel

The Browser Control Center brings Herdr workspace, pane, terminal, and agent state into Chrome Side Panel.

It is not a shortcut for giving the browser unrestricted shell access. It solves a more fundamental problem:

> When Web AI, local agents, tests, and terminals are all active, how do you keep the real workstation state visible and make the next human control target explicit?

The current Control Center is **active-page context + live observation + explicit targeting + bounded reads**. Mutation surfaces remain preview-only, so Prompt Agent, Steer Session, Herdr API, and Terminal Input do not bypass existing mutation safety boundaries.

## How it differs from browser continuity

The extension now has two related but distinct operational surfaces:

| Surface | Main question | Entry point |
|---|---|---|
| HUD / Continuity | What is this page doing, what is Herdr doing, and should I run Auto or one of the three preset conversation actions? | Inside supported Web AI pages |
| Control Center | Which Project / conversation is the active tab, what is it bound to, what is happening locally, and which pane is the explicit target? | Chrome Side Panel |
| Options | What low-frequency timing / model / language settings should apply? | Control Center Settings |

The HUD is deliberately **not a second control panel**. It has no drawer, workspace picker, binding editor, timing form, or handoff button. It shows Web state, Herdr state, one compact binding badge (`🔗N`), Auto, and the three preset manual conversation actions. Pane and Agent detail stays in the Control Center.

The Control Center owns **active-page identity, binding/unbinding, manual handoff, detailed workstation state, and explicit target selection**.

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
- an always-discoverable **Manual handoff** action. When the current page is unsupported, unbound, busy, or not yet a concrete conversation, the action stays visible but disabled and explains why.

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

Workspaces already bound to the active page move to the front and remain highlighted. Clicking the workspace body only expands or collapses its panes; clicking the binding toggle only binds or unbinds, so the two interactions do not trigger each other. Binding mutations are serialized in the UI to avoid ambiguous intermediate states from repeated clicks. If a bound workspace has been closed or is temporarily absent from the runtime snapshot, the list keeps a “not currently visible” bound row so the stale binding can still be removed instead of becoming hidden state.

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

Only **read operations** execute from the current panel.

### Inspect state

`Inspect state` displays structured pane state while bounding potentially large recent output.

Use it to confirm:

- the current pane identity;
- status;
- cwd / project root;
- recent output.

### Read output tail

`Read output tail` asks the local runtime for a bounded terminal tail.

The request is deliberately limited (roughly 40 lines / 4096 characters), so a terminal that has run for hours cannot dump unbounded history into the Side Panel.

## Why Prompt Agent / Steer Session / Herdr API / Terminal Input say “Action preview”

The UI already models future control actions, but **those mutations are not enabled in the current release**.

The panel says this directly:

> Live state · preview-only controls

The `Action preview` section contains four modes:

| Mode | Intended future action | Current behavior |
|---|---|---|
| Prompt Agent | Start or supplement work by sending a new prompt through Herdr `agent.prompt` to the pinned Agent | Build a descriptor only; nothing is sent |
| Steer Session | Redirect an **already-running** provider / Agent session when that provider supports steer | Build a descriptor only; nothing is sent |
| Herdr API | Name an intended Herdr control-plane method; future execution must pass the live method schema and safety checks, and is not arbitrary shell | Build a descriptor only; nothing executes or claims validation |
| Terminal Input | Write literal text / input / keys to the pinned terminal pane; this is the highest-risk path | Build a descriptor only; nothing is written |

`Preview action` returns a classified descriptor containing fields such as:

- action type;
- risk class;
- workspace / pane identity;
- target revision;
- args;
- `executable: false`;
- `execution_mode: dry_run`.

This is a deliberate Phase A safety boundary, not a disabled button waiting to be wired up casually.

## Why preview comes before terminal mutation

The difficult part of browser control is not writing bytes to a terminal. The hard part is preserving answers to questions such as:

- Is the target still the exact object the user selected?
- Did a failed request get delivered?
- Would retrying duplicate a mutation?
- Has the agent session behind a pane changed?
- Should provider steer and raw terminal input have different confirmation rules?
- Can delivery phase survive browser reload or MV3 service-worker restart?

Opening arbitrary terminal mutation before those contracts are reliable would undermine the mutation, idempotency, and recovery discipline already enforced by herdr-mcp.

The intended order is therefore:

```text
observe state accurately
  ↓
pin an explicit target
  ↓
describe action and risk
  ↓
only then enable mutation classes one by one
```

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
6. For long work, the browser continuity engine maintains progress / settled / recovery / automatic handoff; the HUD only exposes page-scoped status, Auto, and three preset manual actions, while binding and Manual handoff stay in the Side Panel.

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
- live workspace / pane lifecycle;
- agent status presentation;
- explicit pinned target;
- stale-target fail-closed behavior;
- bounded state / output reads;
- mutation risk classification;
- preview-only action descriptors;
- en / zh / ja UI.

It currently does **not** execute:

- agent Prompt from the Side Panel;
- provider Steer;
- arbitrary Herdr mutation;
- terminal text / input / key writes;
- interrupt.

Any future action moving from preview to executable must pass its own reliability, safety, and real-browser UAT. The presence of a tab in the UI is not authorization to enable it.

## Related documentation

- [Browser extension overview](extension.md)
- [Browser continuity](browser-continuity.md)
- [Auto-continue, recovery and handoff](extension-wake.md)
- [JSON → MCP bridge](extension-bridge.md)
- [Troubleshooting](troubleshooting.md)
