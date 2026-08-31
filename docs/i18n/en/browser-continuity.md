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

`v0.4.2` solves the first durability problem: keep recording working turns so a dead tab, extension reload, runtime restart, or conversation rollover does not erase the only recoverable context. Continuity 2.0 is a formal post-`v0.4.2` roadmap target for a different problem: keeping recovery context small even when one work chain spans days, hundreds of turns, or longer. It is not part of the `v0.4.2` scope and is not assigned to a concrete release number yet; its release assignment remains a separate post-`v0.4.2` planning decision.

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
- [JSON → MCP bridge](extension.md) — a local tool compatibility path for z.ai / DeepSeek when the site does not expose a native MCP Connector.

They share Native Messaging and local IPC, but they are not one state machine. Continuity decides how a Web conversation persists; Control Center presents local truth and explicit human targeting; JSON → MCP adapts tool protocol.

## The mental model

Think of Herdr as the persistent workshop, MCP as the remote-control cable, and browser continuity as the return signal that tells the Web planner when the workshop changed.

With all three pieces in place, a long coding task no longer has to fit inside one synchronous browser turn.

Next:

- [Browser extension](extension.md) — installation and the full browser-product overview
- [Browser Control Center](browser-control-center.md) — Side Panel live state and explicit targeting
- [Wake, recovery and handoff](browser-continuity.md) — continuity state machine and current behavior
- [JSON → MCP bridge](extension.md) — z.ai / DeepSeek compatibility path

## Continuity implementation and recovery details

> **Role:** advanced reference for the browser continuity state machine. Most users only need [Browser Extension](extension.md) and [Browser Control Center](browser-control-center.md).

This page describes the browser continuity state machine: how the extension wakes the correct Web conversation when Herdr work continues after a browser turn ends, how it recovers stalled ChatGPT views, and how it hands a long conversation to a fresh one without duplicating mutations.

Read [Browser Continuity](browser-continuity.md) first for the architectural motivation. Installation and HUD usage are covered in [Browser Extension](extension.md).

## Bind a Project or conversation to a workspace

Continuity binds a browser scope to a Herdr **workspace**, not to a single agent. For normal conversations that scope is the conversation itself. For ChatGPT Projects it is the stable `project_id`.

```text
ChatGPT conversation
        │ binding
        ▼
Herdr workspace
  ├─ implementation agent
  ├─ test process
  ├─ server/log pane
  └─ review agent
```

`workspace_id` is the stable identity. The label is presentation metadata and is refreshed from the live catalog.

Since 0.1.59, a ChatGPT workspace can be bound directly from `https://chatgpt.com/g/<project>/project` before any conversation exists. The Project binding persists independently; a concrete active `/c/<id>` becomes only its `active_conv_key` delivery target. `https://chatgpt.com/` can also hold a tab-scoped pending binding which migrates once when that tab first enters a Project or conversation. Root and Project-home pages expose binding controls but do not run conversation-only Continue/LLM/recovery/rollover actions.

## working, progress and settled

The extension observes local `/push/events` through the trusted Native Messaging path.

### working

When any relevant agent in the bound workspace becomes working, the extension arms progress observation.

### progress

`progressTickSec` controls how often progress is **checked**, not how often a message is guaranteed to be sent.

A progress message is sent when there is meaningful new output, or after the configured fallback interval. The last sent summary and timestamp are persisted so a Service Worker restart does not cause repeated notifications.

### settled

If one pane settles while other agents in the workspace are still working, that is partial progress. The workspace is considered settled only when the relevant working set becomes empty.

A settled event wakes the Web planner so it can inspect Git, tests and agent output. The event itself is not proof that business acceptance criteria passed.

## Manual HUD actions

The HUD can expose:

- **Continue** — send a simple continuation to the current Web conversation;
- **Herdr monitor** — inspect the bound workspace before continuing;
- **LLM analysis** — ask a small configured model whether the latest reply is clearly unfinished;

The HUD exposes Continue / Check Herdr / LLM decide plus Manual handoff. The first three page-scoped progression actions are locked while Auto is on; Manual handoff remains available when its safety gates pass. Workspace binding and local Herdr controls stay in the Side Panel. Manual handoff has a single UI entry in the **HUD**, pauses source automatic wakes during transfer, and makes the target inherit the source Auto state.

## Queue: explicit next-turn user intent comes before auto-continue

**Queue** beside the ChatGPT composer is different from the HUD's manual Continue action.

Queue is for the case where the assistant is still replying but the user already knows what the next instruction should be. Clicking it does not interrupt the live turn; it persists the current composer text for that conversation.

When the turn settles, ordering is:

```text
current assistant turn ends
       ↓
queued content? ── yes ──► merge and send the next user message
       │ no
       ▼
then consider generic LLM auto-continue / idle nudge
```

This priority is deliberate: **an explicit next-turn user instruction outranks the model deciding for itself whether to continue.**

The queue also follows these bounds:

- entries preserve insertion order and merge with blank lines;
- a `turn-in-progress` or other blocked delivery does not ACK or drop content;
- only a confirmed delivered batch is removed;
- entry count, per-entry length, and merged length are bounded;
- right-click Queue to clear the current conversation queue;
- click with an empty composer to retry a still-pending batch;
- after handoff cutover is confirmed, pending content migrates to the target conversation in the same order.

Queue does not execute a Herdr tool or change workspace binding. It only preserves and delivers **the next user message**.

## Automation scope

### ChatGPT Projects

Project automation requires both:

1. global permission for ChatGPT Project automation in Options;
2. the current Project HUD set to Auto on.

The preference is keyed by stable `project_id`, so a handoff conversation in the same Project can inherit the Project automation setting.

### Normal ChatGPT / z.ai / DeepSeek

Where supported, these use conversation-scoped Auto.

z.ai and DeepSeek Auto only performs Herdr progress/settled wake behavior. ChatGPT-specific stale-view recovery, permission-card handling, end-of-turn LLM judgement and automatic rollover are not treated as generic capabilities.

## End-of-turn LLM judgement

A ChatGPT reply can be syntactically finished while semantically unfinished: for example, it may say that tests still need to run or that the next step is to inspect Git.

An optional small model can answer one narrow question: **does this turn clearly need to continue?**

It is not a second planner. It does not choose implementation strategy. If configured and the judgement says continue, the extension submits a bounded continuation message.

Without the small model, automatic turn judgement does not silently fall back to broad keyword guessing. Manual controls remain available.

## Recovery is evidence-first

A stalled browser does not mean the server never accepted the request.

The user message, tool mutations or assistant response may already have progressed server-side while the DOM stayed stale.

The recovery sequence is therefore:

```text
browser appears stalled
        │
        ▼
best-effort same-origin conversation snapshot
        │
        ├─ server ahead ───────► safe reload
        ├─ request not accepted ► bounded Retry
        ├─ server stalled ─────► wait, then one reload
        └─ unknown ────────────► fail closed
```

Unknown delivery never justifies blindly resending the original task.

## Stale view: server ahead of the DOM

The extension can compare the last visible assistant message with the same-origin conversation snapshot: message identity, text length, completion state and update time.

- **server ahead** — the server has a newer or longer message; reload once to synchronize the view;
- **server stalled** — the server itself still shows an incomplete assistant turn with no progress; wait conservatively before one reload;
- **synced** — server and DOM agree; no recovery action;
- **unknown** — snapshot unavailable or ambiguous; fail closed.

Reloading is meant to reveal an existing server-side turn, not to resubmit the user's task.

## Explicit send-timeout errors

When ChatGPT renders a send-timeout/thread-error card, the extension first checks server-side conversation state.

- If `current_node` already moved to an assistant message, the request was accepted; Retry could duplicate tool work, so the safer action is a view reload.
- If `current_node` is still the user message, ChatGPT's own Retry may be used once.
- If delivery cannot be determined, prefer bounded view synchronization over creating another user turn.

Retry and reload budgets are finite. Exhausted recovery becomes an explicit failure/rollover recommendation rather than an infinite loop.

## Interrupted response streams

If the assistant started responding but the page reports a disconnected stream, the error placeholder itself is not progress. Only assistant text growth or signature change advances the progress clock.

After a conservative stall window and only when the page is otherwise safe, the extension may reload once to resynchronize the existing server-side turn. It does not resend the original task.

## Page-health self-recovery: stalls, memory and 429

Version 0.1.63 connects the previously diagnostic-only UI-pressure meter to a strictly bounded page-health recovery layer. It does not delete ChatGPT/React-owned history DOM. Removing those nodes behind React can desynchronize the framework tree, event handlers, virtualization state and the real DOM; when the whole page runtime needs reclamation, a controlled reload is safer because it rebuilds the document, React tree and JS heap together.

The fixed-window O(1) signals are MutationObserver callback rate, watcher tick rate, timer drift, Long Tasks and, when Chromium exposes it, JS heap usage. A single spike is observational only.

- An active turn becomes reload-eligible only after sustained page pressure plus a real assistant stall, and only when the same-origin conversation snapshot proves `current_node` is a **finished assistant**. That proof is what allows a stale Stop/streaming UI bit to be ignored as a renderer problem rather than live work.
- Critical heap pressure is eligible only after a quiescent interval. Manual composer text, tool execution, permission cards and uncertain delivery always block reload.
- Level one is at most one durable `location.reload()`. If the same health failure survives that refresh, level two is at most one sender-scoped `chrome.tabs.reload(tabId)`. The background worker accepts only its actual `sender.tab`, the same conversation, Auto enabled and a matching durable pending record, then persists executed-at before navigation so MV3 worker restart cannot create a reload loop.
- Exhausting both levels stops further reloads and recommends controlled conversation rollover instead.

HTTP 429 is the opposite kind of signal: **429 is backoff-only and never a Retry/reload trigger.** A visible rate-limit error or a Resource Timing 429 enters a `30s → 60s → 120s` capped cooldown. Automatic recovery does not create additional page/API/attachment traffic during that cooldown, avoiding a rate-limit amplification loop.

## Context pressure and automatic rollover

Long Herdr sessions can accumulate visible text, MCP payloads, Project instructions and hidden system context. ChatGPT may also virtualize old DOM nodes, so a short current DOM is not evidence of a short conversation.

The extension uses conservative pressure signals:

- approximate visible user/assistant tokens;
- maximum absolute `conversation-turn-N` index still observable;
- a persisted monotonic message-count floor;
- reserved headroom for Project/system/tool payloads not visible in the page.

High pressure only makes rollover eligible. Automatic handoff still requires a safe boundary: Project Auto on, bound workspace not working, no stream/tool/permission card, no unsent manual draft, no uncertain delivery and no other handoff in progress.

## Fail-closed handoff

```text
old Project conversation
        │ generate compact packet with transfer id
        ▼
new conversation in the same Project
        │ submit seed packet
        ▼
verify new conversation id + seed marker
        │
        └── only then switch Project active_conv_key
```

The Project/workspace binding and `continuity_id` remain stable. The old active conversation remains authoritative until the new conversation is verified. z.ai is still conversation-scoped, so its binding moves only after the target seed is confirmed.

Opening a new tab is not enough. Attempting to send the seed is not enough. If seed delivery is uncertain, the old binding stays in place and the transfer remains recoverable.

## What belongs in a handoff packet

A useful packet preserves:

- current objective;
- completed work;
- important decisions;
- incomplete work;
- known workspace/path/branch/commit/task identifiers;
- safety constraints;
- recommended next actions.

It does **not** certify that runtime or Git state is still current. The fresh conversation must re-inspect live Herdr/Git/runtime state before mutation.

## Manual handoff

Manual handoff is useful at a natural work boundary before the current conversation becomes difficult to manage. Start it from the **in-page HUD**; the Side Panel does not duplicate this conversation action.

It is supported for bound ChatGPT Project conversations and stable z.ai `/c/<chat_id>` conversations. Manual handoff can start with Auto on or off; the target conversation inherits the source Auto state. Automatic wakes from the source pause while the transfer is active. For ChatGPT, cutover changes only the Project binding's active conversation target; for z.ai it migrates the conversation-scoped binding. The workspace must not have active working agents, so settled/wake delivery cannot race the cutover.

Normally the current web model creates the handoff packet because it has the richest conversation context. If the page already signals a hard conversation limit, the handoff prompt cannot be submitted, or a settled primary summary does not arrive within the bounded grace period, Herdr uses the configured OpenAI-compatible LLM as a fallback. The fallback receives a bounded user/assistant source transcript, must produce the same validated `HERDR_HANDOFF_V1` packet, and then rejoins the existing target/seed/binding/continuity commit path.

z.ai summary/seed control messages use a raw channel so they are not wrapped again as JSON→MCP coding tasks.

## Why Auto defaults off

Continuity can actively submit messages, handle some page actions and change conversation identity. New scopes therefore default to Auto off.

The intended progression is: **observe first, then automate.**

## Validate continuity with a real task

A meaningful UAT should verify:

1. the intended browser scope is bound to the intended workspace; for ChatGPT Projects, verify the stable Project binding and its active conversation target separately;
2. Auto is enabled for the right scope;
3. a real agent task enters working;
4. new output produces a progress wake without spam;
5. workspace settle wakes the Web planner;
6. reload/browser restart preserves the correct binding;
7. an explicit manual handoff changes the ChatGPT Project active target only after the new seed is verified (or migrates the binding after confirmation on conversation-scoped sites).

Implementation history belongs in [CHANGELOG](../../../CHANGELOG.md). This page describes current behavior.
