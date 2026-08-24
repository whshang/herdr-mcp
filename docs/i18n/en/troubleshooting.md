# Troubleshooting

A symptom-first checklist for the common failure modes. When in doubt, start a new conversation after reconnecting — a large share of “0 tools” reports are stale snapshots, not a dead service.

## ChatGPT shows 0 tools, or an old tool count

- **Start a new conversation** after reconnecting; an old conversation keeps its old tool snapshot.
- Verify the same origin is used for the MCP URL **and** `OAUTH_ISSUER` (no `/mcp` suffix in the environment variable).
- Check Edge health, `herdr-link` connectivity and OAuth discovery (`/.well-known/oauth-authorization-server`).
- Current production contract is epoch 2 with **18 tools including `herdr_skill`**. If ChatGPT still shows the epoch‑1 17‑tool list, the conversation/Connector cache is stale. The runtime version comes from `/.well-known/mcp.json` / `initialize.serverInfo.version`.

Hard requirements and diagnostics: [chatgpt-connector](chatgpt-connector.md).

## The MCP Connector card keeps popping up

Verify the extension is loaded, **per-Project automation** is enabled in Options, and the current ChatGPT Project HUD shows **`Auto on`**. Permission-card handling is part of Project automation; in global manual mode or with the Project `Auto off`, the extension still observes the page and Herdr state but does not click permission cards. Native browser permission bars always require manual handling. See [extension](extension.md).

## The HUD is bound to w68 but the bottom bar shows another project name

Treat `workspace_id` as identity; the label is display cache only. The current extension prefers the live `/push/events` / `/push/state` workspace catalog over a stale binding label and repairs the persisted binding automatically. If the drawer already shows the correct `herdr-mcp (w68)` while the bar still shows an older project name, make sure the current 0.1.51 extension is loaded and refresh the page; do not unbind/rebind merely to repair a label for the same workspace id.

## ChatGPT replies halfway and then the page appears frozen or stale

0.1.44 no longer equates this directly with “the model is stuck.” The current Project must be `Auto on` for automatic stale-view recovery. After roughly 30 seconds without fresh page progress on a recent turn, the extension best-effort compares ChatGPT's same-origin conversation snapshot with the current DOM. A proven server-ahead view is refreshed once. If the server itself explicitly reports an unfinished assistant message, it must remain stalled for at least 60 seconds before reload is allowed; if the page still claims streaming, the detector waits another 30 seconds.

After reload, newer content or resumed streaming ends recovery. If the identical partial answer remains, the extension waits 10 seconds and submits one browser-recovery activation message. If the private snapshot endpoint is unavailable, errors, times out, or changes shape, freshness is `unknown` and the detector fails closed rather than refreshing on elapsed time alone. In that case, refresh manually and use HUD **Manual continue** or **Herdr monitor**. The recovery prompt tells ChatGPT to continue from the actual stop point and to re-check live Herdr/runtime/Git state before external mutations, reducing duplicate work.

## Manual handoff is unavailable, or stays on Compressing / Moving

Version 0.1.47 exposes **Manual handoff** for bound ChatGPT Project conversations and persisted z.ai `/c/<chat_id>` conversations. z.ai `/`, plain ChatGPT `/c/<id>`, Claude, and DeepSeek do not show it. Turn `Auto off` first: with automation on the HUD locks the button and background independently rejects `automation_enabled`. If a bound workspace still has an agent `working`, the background also rejects the handoff because binding cutover must not race settled/wake delivery.

If z.ai is already on `/c/<chat_id>` but the HUD still looks like the root launcher, verify that 0.1.51 is loaded and refresh once. A temporary binding/automation preference created on z.ai `/` migrates once when the same tab first becomes `/c/<chat_id>`; later navigation between existing `/c/A` and `/c/B` chats never drags it along. z.ai handoff summary/seed messages use the raw send path and are not rewritten by the JSON→MCP bridge.

If a z.ai / DeepSeek JSON→MCP task appears to stop and the **last real assistant message is still `{"tool": ...}` JSON**, the task is unfinished. Version 0.1.50 no longer treats round 12 as a completion boundary and can resume this pending tool JSON after page/script recovery. Internal protocol rows are also folded again after history reload. If an expanded folded z.ai row turns into a thin vertical strip, the current extension is stale; 0.1.50 moves the fold control outside the site's flex message root.

If the button reads **Compressing… / Moving…**, the source conversation already owns an active transfer and the button is locked to prevent a duplicate handoff. A delivery-uncertain seed becomes **Resume handoff**; clicking it probes the target conversation for the transfer marker before deciding whether to finish cutover or retry the seed. The old workspace binding remains authoritative until the new seed is confirmed, so do not manually unbind merely to “unlock” the button.

## z.ai `Auto off` looks clickable but does not switch on

Load extension **0.1.48 or newer**. Earlier 0.1.47 builds incorrectly required an explicit workspace binding before writing a z.ai / DeepSeek conversation automation preference, so an unbound HUD could show a clickable `Auto off` button and then fail with `conversation-unbound`. Since 0.1.48 the conversation preference can be toggled and persisted before binding; Herdr progress/settled push-back becomes effective once a workspace is actually bound.

## Local server not answering

- `curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/` should return `200` or `401`, not a connection error.
- On macOS with the LaunchAgent: `herdr-mcp status`, then `herdr-mcp logs [-f]`; `herdr-mcp watchdog install` restarts a down MCP every 120s.
- Confirm `HERDR_MCP_PORT` used by the server matches the port you probe.

## Tools fail intermittently, agent still shows working

Transient control-plane failure (ExceptionGroup / TaskGroup aggregation in the Herdr daemon): a request fails, seconds later the same one succeeds. Do not treat a control-plane blip as a repository blocker; re-check with `herdr_inspect` / `herdr_since`. Some requests degrade to composed list APIs with `warnings` like `snapshot_failed_used_list_apis`. See [architecture](architecture.md).

## Local workers unavailable

If Pi/Herdr workers are down, `dsh --profile headless "job"` is a tested CLI fallback — run it through a long `herdr_exec_start` session, because tool edits may complete before the final headless answer prints; check Git/tests before retrying. `dsh-tui` is the human-interactive fallback, not the default automation surface. See [worker-fallbacks](worker-fallbacks.md).

## I want to roll back a runtime release

Runtime A/B keeps the previous generation: `herdr-runtime-generation status`, then `rollback` (or `activate --generation <previous>`). Never use `herdr-self-update` to cross a contract epoch. See [runtime-self-upgrade](runtime-self-upgrade.md).

## Token security mistakes

- Never paste the static `HERDR_MCP_TOKEN` into ChatGPT — it authenticates via OAuth at the Edge.
- Never commit `~/.config/herdr-mcp/*.env` (Cloudflare cutover credentials, mode `0600`).
- Use a least-privilege Cloudflare token; expand permissions only for the one-shot legacy migration. See [cloudflare-edge-token](cloudflare-edge-token.md).

## Still stuck?

- Local: `herdr-mcp logs -f` or the server stdout; the boot line prints `boot_id` and the listening port.
- Edge: the worker `/health` endpoint and OAuth discovery.
- Then open the issue with the `boot_id`, the failing tool and its `failure_phase`, and whether a `commitAtomic` / `herdr_exec` delivery happened before the error.