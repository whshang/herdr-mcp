# Browser extension

Audience: extension authors and users. Chrome display name **herdr → Web wake**. `extension/` keeps one clear boundary: it keeps web conversations connected to local Herdr work over time; it is not another Agent runtime, memory system, or orchestration platform. JSON→MCP is a separate compatibility track.
Language: product UI is en / Simplified Chinese / Japanese (same as herdr); this page is written in English. Repo-root README: [en](../../../README.md) / [zh](../../../README.zh.md) / [ja](../../../README.ja.md).

| Track | Problem | Direction | Status | First sites |
|---|---|---|---|---|
| **A. Web work continuity** | web work stalls after dispatch; replies can timeout or freeze halfway; long conversations need a fresh chat | Herdr observation + conversation binding + manual continue + ChatGPT Project automation/freshness recovery/rollover | **usable** (0.1.46 series; global run mode + Project automation switch + Manual handoff) | binding/observation: 4 sites; automation/recovery/rollover: ChatGPT Project |
| **B. JSON→MCP** | DeepSeek / z.ai web has no MCP Connector | web → local `127.0.0.1:8772/mcp` | **incomplete** (can extract JSON, MCP not called) | `chat.deepseek.com`, `chat.z.ai` |

Shared: same extension, same static token, same options.  
Deployment boundary: the extension only ever hits local `127.0.0.1:8772` `/push/*` (only future B touches local `/mcp`); it never goes through the public Worker/Tunnel. So Cloudflare Edge, Custom Domain, and contract-epoch changes never require the extension to change URL/OAuth. Track A stays compatible with the newer server. A plain ChatGPT `/c/<id>` is identified by its conversation id; a Project conversation is identified by stable `g-p-<resource-id> + conversation id`, ignoring the human-readable Project slug ChatGPT may append. SPA route changes re-register automatically without requiring a full page refresh.

The transport layer keeps exactly **1 global `/push/events` SSE** open. All workspace events are dispatched by background to the corresponding binding based on the event's `workspace` field; you must not create one SSE per binding, or many historical bindings would exhaust the browser's HTTP/1.1 connection pool to `127.0.0.1:8772`, leaving `/push/state` and `/push/mcp-activity` permanently queued.

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

- Options: global run mode is **Manual globally / Per-Project automation**; `progressTickSec` (default `60` seconds: working progress checks + turn-nudge cooldown; `0` disables only those interval-driven actions), `progressFallbackSec` (default `1200` seconds fallback with no new summary), separate progress and done templates (`progressTemplate` / `wakeTemplate`); UI languages en / Simplified Chinese / Japanese
- actual send rule: at a checkpoint, only push to the web when the herdr summary has **new non-empty content** since the last actual send; otherwise wait until `progressFallbackSec` and send one fallback, avoiding empty-spin spam
- `working`: check the summary every `progressTickSec`; push progress into the bound conversation and submit only on new non-empty content or when `progressFallbackSec` is reached; one timer per convKey; repeated `working` does not lose the last-sent baseline
- popup lists by **workspace** (incl. panes with terminal only, no running agent); one title line + one pane-stats line, no repeated project names, no `agent@pane` listing
- `settled`: cancel the tick timer first, then wake once with the done template
- all four sites retain workspace binding, state observation and injector foundations; the new Per-Project automation mode permits automatic mutations only for ChatGPT Project conversations with a stable `project_id`. Plain ChatGPT `/c/<id>`, Claude, DeepSeek and z.ai remain manual and are not auto-enabled merely because the global mode is Per-Project automation

### Global run mode and Project HUD automation

Options owns the global policy. **Manual globally** disables automatic mutations everywhere and hides the automation switch from ChatGPT Project HUDs; Manual continue, Herdr monitor, and LLM analysis remain available, and Project conversations also keep **Manual handoff** (usable once bound). **Per-Project automation** only permits Projects to use automation: every new ChatGPT Project still defaults to off and must be explicitly enabled from that Project's HUD.

In Per-Project mode, the bottom HUD bar owns frequent actions: **Manual continue / Herdr monitor / LLM analysis / Manual handoff / Automation on-off / expand**. In Manual globally mode, the automation switch is absent but Project conversations still keep Manual handoff. The drawer only contains low-frequency settings: event timing, conversation bindings, and advanced options.

Starting with 0.1.45, an effectively **`Auto on`** Project gives the entire persistent bottom HUD a restrained light-green surface, green top border, and soft green shadow. `Auto off` and global manual mode keep the neutral treatment. This color is only an automation-state cue: orange/red runtime states such as `working`, `blocked`, `recovering`, and `failed` keep their own semantic colors. Dark mode uses a corresponding low-glare green surface.

**Automation on** is scoped to the current stable ChatGPT `project_id`. All conversations in that Project, including a fresh conversation created by rollover, share the same setting. It enables:

- Herdr workspace progress checks and settled wakes
- post-turn LLM analysis and continue submission
- stalled reply recovery probes plus stale-view comparison against ChatGPT's same-origin conversation snapshot; a proven server-ahead view is refreshed once, while a server-side unfinished message must remain stalled before reload is allowed
- same-Project fail-closed rollover after recovery exhaustion or high context pressure
- in-page ChatGPT permission-card handling as part of the current Project automation state; there is no separate permission-card toggle

**Automation off** keeps observation active but stops automatic mutations for that Project. Manual globally applies the same stop at the global layer without deleting saved Project preferences; switching back to Per-Project mode restores those preferences. Use Manual continue / Herdr monitor / LLM analysis when automatic execution is off; Manual handoff remains a separate explicit lifecycle control for bound Project conversations.

**Manual handoff** is intentionally independent of the automation switch. It appears only on ChatGPT Project HUDs and becomes usable once the conversation is bound to a Herdr workspace. Even with `Auto on`, a user can roll over early instead of waiting for context-pressure or recovery thresholds. The button reuses the same fail-closed handoff state machine: ask the current conversation for a marked transfer packet, open a fresh conversation in the same Project, seed it, and move workspace bindings only after that seed is confirmed. A bound workspace that is still `working` blocks the operation. Existing transfers surface as `Compressing…`, `Moving…`, or `Resume handoff` so a second transfer is not created accidentally.

### Page freshness / stale-view recovery

0.1.44 separates “ChatGPT never replied” from “the server has newer conversation state but this tab is showing stale DOM.” Both human-submitted and extension-submitted user turns enter the health state machine. Once a recent turn has been idle long enough, the content script best-effort fetches the current same-origin conversation snapshot, follows `current_node` to the latest assistant message, and compares its message id, text, completion status and update time with the last assistant message visible in the page.

- **server ahead**: a newer assistant message id exists, or the server copy of the same message is clearly longer than the DOM copy → refresh once when the composer/tool/permission safety gates allow it;
- **server stalled**: the server explicitly says the assistant message is unfinished and it has not advanced for at least 60 seconds; if the page still claims streaming, wait another 30 seconds before allowing refresh;
- **synced**: server and DOM agree and the server message is finished → do not refresh;
- **unknown**: the private same-origin snapshot endpoint is unavailable, times out, or changes shape → fail closed; never refresh just because the probe failed.

Before reload the extension persists the last assistant signature. After reload, newer content or resumed streaming ends recovery immediately. If the same partial answer is still present after 10 seconds and the page is otherwise safe, the extension submits one localized browser-recovery activation message telling ChatGPT to reread the current conversation state, continue from the actual stop point, and not replay completed mutations. Only if that activation also fails does the existing recovery-exhausted rollover path become eligible.

### A2. Long-conversation compression and rollover (ChatGPT Project)

Extension 0.1.39 adds **Rollover** to the in-page HUD. Version 0.1.43 adds the Project-scoped automation gate; 0.1.44 adds stale-view refresh and post-refresh activation; 0.1.46 exposes Manual handoff directly on the bottom HUD. Global manual mode or Project `Auto off` prevents automatic recovery/rollover actions from starting, but a user may still trigger Manual handoff explicitly on a bound Project conversation.

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

### What exists

- SpeaksJSON: extracts `{"tool":"...","args":{}}` from assistant replies

### Gaps

- calling local MCP, result backfill, allowlist, and Options

### Planned (three stages, agreed)

1. **protocol**: parse → `tools/call` → backfill (default read-only allowlist)
2. **capabilities**: turn on exec / file writes / prompt on demand
3. **full surface**: align with the ChatGPT default 18 tools (still local only)

See [extension-bridge.md](./extension-bridge.md).

## What we do not do

- pretend DeepSeek "has" an OAuth connector natively
- send the extension over the public Cloudflare MCP URL by default
- replace track A with track B (ChatGPT already has a connector; the stall problem is solved by A)

## Acceptance mantra

- **A**: after `herdr_prompt` from ChatGPT (or any bound site), there is a progress stamp while working, and a continue prompt after settled; the conversation keeps moving by itself. A bound ChatGPT Project can also use **Manual handoff** at any time to move safely to a fresh conversation in the same Project without waiting for an automatic threshold.
- **B** (not yet implemented): DeepSeek outputs a `herdr_inspect` JSON, the extension calls local and backfills an `ok` summary. For now we can only confirm the JSON appears in the assistant reply.