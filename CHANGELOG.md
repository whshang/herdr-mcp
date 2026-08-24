# Changelog

- Background `herdr_exec_start` sessions and the `herdr_exec` local-shell fallback now choose an available login shell (`zsh` → `bash` → `sh`) instead of hard-coding `/bin/zsh`, so the same exec/session semantics work on Linux CI and Linux Herdr workstations.

Versions below follow `src/version.ts` / `package.json`. Git has no tags; 0.3.19–0.3.25 were never published as separate commits and landed in **0.3.26**.

ChatGPT can retain a `tools/list` snapshot for a conversation. A runtime version bump does **not** have to change that public catalog: production may run a newer implementation under a frozen contract profile. Only a deliberate contract/tool-surface change requires Connector/tool-snapshot migration and a new chat.

## 0.3.32 — 2026-08-24

- Browser extension **0.1.46** adds an explicit **Manual handoff** action to the persistent ChatGPT Project HUD. It reuses the existing fail-closed rollover state machine: summarize the current conversation into a marked handoff packet, open a fresh conversation in the same Project, seed it, and move Herdr workspace bindings only after the seed is confirmed. Manual handoff remains available even when Project automation is `Auto on`, so users can roll over early; it still requires a bound Project and refuses to move while the bound workspace is working. The button reflects active transfer state (`Compressing…` / `Moving…` / `Resume handoff`) instead of allowing duplicate handoffs.
- Browser extension **0.1.45** makes the active automation state visually unmistakable: when the current ChatGPT Project is effectively `Auto on`, the entire persistent bottom HUD uses a restrained light-green surface, green top border and soft green shadow. The stronger background is only an automation-state cue; runtime warning/error colors remain authoritative, and global manual / Project `Auto off` keeps the neutral HUD treatment. Dark mode receives a matching low-glare green treatment.
- Browser extension **0.1.44** simplifies Options to one **Enable per-Project automation** checkbox. Unchecked means global manual mode; checked only exposes the Project HUD `Auto on/off` control, and every new Project still defaults off until explicitly enabled there. The old separate permission-card auto-allow preference is removed: in-page Allow handling now follows the effective Project automation state, while browser-native permission bars remain manual.
- Browser extension **0.1.44** also adds stale-view recovery for ChatGPT replies that start and then freeze or whose DOM lags the server. After a recent user turn has been idle for 30s, the extension best-effort compares the page's latest assistant message with ChatGPT's same-origin conversation snapshot (`current_node`, message id, completion status, update time). A proven server-ahead view is refreshed once; a server-side unfinished response must remain stalled for at least 60s (90s if the page still claims streaming) before refresh is allowed. After reload, newer content ends recovery; if the same partial reply remains, the extension sends one localized activation message telling ChatGPT to reread the current conversation and continue without replaying completed work. If the internal snapshot endpoint is unavailable or changes shape, this detector fails closed and does not refresh on guesswork.
- Browser extension **0.1.43** replaces the global HUD automation master switch with a two-level policy: Options chooses **Manual globally / Per-Project automation**, while each ChatGPT Project must be explicitly enabled from its HUD and new Projects default off. The Project preference is keyed by stable `project_id`, so sibling conversations and rollover successors share it; Manual globally hides the HUD automation switch without deleting saved Project preferences.
- Promote the public ChatGPT ABI to **contract epoch 2 / 18 tools**, including the read-only `herdr_skill`, frozen at `sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8`. Epoch 1 remains tracked exactly as the historical 17-tool rollback/old-session compatibility baseline.
- Browser extension **0.1.42** turns the old wake toggle into a real automation master switch. The persistent bottom HUD now keeps the high-frequency controls on the bar — **Manual continue / Herdr monitor / LLM analysis / Auto on|off** — while the drawer is limited to event timing, conversation binding and advanced settings. `Auto off` keeps Herdr/workspace observation live but blocks new automatic progress/wake sends, post-turn LLM decisions, recovery probes, safe reloads, automatic Project rollover and permission-card clicks.
- Browser extension **0.1.42** adds conservative ChatGPT conversation-pressure tracking and timeout recovery. Long bound Project conversations may reuse the existing fail-closed handoff state machine automatically only after the estimated visible text reaches the auto-rollover threshold and quiescence/safety gates pass; reply stalls can receive one read-only recovery probe, one safe reload, then rollover when recovery is exhausted. These are browser-side estimates and safety gates, not ChatGPT backend token counts.
- Browser extension **0.1.42** completes HUD i18n across en / zh / ja, gives `Auto on` and `Auto off` separate detailed tooltips, and treats `workspace_id` as binding identity while live workspace labels are authoritative display metadata. Stale persisted labels are repaired automatically so the drawer and bottom bar cannot show different project names for the same workspace id.
- Make the production contract explicit across the Edge, workstation link, runtime profile, self-update gates and Cloudflare domain probes. Same-epoch runtime A/B remains automatic; cross-epoch migration is a supervised server → Edge → link operation and `herdr-self-update` fails closed if asked to cross it.
- Remove obsolete Edge scaffolding: rename the production MCP handler away from `mcp-dev`, remove the unused `mcp-placeholder` module, rename shared/static auth and `DEFAULT_WORKSTATION_ID` by their actual roles, and remove the orphaned `herdr-edge-cutover` script/test in favor of `herdr-cloudflare-domain` + `herdr-custom-domain-cutover`.
- Rewrite current architecture, Connector, deployment, capability and runtime-upgrade documentation so epoch 2 is the only current production path; historical epoch-1 claims remain only where they are intentionally documenting or testing compatibility history.

## 0.3.31 — 2026-08-23

- Make `workspace.list` the sole admission authority for unknown workspace IDs. Contradictory daemon event sequences such as `workspace_closed` followed by stale `workspace_created` can no longer resurrect a closed workspace.
- The first event for an unknown workspace forces an immediate full reconciliation; a genuinely new workspace is admitted as soon as `workspace.list` confirms it, while repeated orphan events are ignored without refresh loops.
- Browser extension **0.1.39** adds fail-closed ChatGPT Project conversation rollover: the current web model produces a marked compact handoff, a fresh chat in the same Project is seeded, and Herdr workspace bindings move only after the target conversation and seed are confirmed. Project URL aliases with cosmetic slugs normalize to the stable Project resource id, and explicit workspace bindings no longer silently expire after 24 hours.

## 0.3.30 — 2026-08-23

- Keep an authoritative live-workspace ID set from `workspace.list` and reject orphan pane/workspace/tab updates for IDs that are no longer live. Herdr may continue emitting pane updates after a workspace is closed; those events can no longer resurrect phantom rows in `herdr_inspect`, `/push/state`, or the browser extension picker.
- Explicit `workspace.created` events admit new workspace IDs immediately; periodic reconciled snapshots remain the fallback for missed topology events.

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
