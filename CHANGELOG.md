# Changelog

Versions below follow `src/version.ts` / `package.json`. Git has no tags; 0.3.19–0.3.25 were never published as separate commits and landed in **0.3.26**.

ChatGPT caches `tools/list` by this version. After a bump: restart the public process, reconnect the connector, **open a new chat**.

## 0.3.26 — 2026-08-22

- Default MCP surface is **18 tools**: add read-only `herdr_skill` (fetch Herdr `SKILL.md` from upstream master; bundled fallback `assets/herdr-agent-SKILL.md`). Session start is `herdr_inspect` then `herdr_skill` once.
- CLI/extension UI: `en` / `zh` / `ja` (`herdr-mcp lang`, extension Options → Language).
- `herdr-mcp watchdog` (macOS LaunchAgent): restart MCP if the process is down; TaskGroup / missing socket is log-only.
- Socket RPC timeout cap (60s); `herdr api schema` warm + stale cache so `tools/list` does not wait on a live herdr.
- `session.snapshot` TaskGroup fallbacks: `herdr_git` can spawn local `git`; `herdr_inspect` can assemble `workspace.list` + `pane.list` + `agent.list`.
- `GET /push/state`, `GET /push/mcp-activity` (recent `tools/call` ring buffer).
- Browser extension **0.1.28**: bind a web chat to a herdr **workspace** (not a single pane); progress tick + optional ChatGPT post-turn LLM nudge; i18n; compact popup.
- Browser extension **0.1.30**: harden wake submit (stale composer, send-button scan, fixed footer visibility, textarea clear check); drop LLM judge `max_tokens` cap so reasoning models return `content`.

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
