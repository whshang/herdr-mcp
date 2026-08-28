# Auto-continue, recovery and conversation handoff

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

The HUD exposes only Continue / Herdr monitor / LLM analysis; these manual progression actions are locked while Auto is on. Manual handoff has a single UI entry in **Control Center → Current page** and is the deliberate exception: it can start with Auto on or off on supported conversations, pauses source automatic wakes during transfer, and makes the target inherit the source Auto state.

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

Manual handoff is useful at a natural work boundary before the current conversation becomes difficult to manage. Start it from **Control Center → Current page**; it is intentionally not duplicated in the HUD.

It is supported for bound ChatGPT Project conversations and stable z.ai `/c/<chat_id>` conversations. Manual handoff can start with Auto on or off; the target conversation inherits the source Auto state. Automatic wakes from the source pause while the transfer is active. For ChatGPT, cutover changes only the Project binding's active conversation target; for z.ai it migrates the conversation-scoped binding. The workspace must not have active working agents, so settled/wake delivery cannot race the cutover.

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
