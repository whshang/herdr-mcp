# Browser extension

Audience: extension authors and users. Chrome display name **herdr → Web wake**. `extension/` keeps one clear boundary: it keeps web conversations connected to local Herdr work over time; it is not another Agent runtime, memory system, or orchestration platform. JSON→MCP is a separate compatibility track.
Language: product UI is en / Simplified Chinese / Japanese (same as herdr); this page is written in English. Repo-root README: [en](../../../README.md) / [zh](../../../README.zh.md) / [ja](../../../README.ja.md).

| Track | Problem | Direction | Status | First sites |
|---|---|---|---|---|
| **A. Web work continuity** | web work stalls after dispatch; replies can timeout or freeze halfway; long conversations need a fresh chat | Herdr observation + conversation binding + manual continue + automation gates + safe handoff | **usable** (0.1.52 series) | binding/observation: 4 sites; shared automation: ChatGPT Project; conversation-scoped automation: plain ChatGPT / z.ai / DeepSeek; Manual handoff: ChatGPT Project + z.ai `/c/<chat_id>` |
| **B. JSON→MCP** | DeepSeek / z.ai web has no MCP Connector | web → extension service worker → Native Messaging host → trusted local `/mcp` IPC | **usable** (bounded `tools/list` / `tools/call` loop) | `chat.deepseek.com`, `chat.z.ai` |

Shared: same extension, same Native Messaging + local Unix IPC transport, same options.
Deployment boundary: the extension never goes through the public Worker/Tunnel. Current builds send bounded requests/streams to the Chrome Native Messaging host, which reaches local `/push/*` and `/mcp` through `~/.config/herdr-mcp/extension.sock` (mode `0600`). The browser receives no Herdr bearer and current Options has no Herdr Token field. Old-version bearer compatibility remains inside the native host/server. A plain ChatGPT `/c/<id>` is identified by its conversation id; a Project conversation is identified by stable `g-p-<resource-id> + conversation id`. Persisted z.ai conversations use `/c/<chat_id>`; the `/` route is only the new-chat launcher. SPA route changes re-register automatically without requiring a full page refresh.

The transport layer keeps exactly **1 global `/push/events` stream** open through one persistent Native Messaging port; the host owns one local SSE connection over Unix IPC. All workspace events are dispatched by background to the corresponding binding based on the event's `workspace` field. Do not create one Native Messaging/SSE stream per binding: historical bindings would multiply native host processes and local streams for no benefit.

Chrome 145+ splits local loopback access into a `loopback-network` permission (shown in the UI as "Applications on your device"). The extension still declares the `http://127.0.0.1:8772/*` host permission, but some Chrome/Chromium profiles may still keep loopback permission as "Ask". Options "Test connection" and the page HUD both use bounded requests: if a local-service request is permission-gated, it clearly tells you to go to "Manage extensions → Site settings → Applications on your device → Allow" instead of loading forever.

Per-track docs: [extension-wake.md](./extension-wake.md) (A), [extension-bridge.md](./extension-bridge.md) (B).

```text
┌─────────────┐  MCP Connector / JSON bridge  ┌──────────┐
│ Web AI       │ ────────────────────────────► │ herdr-mcp│──► herdr
│ ChatGPT etc. │ ◄── A progress/done push-back │  /push   │
│ DeepSeek etc.│ ── B SpeaksJSON→/mcp ───────► │  /mcp    │
└─────────────┘                                └──────────┘
```

## A. Web work continuity

This track does not make the extension think on behalf of the web model. It solves two continuity problems:

1. **Local work continuity**: when a Herdr workspace changes state, the original web conversation can be woken and continue orchestration.
2. **Web conversation continuity**: when a ChatGPT Project conversation gets long, compact the current working state into a handoff packet, start a fresh conversation in the same Project, and move the existing workspace binding.

### A1. Proactive progress reminders (site-wide)

### What exists

- Binding: the popup binds the "current conversation" to a herdr **workspace** (multiple agents in one space can push back in parallel)
- SSE: `/push/events?workspace=...`
- Policy: after seeing `working`; partial settle reports progress, all-idle in scope reports done; `hello` can backfill
- chatgpt.com in-page permission-card handling is part of Project automation: it auto-clicks an explicit Allow only when per-Project automation is enabled globally and the current Project HUD is `Auto on`
- Optional small-model nudge after a ChatGPT turn ends runs in automation mode (Options configures Base URL + Key + Model; cooldown and progress checks share `progressTickSec`)
- Reply timeout recovery, stale-view freshness checks, safe reload, and fail-closed Project rollover can run automatically when automation is enabled and safety gates pass
- UI: en / Simplified Chinese / Japanese

### Gaps

1. ~~periodic progress announcements while working~~ (implemented, see below)
2. binding friction: no binding → zero push-back
3. agent-less `herdr_exec` utilities do not enter the agent state machine (optional follow-up; workspace binding does not cover utilities either)
4. ~~ChatGPT turn nudge~~ (extension ≥0.1.18: small-model decision only; Options pre-fills prompt / do-not-send words; submit the model's original text when continuing)

### Implemented (track A periodic progress announcements)

- Options gates only **ChatGPT Project automation** globally; `progressTickSec` (default `60` seconds: working progress checks + turn-nudge cooldown; `0` disables only those interval-driven actions), `progressFallbackSec` (default `1200` seconds fallback with no new summary), separate progress and done templates (`progressTemplate` / `wakeTemplate`); UI languages en / Simplified Chinese / Japanese. Plain ChatGPT `/c/<id>`, z.ai, and DeepSeek conversation-scoped Auto remain available independently of that Project gate.
- actual send rule: at a checkpoint, only push to the web when the herdr summary has **new non-empty content** since the last actual send; otherwise wait until `progressFallbackSec` and send one fallback, avoiding empty-spin spam
- `working`: check the summary every `progressTickSec`; push progress into the bound conversation and submit only on new non-empty content or when `progressFallbackSec` is reached; one timer per convKey; repeated `working` does not lose the last-sent baseline
- popup lists by **workspace** (incl. panes with terminal only, no running agent); one title line + one pane-stats line, no repeated project names, no `agent@pane` listing
- `settled`: cancel the tick timer first, then wake once with the done template
- all four sites retain workspace binding, state observation and injector foundations. ChatGPT Project settings are keyed by stable `project_id`; plain ChatGPT `/c/<id>`, z.ai, and DeepSeek use the current conversation key. z.ai / DeepSeek `Auto on` enables only Herdr progress/settled push-back; ChatGPT-only stale-view recovery, turn LLM decisions and automatic rollover remain ChatGPT-scoped.

### Project-shared automation and conversation Auto

The Options master switch gates **ChatGPT Project-shared automation only**:

- With the Project gate off, ChatGPT Projects do not expose/execute shared Auto, but observation, bindings, and manual controls remain. Plain ChatGPT `/c/<id>`, z.ai, and DeepSeek still expose their own conversation-scoped Auto switch.
- With the Project gate on, a ChatGPT Project can turn Auto on/off and shares that preference by stable `project_id` across sibling and rollover conversations. A new Project still defaults off.
- Plain ChatGPT, z.ai, and DeepSeek always store Auto by conversation key, default off, and do not need to belong to a ChatGPT Project. Since 0.1.48, z.ai / DeepSeek may save the preference before binding a workspace; workspace-dependent progress/settled push-back starts after binding exists.

Supported scopes keep frequent actions on the bottom HUD: **Manual continue / Herdr monitor / LLM analysis / Manual handoff (when supported) / Automation on-off / expand**. ChatGPT Project conversations and persisted z.ai `/c/<chat_id>` conversations can show Manual handoff; the z.ai `/` launcher and DeepSeek do not. The drawer only contains low-frequency settings: event timing, conversation bindings, and advanced options.

Starting with 0.1.45, an effectively **`Auto on`** Project gives the entire persistent bottom HUD a restrained light-green surface, green top border, and soft green shadow. `Auto off` and global manual mode keep the neutral treatment. This color is only an automation-state cue: orange/red runtime states such as `working`, `blocked`, `recovering`, and `failed` keep their own semantic colors. Dark mode uses a corresponding low-glare green surface.

**Automation on** is scoped to the current stable ChatGPT `project_id`. All conversations in that Project, including a fresh conversation created by rollover, share the same setting. It enables:

- Herdr workspace progress checks and settled wakes
- post-turn LLM analysis and continue submission
- stalled reply recovery probes plus stale-view comparison against ChatGPT's same-origin conversation snapshot; a proven server-ahead view is refreshed once, while a server-side unfinished message must remain stalled before reload is allowed
- same-Project fail-closed rollover after recovery exhaustion or high context pressure
- in-page ChatGPT permission-card handling as part of the current Project automation state; there is no separate permission-card toggle

**Automation off** keeps observation active but stops automatic mutations for that Project/conversation. The Options Project gate adds that stop only to ChatGPT Project-shared automation; it does not disable plain ChatGPT, z.ai, or DeepSeek conversation Auto and does not delete any saved scope preference. Manual HUD actions are available only while their current scope is `Auto off`.

**Manual handoff** supports bound ChatGPT Project conversations and persisted z.ai `/c/<chat_id>` conversations, but the current scope must first be switched to `Auto off`. The HUD locks the button while automation is on, and background independently rejects `automation_enabled` if the UI is bypassed. The same fail-closed state machine asks the current web model for a marked transfer packet, opens a fresh target conversation, seeds it, and moves workspace bindings only after a new conversation id and seed marker are confirmed. z.ai summary/seed control messages use the raw send path so the JSON bridge cannot rewrite them into agent tasks. A bound workspace that is still `working` blocks the operation.

### Page freshness / stale-view recovery

0.1.44 separates “ChatGPT never replied” from “the server has newer conversation state but this tab is showing stale DOM.” Both human-submitted and extension-submitted user turns enter the health state machine. Once a recent turn has been idle long enough, the content script best-effort fetches the current same-origin conversation snapshot, follows `current_node` to the latest assistant message, and compares its message id, text, completion status and update time with the last assistant message visible in the page.

- **server ahead**: a newer assistant message id exists, or the server copy of the same message is clearly longer than the DOM copy → refresh once when the composer/tool/permission safety gates allow it;
- **server stalled**: the server explicitly says the assistant message is unfinished and it has not advanced for at least 60 seconds; if the page still claims streaming, wait another 30 seconds before allowing refresh;
- **synced**: server and DOM agree and the server message is finished → do not refresh;
- **unknown**: the private same-origin snapshot endpoint is unavailable, times out, or changes shape → fail closed; never refresh just because the probe failed.

Before reload the extension persists the last assistant signature. After reload, newer content or resumed streaming ends recovery immediately. If the same partial answer is still present after 10 seconds and the page is otherwise safe, the extension submits one localized browser-recovery activation message telling ChatGPT to reread the current conversation state, continue from the actual stop point, and not replay completed mutations. Only if that activation also fails does the existing recovery-exhausted rollover path become eligible.

### A2. Long-conversation compression and rollover (ChatGPT Project)

Extension 0.1.39 adds **Rollover**; 0.1.43 adds the Project automation gate; 0.1.44 adds stale-view recovery; 0.1.46 exposes Manual handoff; 0.1.47 extends explicit handoff to persisted z.ai chats and requires `Auto off` before any manual handoff. ChatGPT automatic recovery/rollover remains Project-only.

Flow:

```text
old Project conversation (workspace binding remains authoritative)
  -> current ChatGPT uses the full old context to produce a marked compact handoff packet
  -> extension opens a fresh conversation entry in the same ChatGPT Project
  -> handoff packet is submitted as the first user message in the new conversation
  -> extension confirms the page has a new /c/<conversation-id> and that the seed message really exists
  -> only then atomically move workspace bindings from the old convKey to the new convKey
  -> future Herdr wakes target the new conversation only
```

"Compression" here means **semantic rollover**: the fresh conversation receives only the compact working state it needs instead of carrying the full old conversation context. The extension does not and cannot change ChatGPT's internal context-compaction algorithm.

Safety boundary:

- available only for an **already-bound ChatGPT Project conversation**; plain chats can be considered later;
- rollover is rejected while an agent in a bound workspace is still `working`, avoiding a race between settle wakes and destination cutover;
- the handoff prompt tells the current web model to summarize only, not continue implementation or call tools, and to use only facts already established in the old conversation;
- the new-chat seed says that live Herdr/runtime/Git state must be verified before any mutation; the handoff is working context, not proof that live state is unchanged;
- **the old binding remains authoritative until the new seed is confirmed**. Opening a new tab, or even attempting the seed submit, is not enough to move bindings;
- uncertain seed delivery records `seed_uncertain` and leaves the old binding untouched. Explicit **Resume handoff** first probes the target marker; if it already exists, cutover completes without replaying the seed; otherwise the user-requested resume may retry it;
- the handoff packet is temporarily stored in `chrome.storage.local`; after successful cutover its body is cleared and only small recovery/diagnostic metadata remains;
- workspace bindings no longer silently expire after 24 hours. They remain explicit until the user unbinds or a successful rollover moves them.

Context pressure is estimated from visible user/assistant text only. It is not the ChatGPT backend token count and it does not persist message bodies. Automatic rollover requires a bound Project conversation, a quiet page, no uncertain delivery, no active tools/streaming, and a valid handoff path.

## B. JSON→MCP (DeepSeek / z.ai)

### Implemented

- the extension service worker sends local `/mcp` `tools/list` / `tools/call` through the Native Messaging host; the host uses trusted Unix IPC, so neither the service worker nor page JavaScript receives a Herdr bearer;
- z.ai / DeepSeek content scripts turn normal user tasks into a Herdr-tool protocol round, extract tool calls, execute controlled sequential rounds until a normal non-JSON answer, and feed results back to the web model;
- intermediate protocol messages are folded; handoff summary/seed messages use a raw channel and bypass the JSON bridge;
- current z.ai 1.1.88 compatibility uses `.user-message` / `.markdown-prose`, the real `#send-message-button`, and `/c/<chat_id>` as the stable persisted-chat identity.

See [extension-bridge.md](./extension-bridge.md).

## What we do not do

- pretend DeepSeek "has" an OAuth connector natively
- send the extension over the public Cloudflare MCP URL by default
- replace track A with track B (ChatGPT already has a connector; the stall problem is solved by A)

## Acceptance mantra

- **A**: ChatGPT Project `Auto on` enables full working/settled, LLM, stale-view and automatic rollover continuity; z.ai / DeepSeek `Auto on` enables the narrower working/settled push-back path. Manual handoff requires `Auto off` and a bound ChatGPT Project or persisted z.ai `/c/<chat_id>` conversation.
- **B**: normal DeepSeek / z.ai tasks can drive local Herdr MCP tools through the JSON bridge, with bounded tool rounds and results fed back to the web model.