# Changelog

- Background `herdr_exec_start` sessions and the `herdr_exec` local-shell fallback now choose an available login shell (`zsh` → `bash` → `sh`) instead of hard-coding `/bin/zsh`, so the same exec/session semantics work on Linux CI and Linux Herdr workstations.

Versions below follow `src/version.ts` / `package.json`. Git has no tags; 0.3.19–0.3.25 were never published as separate commits and landed in **0.3.26**.

ChatGPT can retain a `tools/list` snapshot for a conversation. A runtime version bump does **not** have to change that public catalog: production may run a newer implementation under a frozen contract profile. Only a deliberate contract/tool-surface change requires Connector/tool-snapshot migration and a new chat.

## 0.3.29 — 2026-08-23

- Reconcile `session.snapshot` workspace/pane/agent collections with the dedicated `workspace.list` / `pane.list` / `agent.list` APIs. Recently closed workspaces can no longer be reintroduced into `/push/state` by a stale aggregate snapshot.
- Handle the daemon's direct-id `workspace_closed` event shape and remove the entire closed workspace scope (workspace, panes, agents, tabs) immediately, preventing stale panes from recreating phantom workspace rows in the browser extension.
- Browser extension **0.1.38** recovers content scripts after an unpacked-extension reload and recognizes the active ChatGPT conversation from its URL while reinjection completes, including project conversations under `/g/g-p-…/c/…`.

## 0.3.28 — 2026-08-23

- Fix the real self-update post-reload probe discovered during the 0.3.27 production upgrade: `server/discover` version validation now accepts the current `_meta["io.modelcontextprotocol/serverInfo"]` shape as well as the legacy direct `serverInfo` shape.
- Treat a launchd reload attempt as a runtime-switch boundary before verification. Rollback now restores, reloads, and verifies the original 8772 runtime before re-activating the original generation pointer, and bootstrap results are checked through `reloadServer().ok`.
- Epoch-1 `initialize.instructions` no longer tells a web planner to call the intentionally hidden `herdr_skill`; current-contract sessions still use the project skill, while frozen 17-tool sessions use `herdr_inspect` + live method reflection.
- Browser extension **0.1.35** reconciles partial-vs-round wake messages against a fresh workspace snapshot immediately before rendering, so stale settle events cannot contradict the latest worker count.

## 0.3.27 — 2026-08-23

- `herdr_skill` becomes a **herdr-mcp remote-planner operating skill**, not merely a copy of the native Herdr tutorial: project policy covers deterministic tool order, direct code edits, cheap-worker preferences, DSH fallback rules, mutation/idempotency discipline, browser boundary, CI/CD, and supervised self-upgrade. Calls also include live runtime/generation/update context and an explicitly scoped, release-matched `herdr --skill` native reference.
- Add `herdr-self-update`: a detached, structured-status runtime updater that builds/tests an isolated release, validates the exact frozen contract through the persistent generation manager, A/B switches traffic, reloads the stable 8772 service, promotes the new stable generation, and rolls back on failure without changing Edge/DNS/OAuth/contract epoch.
- Add GitHub Actions for root/Edge/extension CI, native GitHub Pages, and gated Cloudflare production Worker deployment. The documentation site at `https://whshang.github.io/herdr-mcp/` is generated from tracked `docs/*.md` by `npm run build:site` and also publishes the refreshable project skill plus `release.json`.
- Repository is public with the documentation site as its GitHub homepage. `workers.dev` remains the default public deployment for users; Custom Domain remains optional.
- Validate DeepSeek Harness as an optional worker fallback: `dsh --profile headless` can perform real code edits but must run as a long exec session because mutations may complete before the final answer is printed. `dsh-tui` is reserved for human-interactive takeover. Pi/Herdr-native workers remain preferred.
- Production ChatGPT remains on frozen contract epoch 1 (exact 17-tool hash); the runtime implementation can advance to 0.3.27 without forcing an existing Connector tool-snapshot change.

## 0.3.26 — 2026-08-22

- Standalone/local default MCP surface is **18 tools**: add read-only `herdr_skill` (fetch Herdr `SKILL.md` from upstream master; bundled fallback `assets/herdr-agent-SKILL.md`). When the tool is present, session start is `herdr_inspect` then `herdr_skill` once. At the 0.3.26 release, production ChatGPT ran under frozen contract epoch 1, exposing the prior exact 17-tool ABI and intentionally hiding `herdr_skill` until an explicit epoch upgrade.
- CLI/extension UI: `en` / `zh` / `ja` (`herdr-mcp lang`, extension Options → Language).
- `herdr-mcp watchdog` (macOS LaunchAgent): restart MCP if the process is down; TaskGroup / missing socket is log-only.
- Socket RPC timeout cap (60s); `herdr api schema` warm + stale cache so `tools/list` does not wait on a live herdr.
- `session.snapshot` TaskGroup fallbacks: `herdr_git` can spawn local `git`; `herdr_inspect` can assemble `workspace.list` + `pane.list` + `agent.list`.
- `GET /push/state`, `GET /push/mcp-activity` (recent `tools/call` ring buffer).
- Browser extension **0.1.28**: bind a web chat to a herdr **workspace** (not a single pane); progress tick + optional ChatGPT post-turn LLM nudge; i18n; compact popup.
- Browser extension **0.1.30**: harden wake submit (stale composer, send-button scan, fixed footer visibility, textarea clear check); drop LLM judge `max_tokens` cap so reasoning models return `content`.
- Browser extension **0.1.31**: the in-page HUD now fetches the live `/push/state` workspace catalog, shows a workspace picker, and can bind/unbind directly from the page. The popup and in-page HUD now use the same workspace source instead of leaving workspace discovery popup-only.
- Browser extension **0.1.32**: keep conversation registration synchronized across SPA navigation, including ChatGPT project conversation URLs (`/g/g-p-…/c/…`), so the in-page workspace picker follows the currently visible conversation without a full page reload.
- Browser extension **0.1.33**: replace one-SSE-per-workspace with one shared `/push/events` stream and fan out by workspace in the service worker. This prevents historical bindings from exhausting Chromium's per-origin HTTP connection pool and starving `/push/state`, which previously made the in-page workspace picker appear permanently empty.
- Browser extension **0.1.34**: bound localhost state requests and SSE connection handshakes, expose Chrome 145+ loopback-network permission failures instead of hanging, and make Options use the same background transport as popup/HUD. The UI now points users to the extension-specific “Apps on device” permission when Chrome gates `127.0.0.1`.
- Browser extension **0.1.35**: reconcile partial-vs-round wake messages against a fresh `/push/state` snapshot immediately before rendering. A stale settle event can no longer produce the contradictory “0 still working / partial finish” message; badge state and text now follow the same final workspace state.
- Browser extension **0.1.36**: keep one shared `/push/events` stream whenever wake is enabled and cache its authoritative `hello.workspaces` catalog for the in-page picker. The HUD renders that catalog immediately, while concurrent `/push/state` refreshes are coalesced into one bounded request.
- Browser extension **0.1.37**: keep the shared discovery stream alive even with zero bindings, clear its workspace catalog when the configured endpoint is rebuilt, and report Chrome `loopback-network` permission denial/prompt even when `fetch()` fails immediately with `TypeError: Failed to fetch` instead of timing out.

## 0.3.18 — 2026-08-21

- `herdr_exec`: if the control plane TaskGroup blocks pane `send_text` **before** delivery, fall back to a local zsh (`backend:local_fallback`). Never double-run after delivery.

## 0.3.17 — 2026-08-21

- Lean default surface documented as **17 tools** (passthrough + inspect/since/prompt + fs/git/exec). `HERDR_MCP_ALL_TOOLS=1` still adds lifecycle tools (30).
- Agent soft-hide: `inspect`/`since` list cheap workers + auditors; Claude/OMP/Codex hidden unless `HERDR_MCP_AGENT_ALLOW`.
- Atomic file writes, long `herdr_exec_*` sessions, `herdr_fs_patch`, `confirm_busy` write gate, `inspect.workstation_info` / `boot_id` / `exec_sessions`.

## 0.3.10 — 2026-08-21

- Extension: progress wake gating (new summary or fallback heartbeat).
- `herdr_prompt` error semantics (`agent_status_wait_timeout` vs transport).
- `herdr_exec` busy gate when a worker is already on the same project.

## 0.3.0 – 0.3.9 — 2026-08-21

ChatGPT Connector path: CIMD OAuth, JWT `aud` = resource URL, stateless Streamable HTTP (no `Mcp-Session-Id` for `openai-mcp`), `initialize`/`tools/list` as SSE, protocol-version header rewrite, schema hygiene so one bad tool does not drop the whole table.

## 0.3.0 identity / baseline — 2026-08-21

TypeScript MCP server + MV3 extension (chatgpt / claude / deepseek / z.ai) + `/push/events` SSE. Server version is the ChatGPT tool-registry cache key.
