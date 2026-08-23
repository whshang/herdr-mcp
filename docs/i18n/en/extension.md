# Browser extension — product overview (two tracks)

Audience: extension authors and users. Chrome display name **herdr → Web wake**. `extension/` does two things at once, sharing the token and Options, with **uneven completeness**: A is usable, B only reaches JSON extraction.  
Language: product UI is en / Simplified Chinese / Japanese (same as herdr); this page is written in English. Repo-root README: [en](../../../README.md) / [zh](../../../README.zh.md) / [ja](../../../README.ja.md).

| Track | Problem | Direction | Status | First sites |
|---|---|---|---|---|
| **A. Progress push-back** | after the web dispatches work to herdr, the conversation stops observing/continuing | herdr → web (writes the input box and submits) | **usable** (extension 0.1.38) | chatgpt / deepseek / z.ai / claude |
| **B. JSON→MCP** | DeepSeek / z.ai web has no MCP Connector | web → local `127.0.0.1:8772/mcp` | **incomplete** (can extract JSON, MCP not called) | `chat.deepseek.com`, `chat.z.ai` |

Shared: same extension, same static token, same options.  
Deployment boundary: the extension only ever hits local `127.0.0.1:8772` `/push/*` (only future B touches local `/mcp`); it never goes through the public Worker/Tunnel. So Cloudflare Edge, Custom Domain, and contract-epoch changes never require the extension to change URL/OAuth. The 0.3.26 production runtime passed `tests/manual/extension_smoke.mjs` in epoch1-compatible mode; track A stays compatible with the newer server. ChatGPT's plain `/c/<id>` and in-project `/g/g-p-…/c/<id>` both use the full `origin + pathname` as the session binding key; SPA route changes re-register automatically without requiring a full page refresh.

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

## A. Proactive progress reminders (site-wide)

### What exists

- Binding: the popup binds the "current conversation" to a herdr **workspace** (multiple agents in one space can push back in parallel)
- SSE: `/push/events?workspace=...`
- Policy: after seeing `working`; partial settle reports progress, all-idle in scope reports done; `hello` can backfill
- chatgpt.com permission cards: persistent auto-click "Allow"
- Optional small-model nudge after a ChatGPT turn ends (Options configures Base URL + Key + Model; `idleNudgeEnabled`; cooldown and progress checks share `progressTickSec`)
- UI: en / Simplified Chinese / Japanese

### Gaps

1. ~~periodic progress announcements while working~~ (implemented, see below)
2. binding friction: no binding → zero push-back
3. agent-less `herdr_exec` utilities do not enter the agent state machine (optional follow-up; workspace binding does not cover utilities either)
4. ~~ChatGPT turn nudge~~ (extension ≥0.1.18: small-model decision only; Options pre-fills prompt / do-not-send words; submit the model's original text when continuing)

### Implemented (track A periodic progress announcements)

- Options: `progressTickSec` (default `60` seconds: working progress checks + turn-nudge cooldown; `0` disables), `progressFallbackSec` (default `1200` seconds fallback with no new summary), separate progress and done templates (`progressTemplate` / `wakeTemplate`); UI languages en / Simplified Chinese / Japanese
- actual send rule: at a checkpoint, only push to the web when the herdr summary has **new non-empty content** since the last actual send; otherwise wait until `progressFallbackSec` and send one fallback, avoiding empty-spin spam
- `working`: check the summary every `progressTickSec`; push progress into the bound conversation and submit only on new non-empty content or when `progressFallbackSec` is reached; one timer per convKey; repeated `working` does not lose the last-sent baseline
- popup lists by **workspace** (incl. panes with terminal only, no running agent); one title line + one pane-stats line, no repeated project names, no `agent@pane` listing
- `settled`: cancel the tick timer first, then wake once with the done template
- same set site-wide; site differences only live in the injector write/send

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

- **A**: after `herdr_prompt` from ChatGPT (or any bound site), there is a progress stamp while working, and a continue prompt after settled; the conversation keeps moving by itself.
- **B** (not yet implemented): DeepSeek outputs a `herdr_inspect` JSON, the extension calls local and backfills an `ok` summary. For now we can only confirm the JSON appears in the assistant reply.