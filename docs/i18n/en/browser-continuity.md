# Browser Continuity: why MCP alone is not enough

MCP gives a Web AI a way to call tools on the workstation. It does not guarantee that a browser conversation will keep running after a long local task finishes.

That distinction matters for Herdr workflows.

```text
MCP direction
Web AI ─────────────► workstation

Continuity direction
workstation ────────► browser conversation
```

A durable AI development workflow needs both directions.

## The long-task gap

Consider a typical task:

1. ChatGPT inspects a repository.
2. It dispatches a focused implementation task to a Herdr agent.
3. The tool call returns quickly: the job was accepted.
4. The browser turn ends.
5. The local agent keeps working for another 20 minutes.
6. The agent finishes.

At step 6, nothing in standard request-driven MCP automatically starts a new ChatGPT turn. The local machine knows that work finished; the browser does not.

This is the gap the extension closes.

## Continuity is not another agent

The browser extension does not replace ChatGPT, Herdr or the local worker. It does not own project planning or code-generation policy.

Its job is narrower:

- bind a browser scope to a Herdr workspace — a stable ChatGPT Project where available, otherwise a concrete conversation;
- observe local progress and settled state;
- feed useful progress back into the right conversation;
- recover a browser view that stalled or lost part of a response;
- hand a very long conversation to a fresh conversation safely;
- keep the right browser conversation connected to that local work over time.

The Web model remains the planner. Herdr remains the runtime truth. The extension keeps the two sides connected over time.

## Why the binding is a workspace, not an agent

Real work often involves more than one pane:

```text
workspace: my-project
  ├─ pane: pi implementing a fix
  ├─ pane: test suite
  ├─ pane: local server
  └─ pane: grok reviewing the diff
```

Binding the browser to one agent would lose the rest of the project state. A workspace is the better unit of continuity because it represents the complete local work area.

The binding stores the stable workspace identity; the label is only presentation data and can be refreshed from the live workspace catalog. ChatGPT adds one more level: a Project binding is keyed by stable `project_id`, while `active_conv_key` points at the concrete conversation that should receive messages. This lets the user bind from the Project home before creating a chat and lets rollover keep the binding itself stable.

## Progress and settled events

During long work, the extension can send progress updates when there is meaningful new local output. When the bound workspace settles, it can wake the Web conversation once so the planner can inspect results and decide what to do next.

The important rule is to avoid turning continuity into notification spam. Progress checks and progress sends are different concepts: the extension may check frequently but only send when there is new information or a configured fallback interval has elapsed.

## Browser recovery

A Web AI turn can also fail on the browser side while the server-side conversation has already advanced. Examples include:

- an explicit send-timeout error;
- a response stream that disconnected;
- the page showing only a stale partial answer;
- the server having a newer assistant message than the current DOM.

Blindly resending the user task is dangerous because tool mutations may already have happened.

The recovery policy is therefore evidence-first:

```text
error / stale view
      ↓
inspect same-origin conversation state when possible
      ↓
server already advanced? → refresh view
request clearly not accepted? → bounded retry
unknown delivery? → fail closed
      ↓
only after recovery is exhausted consider handoff
```

The goal is not maximum automation. The goal is preventing duplicate work while still recovering from ordinary browser failures.

## Conversation rollover

A productive Herdr session can outlive one ChatGPT conversation. Tool calls, project instructions and visible text all consume context, while the browser may virtualize older messages and stop keeping the entire history in the DOM.

From 0.4.2, finalized user and assistant turns from a bound conversation are appended incrementally through Native Messaging into the Rust `state.db` Continuity Journal. Before an ID-only rollover, the extension performs a live Rust resolve for the current `continuity_id`. When that resolve succeeds, the fresh conversation receives only the compact continuity reference, calls the existing `herdr_call(method="continuity.resume", ...)` path to recover a bounded recent working tail, and then re-inspects Herdr, Git and relevant services before mutation.

A user who **manually** starts another conversation inside the same bound ChatGPT Project does not need to remember or type the `continuity_id`. The first accepted user turn in the new conversation (for example, “continue”) is journaled through the existing Project binding onto the same continuity chain. When the Web planner sees prior-work intent such as “continue”, “resume”, or “where did we leave off?”, it searches with `continuity.resolve` / `continuity.search` before asking the user for an internal ID. Automatic `continuity.resume` is allowed only when stable conversation/project/workspace identity yields exactly one active chain. A text-only match remains confirmation-required even when it returns a single candidate; a generic word such as “continue” is a search trigger, not selection evidence. Ambiguous results expose only bounded title/workspace/update-time and recent-turn evidence for user confirmation, and Herdr never chooses by newest-or-most-similar heuristics.

The continuity id identifies one stable work chain across conversations. On ChatGPT Projects the Project/workspace binding and continuity id stay in place while only the confirmed active conversation target changes.

If the local journal is unavailable or the live Rust resolve fails, the extension keeps the existing semantic handoff path as a compatibility fallback: the source conversation or configured fallback LLM produces a validated `HERDR_HANDOFF_V1` packet from bounded source context, the target is seeded, and cutover happens only after target confirmation.

This preserves continuity without making the dying source page responsible for the only recoverable copy of working state.

### Future target: Continuity 2.0

`v0.4.2` solves the first durability problem: keep recording working turns so a dead tab, extension reload, runtime restart, or conversation rollover does not erase the only recoverable context. Continuity 2.0 is a formal post-`v0.4.2` roadmap target for a different problem: keeping recovery context small even when one work chain spans days, hundreds of turns, or longer. It is not part of the `v0.4.2` scope and is not assigned to a concrete release number yet; release planning should happen after `v0.4.2` ships and dogfood data is available.

Continuity 2.0 will incrementally compact older raw turns into rolling semantic checkpoints that preserve objectives, completed work, decisions, constraints, active files/branches/commits, pending work, next actions, and literal anchors. Resume should then consume the latest verified checkpoint plus a recent raw tail rather than replaying the full long conversation. Old raw bodies may be reclaimed only after a replacement checkpoint has been generated and verified. Browser memory, long-DOM cost, and main-thread/render pressure also become rollover inputs alongside model context pressure.

The implementation order remains `Reliability Kernel → Continuity 2.0`. The Reliability Kernel provides operation identity, idempotency, delivery phases, and uncertain-result reconciliation for checkpoint generation, ACK, and raw-journal retention. The detailed design remains in [Phase 8 of the Rust Native Rearchitecture document](../../history/architecture/rust-native-rearchitecture.md#phase-8continuity-20).

## Manual and automatic control

Automation is scoped deliberately.

For ChatGPT Projects, automation can be shared at the Project level when the global Project permission is enabled. Normal ChatGPT conversations, z.ai and DeepSeek use conversation-level settings where supported.

New scopes default to Auto off. The HUD's three preset progression actions are mutually exclusive with automatic progression so the same conversation is not advanced by two paths at once. Manual handoff is the deliberate exception and has one UI entry in the **HUD**: where supported, it can start with Auto on or off, pauses automatic wakes from the source during transfer, and makes the target inherit the source Auto state.

Manual takeover remains important. The user can turn Auto off to continue manually, extract Herdr status, or run the lightweight LLM judge from the HUD; an explicit handoff starts from Control Center where supported.

## Why local Native Messaging is used

The extension should not need the workstation bearer in browser storage.

The primary path is:

```text
content script
  ↓
extension service worker
  ↓ Chrome Native Messaging
native host
  ↓ local Unix socket (0600)
herdr-mcp runtime
```

The browser does not route continuity traffic through the public Cloudflare Edge. This keeps public OAuth identity and local extension trust as separate security boundaries.

## Continuity is one extension surface, not the whole browser product

This page focuses on **Web continuity**: workspace binding, progress / settled push-back, stale-view recovery, and handoff / rollover.

The extension now has two other product surfaces with different responsibilities:

- [Browser Control Center](browser-control-center.md) — live workspace / pane / agent observation, explicit Pinned Target, and bounded reads in Chrome Side Panel;
- [JSON → MCP bridge](extension-bridge.md) — a local tool compatibility path for z.ai / DeepSeek when the site does not expose a native MCP Connector.

They share Native Messaging and local IPC, but they are not one state machine. Continuity decides how a Web conversation persists; Control Center presents local truth and explicit human targeting; JSON → MCP adapts tool protocol.

## The mental model

Think of Herdr as the persistent workshop, MCP as the remote-control cable, and browser continuity as the return signal that tells the Web planner when the workshop changed.

With all three pieces in place, a long coding task no longer has to fit inside one synchronous browser turn.

Next:

- [Browser extension](extension.md) — installation and the full browser-product overview
- [Browser Control Center](browser-control-center.md) — Side Panel live state and explicit targeting
- [Wake, recovery and handoff](extension-wake.md) — continuity state machine and current behavior
- [JSON → MCP bridge](extension-bridge.md) — z.ai / DeepSeek compatibility path
