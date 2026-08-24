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
- `progressTickSec` only sets the **check/nudge interval** (default 60s), not the send interval; **0 = disable progress announcements and turn nudge**
- **first actual send**: non-empty summary with a fingerprint change → `new_output`
- **already sent**: if less than `progressFallbackSec` (default 1200s / 20 minutes) since **the last send** → **do not send at all** (the floor is counted from the last send, not a fixed cron)
- after the floor: fingerprint changed → `new_output`; otherwise → `fallback`
- the sent baseline is written into the binding (`lastProgressSentAt` / `lastProgressOutput`), so dedupe survives a Service Worker kill

Defaults: `progressTickSec = 60` (progress check + nudge cooldown; `0` disables); `progressFallbackSec = 1200` (20-minute fallback; `0` = send only on new summaries). Both editable in the options page. UI languages: en / Simplified Chinese / Japanese (follows system first, manual override possible).

## Install and bind

1. Load `extension/`
2. Options: `http://127.0.0.1:8772` and the `herdr-mcp token` (the extension only connects local, using `/push/events` and `/push/state`, never Cloudflare)
   - This is independent of the public Worker contract epoch (currently epoch 2 / 18 tools); the extension does not read ChatGPT's `tools/list`.
3. Open the target conversation (chatgpt / deepseek / z.ai / claude)
4. popup: **bind** the **workspace** that will do the work (the list shows herdr **labels**, e.g. `novo (w5A)`; includes panes with terminal only and no agent; any agent progress in the space pushes back)
5. dispatch work

No binding = no push-back. Legacy "single pane" bindings upgrade to workspace by pane prefix after reconnect. Wake-copy features **workspace_label** as the protagonist; `{agent}` only names the focused pane.

## ChatGPT turn nudge (small-model decision)

Bound conversation + extension ≥ 0.1.20:

1. the content script marks turns by Stop appearing/disappearing
2. when Options has Base URL + Key + Model: one OpenAI-compatible `chat/completions` call over the user/assistant body
3. if the reply matches the "do not send" keywords → no nudge; otherwise if judged "continue" → **fill the small model's original text** into the input and submit (prompt / do-not-send words are pre-filled visible defaults in Options)
4. **no longer** uses zero-tool / halfway heuristics; without a configured small model, no nudge this turn
5. if the user bubble was the last nudge sentence, the **new assistant reply is still judged** (since 0.1.20); cooldown and progress checks share `progressTickSec`, **0 = nudge off** (default 60s)
6. persistent status bar at the bottom of the ChatGPT page (≥0.1.22): current config + latest judgment; copy follows the Options language (en / Simplified Chinese / Japanese)
7. herdr working/settled wake-ups remain independent

The key is stored locally only; the repo keeps it empty by default.

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

Content script ≥ 0.1.3 persistently auto-clicks "Allow" on chatgpt.com. See [chatgpt-connector.md](./chatgpt-connector.md).

## Testing

- `node tests/manual/extension_smoke.mjs`
- `node tests/manual/background_bind_test.mjs`
- `node tests/manual/push_sse.mjs`