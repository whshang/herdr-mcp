# Browser Control Center: observe the real Herdr workspace from Chrome Side Panel

The Browser Control Center brings Herdr workspace, pane, terminal, and agent state into Chrome Side Panel.

It is not a shortcut for giving the browser unrestricted shell access. It solves a more fundamental problem:

> When Web AI, local agents, tests, and terminals are all active, how do you keep the real workstation state visible and make the next human control target explicit?

The current Control Center is **live observation + explicit targeting + bounded reads**. Mutation surfaces remain preview-only, so the presence of Prompt, Steer, Herdr, and Terminal tabs does not bypass existing mutation safety boundaries.

## How it differs from browser continuity

The extension now has two related but distinct operational surfaces:

| Surface | Main question | Entry point |
|---|---|---|
| HUD / Continuity | How should this Web conversation bind, continue, recover, or hand off? | Inside ChatGPT / z.ai / DeepSeek |
| Control Center | What is happening in local workspaces and which pane is the explicit human target? | Chrome Side Panel |

The HUD is about **how this browser conversation continues**.

The Control Center is about **what the real local worksite looks like now**.

They share the same Native Messaging and local-IPC trust path, but they are not one state machine.

## Open the Control Center

Click the Herdr extension icon in the browser toolbar. The Popup exposes:

```text
Browser Control Center
Live workspaces · explicit pane target
[Open]
```

Chrome then opens the Side Panel.

The panel follows the same extension language setting as Popup and Options. Current UI locales are:

- English;
- Simplified Chinese;
- Japanese.

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

## What the workspace / pane tree shows

The panel groups panes under their workspace and currently shows:

- workspace label / id;
- pane id;
- agent name or terminal-only state;
- working / idle / done / blocked status;
- current Herdr focus marker;
- cwd / project root;
- agent elapsed time;
- recent activity;
- a bounded recent summary or terminal title.

Workspaces containing active work sort first. Initial expansion is bounded so a workstation with many projects does not open as an unreadable wall of rows.

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

## Why Prompt / Steer / Herdr / Terminal say “Action preview”

The UI already models future control actions, but **those mutations are not enabled in the current release**.

The panel says this directly:

> Live state · preview-only controls

The `Action preview` section contains four modes:

| Mode | Intended future action | Current behavior |
|---|---|---|
| Prompt | Prompt the pinned agent | Build a descriptor only; nothing is sent |
| Steer | Steer a provider / agent | Build a descriptor only; nothing is sent |
| Herdr | Invoke a Herdr mutation method | Build a descriptor only; nothing executes |
| Terminal | Write terminal text / input | Build a descriptor only; nothing is written |

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
Control Center
  observe local workspace / pane / agent truth
  ↓
HUD
  manage current Project / conversation binding and Auto state
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
6. For long work, HUD progress / settled / recovery / handoff maintains continuity.

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
