# Progress nudge

Audience: anyone whose web AI dispatches work to herdr and then the conversation stalls, needing proactive reminders to continue.

Overview and both tracks: [extension.md](./extension.md). JSON→MCP: [extension-bridge.md](./extension-bridge.md).

## The problem it solves

1. The web hands a task to herdr over MCP or the JSON bridge.
2. The tool quickly returns "submitted", **this turn of the conversation ends**.
3. The herdr agent is still `working`, or settles only later.
4. The web model no longer observes automatically → the task looks interrupted.

Extension track A: **periodic progress announcements + done reminder**, written into the bound conversation and submitted.

## Current implementation

| Event | Behavior |
|---|---|
| `agent_working` (any pane in the bound workspace) | arms; if check interval >0, start the timer |
| progress tick | every `progressTickSec` check; `routeWake` only on new non-empty summary or when `progressFallbackSec` is reached |
| `agent_settled` while the same space still has working agents | **partial progress** template wake (not the full-turn done) |
| `agent_settled` and nothing in scope is working | **done** template wake once, stop the tick |
| reconnect `hello` | can backfill a missed in-scope done; if the snapshot still shows working → resume ticking |

**Working-periodic progress announcements (track A)**: while the bound conversation's agent is `working`, check every `progressTickSec` seconds whether to submit a progress reminder to the web; on `settled`, still wake once per the current logic and stop the tick.

**Same field**: `progressTickSec` is also the cooldown seconds of the ChatGPT turn nudge (one place to fill in popup/options).

Actual-send rules (avoid empty-spin spam):
- `progressTickSec` only sets the **interval-driven progress check / automatic LLM turn-decision interval** (default 60s), not the send interval; **0 disables only those two interval-driven actions and does not change the Options global mode or the Project HUD automation switch**
- **first actual send**: non-empty summary with a fingerprint change → `new_output`
- **already sent**: if less than `progressFallbackSec` (default 1200s / 20 minutes) since **the last send** → **do not send at all** (the floor is counted from the last send, not a fixed cron)
- after the floor: fingerprint changed → `new_output`; otherwise → `fallback`
- the sent baseline is written into the binding (`lastProgressSentAt` / `lastProgressOutput`), so dedupe survives a Service Worker kill

Defaults: `progressTickSec = 60` (progress check + automatic LLM-decision cooldown; `0` disables those two); `progressFallbackSec = 1200` (20-minute fallback; `0` = send only on new summaries). The Options master gate controls **ChatGPT Project-shared automation only**. With it off, Projects do not run shared automatic mutations; plain ChatGPT `/c/<id>`, z.ai, and DeepSeek can still enable their own conversation-scoped Auto from the HUD. New Projects and new conversations default off. UI languages: en / Simplified Chinese / Japanese (follows system first, manual override possible).

## Install and bind

1. Load `extension/`
2. Run `bin/herdr-extension-host install`, then keep Options on `http://127.0.0.1:8772`. Chrome Native Messaging carries extension requests to the local host, which reaches herdr-mcp through `~/.config/herdr-mcp/extension.sock` (mode `0600`). Current Options has no Herdr Token field and no extension credential expires. The extension consumes `/push/events` and `/push/state` through this local IPC path and never goes through Cloudflare.
   - This is independent of the public Worker contract epoch (currently epoch 2 / 18 tools); the extension does not read ChatGPT's `tools/list`.
3. Open the target conversation (chatgpt / deepseek / z.ai / claude)
4. popup: **bind** the **workspace** that will do the work (the list shows herdr **labels**, e.g. `novo (w5A)`; includes panes with terminal only and no agent; any agent progress in the space pushes back)
5. dispatch work

No binding = no push-back. Legacy "single pane" bindings upgrade to workspace by pane prefix after reconnect. Wake-copy features **workspace_label** as the protagonist; `{agent}` only names the focused pane.

## ChatGPT turn nudge (small-model decision)

Bound conversation + extension ≥ 0.1.20:

1. the content script marks turns by Stop appearing/disappearing
2. when Options is in Per-Project mode, the current Project is `Auto on`, and Base URL + Key + Model are configured: one OpenAI-compatible `chat/completions` call over the user/assistant body; otherwise use the bottom-bar **LLM analysis** action to trigger it manually
3. if the reply matches the "do not send" keywords → normally no nudge; otherwise if judged "continue" → **fill the small model's original text** into the input and submit (prompt / do-not-send words are pre-filled visible defaults in Options). 0.1.54 adds one deliberately narrow guard: if the assistant's own tail explicitly commits to unfinished work such as `Next: I will...`, `I will continue...`, `still need to...`, or the Chinese equivalents, a contradictory `done` verdict is overridden; optional advice like “next you could consider…” does not match this guard.
4. it no longer uses zero-tool or generic halfway guessing; without a configured small model, no nudge this turn. 0.1.54 also fixes dedup timing: an assistant fingerprint becomes terminal only after explicit `done` or a **confirmed successful** continue submission. Ambiguous results are eligible for a 30-second re-judge, failed sends stay retryable, and only one judge/send attempt per conversation may be in flight at once.
5. if the user bubble was the last nudge sentence, the **new assistant reply is still judged** (since 0.1.20); automatic judgment and progress checks share `progressTickSec`; `0` disables automatic LLM judgment (default 60s) but not manual **LLM analysis**
6. persistent bottom HUD on supported sites: runtime state, **Manual continue / Herdr monitor / LLM analysis / Manual handoff**, optional **Auto on|off**, and expand. ChatGPT Projects expose a `project_id`-scoped switch only when the Options Project gate is enabled; plain ChatGPT `/c/<id>`, z.ai, and DeepSeek always remain eligible for their own conversation-scoped switch. Manual handoff is available for bound ChatGPT Project conversations and persisted z.ai `/c/<chat_id>` conversations; with the current scope `Auto on`, all four HUD manual actions are locked. Frequent actions stay on the bar, and the drawer contains only event settings, conversation bindings and advanced options. Copy follows the Options language (en / Simplified Chinese / Japanese)
7. herdr working/settled wake-ups remain independent

The key is stored locally only; the repo keeps it empty by default.

## Stale ChatGPT page / partial-reply recovery (0.1.44)

With Project `Auto on`, both human-submitted and extension-submitted user messages enter the conversation-health state machine. After roughly 30 seconds without fresh page progress, the extension best-effort fetches the current same-origin ChatGPT conversation snapshot, follows `current_node` to the latest assistant message, and compares it with the last assistant message in the DOM:

- newer server message id, or clearly longer server text for the same message: **server ahead** → refresh the page once when safety gates pass;
- server explicitly reports an unfinished assistant message with no progress for at least 60 seconds: **server stalled**; if the page still claims streaming, wait another 30 seconds;
- server and page agree and the message is finished: **synced** → do not refresh;
- snapshot request fails, times out, or changes shape: **unknown** → fail closed; never refresh on guesswork.

The pre-refresh assistant signature is persisted. If reload reveals newer content or streaming resumes, recovery ends. If the identical partial reply remains after 10 seconds and the composer, tools and permission cards are idle, the extension submits exactly one browser-recovery activation message telling ChatGPT to reread the current conversation, continue from the real stop point, and not repeat completed work. Only if that also fails does recovery-exhausted rollover become eligible.

### Explicit ChatGPT send-timeout card (0.1.53)

When ChatGPT itself renders the thread error card with `data-testid="regenerate-thread-error-button"` (for example, “Message sending timed out, please retry”), Auto does **not** immediately hard-refresh or blindly submit another message. The extension first reads the same-origin conversation snapshot and inspects `current_node`:

- `current_node` is already an assistant message → the request reached the server far enough that retrying may repeat tool work; perform at most one safety-gated reload to reconcile the page;
- `current_node` is still the user message → use ChatGPT's own Retry button once; if no reply starts within the normal reply timeout, reload once when the existing safety gates allow it;
- snapshot delivery state is unknown → prefer one safety-gated reload instead of guessing that Retry is safe;
- after the one Retry / one reload budget is exhausted, keep the explicit error authoritative and suppress generic recovery-message submission rather than creating a second user turn blindly; if the reload produced no new assistant progress after 10 seconds, the HUD moves to `rollover_recommended` instead of silently remaining stuck in `recovering`.

So a hard refresh is a **second-stage recovery**, not the first response to every send-timeout card.

### ChatGPT virtualized long-history rollover (0.1.54)

ChatGPT does not keep an entire long conversation mounted in the DOM. In real UAT, a Project conversation that had reached `conversation-turn-50` exposed only five user/assistant nodes after reload; a DOM-only counter therefore reset a genuinely long conversation to an apparently short one. 0.1.54 fixes that boundary:

- read mounted `[data-testid="conversation-turn-N"]` rows and treat the highest absolute `N + 1` value as a **message-count floor**;
- persist that floor in `chrome.storage.local` and only allow it to increase, so reload, lazy loading and virtual-list eviction cannot reduce recorded pressure;
- visible text is still token-estimated, but the usable text budget is reduced from 120k to 96k to reserve context for hidden Project/system instructions and MCP/tool payloads;
- approximately `56k / 64k / 72k / 80k / 92k` visible-text tokens enter warning / prepare / recommend / required / high-risk, while approximately `40 / 46 / 50` message segments independently enter prepare / recommend / required;
- `required/high-risk` does **not** mean an unconditional cutover. Auto rollover still requires the current Project to be `Auto on`, a bound handoff-capable workspace, a quiet page, no streaming/tool/permission-card/human draft, no uncertain delivery, and no active handoff.

This lets tool-heavy Herdr conversations roll over before visible prose alone approaches the nominal model window.

## Manual handoff (0.1.47)

The bottom-HUD **Manual handoff** action lets the user roll over early, but the current conversation must first be switched to `Auto off`:

- it supports bound ChatGPT Project conversations and persisted z.ai `/c/<chat_id>` conversations; z.ai `/` is only the new-chat launcher and does not show Manual handoff;
- clicking it reuses `h2w_handoff_start(trigger=manual)` and first asks the current web model for a compact transfer-id-marked handoff packet;
- z.ai summary and seed control messages use the raw send path and explicitly bypass the JSON→MCP bridge, so they cannot be rewritten into coding-agent tasks;
- if a bound workspace is still `working`, the operation is rejected to avoid racing settled/wake delivery against binding cutover; with `Auto on`, the button is locked and background independently returns `automation_enabled` if the UI is bypassed;
- ChatGPT opens a fresh conversation in the same Project; z.ai launches from `/`. Workspace bindings move only after a new conversation id exists and the seed marker is confirmed;
- an in-flight transfer changes the button to **Compressing… / Moving… / Resume handoff** instead of creating a duplicate transfer; `seed_uncertain` can be resumed through the same fail-closed recovery path;
- when a z.ai root chat first becomes `/c/<chat_id>`, its temporary binding and automation preference migrate once; later navigation from `/c/A` to `/c/B` never drags the binding along.

## Multi-task semantics

| Event | Behavior |
|---|---|
| any agent in the space → working | arms; starts the progress tick |
| one pane settled while the same space still has working agents | **partial progress** wake |
| nothing in the space is working anymore | **done** wake |
| every wake (partial / progress / done) | focused-pane output + **whole-workspace pane overview** (agent / terminal title / status / cwd); with an idle pane, offers three choices: keep waiting for next round / summarize and wrap up / recycle and open a new one |
| new output while working | progress still sent per fingerprint + fallback interval |

Default templates contain `{roster}` `{idle_hint}` placeholders; the extension forces them on even when the custom template omits them.

## ChatGPT permission cards

The content script keeps observing in-page permission cards on chatgpt.com, but clicks an explicit Allow action only when **ChatGPT Project automation is enabled in Options + the current Project is `Auto on`**. Permission handling is part of Project automation and no longer has a separate toggle. Disabling the Project gate or setting that Project `Auto off` stops automatic permission clicks. Native browser permission bars are outside this mechanism. See [chatgpt-connector.md](./chatgpt-connector.md).

## Testing

- `node tests/manual/extension_smoke.mjs`
- `node tests/manual/background_bind_test.mjs`
- `node tests/manual/push_sse.mjs`