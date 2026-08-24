# Capability benchmark

Audience: maintainers and contributors making design/ADR decisions. This is not an installation or everyday usage guide.

> Purpose: record herdr-mcp's capability trade-offs against official Herdr, other Herdr MCP implementations and coding-tools-mcp, so research is not repeated, features are not blindly copied, and the tool surface does not bloat.

## Design principles

herdr-mcp's goal is not to re-wrap Herdr, nor to become a generic coding sandbox. It serves "the web model as planner, remotely driving local Herdr + workstation", so it prioritizes:

1. a fixed, compact, cacheable MCP tool surface;
2. native Herdr socket capabilities staying reachable through `herdr_methods` + `herdr_call`;
3. first-class MCP tools only for "workstation capabilities the remote web itself cannot get": files, Git, shell, images;
4. explicit delivery / idempotency / rollback semantics for mutations;
5. decoupling the ChatGPT public connection from the local runtime lifecycle;
6. no duplicate agent-orchestration layer just to match some benchmark.

## Current adoption matrix

| Source/idea | Capability | Current status | herdr-mcp implementation/decision |
|---|---|---|---|
| coding-tools-mcp | fixed tool catalog; tools/list does not change dynamically with runtime/permission mode (upstream currently fixes 20 tools) | **principle adopted, catalog not copied** | herdr-mcp production contract epoch 2 fixes 18 tools, including `herdr_skill`. Herdr's ~90 native methods are not registered one by one. |
| coding-tools-mcp | workspace primitives: read/list/search/patch | **adopted** | `herdr_fs_read/list/grep/patch`; `write/edit` kept as precise write tools for the Herdr scenario. |
| coding-tools-mcp | atomicity / failure cleanup of multi-file patches | **equivalent guarantee adopted** | `herdr_fs_patch` + `commitAtomic`; failed new-file commits are cleaned up; writes are gated by managed-root/dirty/busy/readonly gates. |
| coding-tools-mcp | long commands use a separate handle, readable/killable | **adopted** | `herdr_exec_start/read/kill`; separate from the short command `herdr_exec`. |
| coding-tools-mcp | Git fact tools | **partially adopted** | `herdr_git status/diff/log`; no separate `show/blame` for now; use `herdr_exec git ...` when needed, to avoid widening the surface. |
| coding-tools-mcp | image reading | **adopted** | `herdr_fs_image` returns an MCP image directly. |
| coding-tools-mcp | HTTP session layered with long-command session; command handle can continue across tool calls | **core idea adopted, ChatGPT adaptation differs** | each coding-tools-mcp `Mcp-Session-Id` owns an independent Runtime; for ChatGPT's real compatibility problem with reusing stale sids after restart, the OpenAI/ChatGPT path instead stays stateless, and long-command state is managed separately via `herdr_exec_start/read/kill`. |
| coding-tools-mcp | structuredContent as stable machine results | **adopted** | tool results keep structured results; Relay passes the complete `CallToolResult` through, so non-text content like images is not lost. |
| coding-tools-mcp | OAuth / PKCE / DCR / protected resource metadata | **adopted and extended** | the Cloudflare Edge terminates OAuth; DCR/CIMD, PKCE S256, refresh rotation, private_key_jwt, issuer continuity. |
| coding-tools-mcp | safe/trusted/dangerous command permission policy | **not copied** | currently `READONLY` / `WRITE_ROOTS` / busy/dirty confirmations; shell is an explicitly high-capability boundary, not disguised as a full sandbox. |
| coding-tools-mcp | root project instructions auto-injected at initialize | **not adopted** | Herdr/Agent directives belong to the official skill and the concrete agent; scanning project directives ourselves would duplicate AGENTS/agent runtime. |
| official Herdr | live socket API is the source of truth | **adopted** | `herdr_methods` reflects the live schema; `herdr_call` does schema-validated passthrough; the 90+ methods are not duplicated. |
| official Herdr | Agent Skill | **adopted with remote-planner adaptation** | `herdr_skill` is a read-only epoch-2 tool. It returns the project policy plus release-matched upstream Herdr guidance, explicitly distinguishing the off-site web planner from agents running inside Herdr-managed panes; network failure falls back to the bundled skill. |
| official Herdr | Plugin v1 / plugin registry / event hooks / link handlers | **natively reachable, no dedicated MCP tools added** | official plugin capabilities remain provided by the installed Herdr; `herdr_methods` + `herdr_call(plugin.*)` can discover/call the live socket surface. herdr-mcp does not duplicate a plugin management API. |
| official Herdr | agent prompt lifecycle / blocked / idle / done | **adopted** | `herdr_prompt` calls native `agent.prompt`, default fire-and-forget; returns delivery evidence; wait timeout and transport error are separated. |
| official Herdr | events / session state / persistent background server | **web-suited parts adopted** | `herdr_since` cursor incremental recovery, SnapshotCache, `boot_id`; the web planner does not need every session/lifecycle method exposed. |
| herdr-mesh | agent-to-agent relay / handoff / wait/read | **partially adopted, deliberately not one-tool-per-action** | `herdr_prompt` + `herdr_since` + `herdr_call(agent.*)` can perform the same class of flow; the web planner orchestrates itself, no `handoff` intermediate orchestrator added. |
| herdr-mesh | a dedicated MCP tool for every pane/workspace/session operation | **not adopted** | would bloat the catalog; full reachability is preserved through `herdr_methods` + `herdr_call`. |
| DeepSeek Harness | `dsh --profile headless "job"` non-interactive coding agent | **verified as a backup worker** | local 0.1.1-rc.2 can answer pure questions and complete a real code change in a temporary Git repo; the final reply may come later than 60s, so it must run as a long task via `herdr_exec_start/read`; after a timeout check Git/tests before deciding to retry. Pi/Herdr-native workers stay preferred. |
| dsh-tui | Harness full-screen interactive UI / session takeover | **human fallback** | the local `@deepseek-harness-tui/dsh-tui@0.9.0` profile composable successfully; for humans taking over, resuming sessions, approvals, and debugging — not the default machine-call interface for the web planner. |
| other herdr-mcp | recipe engine / React playground | **not adopted** | recipes become a second workflow DSL easily; this project's planner is the web model. Debugging is via tests, CLI, and real ChatGPT UAT. |
| other herdr-mcp | HTTP bridge | **covered by the product architecture** | `/mcp` + Cloudflare Worker/DO + WSS Link, with the public side decoupled from the local runtime lifecycle. |

## Capabilities this project formed itself, not simple benchmark copies

- `herdr_inspect`: aggregates workspace/tab/pane/agent, build, exec environment, managed roots into one cheap snapshot.
- `herdr_since`: for the "run only when the user sends a message" web model, a cursor that fetches only new changes.
- `herdr_prompt`: idempotency key, delivery evidence, error layering for TaskGroup vs post-submit wait; no blind mutation retries.
- `herdr_exec`: visible utility pane; local fallback only when control-plane failure happens before delivery; never double-runs after delivery.
- SnapshotCache + list-API fallback: Herdr `session.snapshot` blips no longer misjudge remote file/Git work as business blockers.
- Cloudflare stable Edge: OAuth, MCP transport, Durable Object, single active-link fencing, structured errors for runtime offline.
- Runtime generation A/B: atomic generation switch, drain and rollback within the **same contract epoch** without restarting the Link; Edge heartbeat syncs the current generation/version. Cross-epoch migration is supervised separately.
- Browser extension reverse channel: Herdr → `/push/events` → browser → the current web conversation, filling the gap that MCP is request-direction-only and a long task does not auto-continue the web conversation.

## Not added yet

1. **Do not expose a future contract epoch implicitly.** Epoch 2 / `herdr_skill` is now the production target; any later tool-catalog change must be captured as a new frozen epoch, migrated explicitly, and verified in a fresh ChatGPT conversation.
2. **Do not copy dozens of pane/agent/workspace MCP tools.** The live Herdr API is reachable through two generic tools.
3. **No recipe DSL / second planner.** Web ChatGPT is the only high-level planner.
4. **No shell sandbox claim.** The permission boundary stays explicit; stronger isolation, if ever needed, gets designed separately.
5. **The browser extension does not go through the public Worker.** The extension is a same-machine reverse channel. Current builds use Native Messaging plus the runtime's mode-`0600` Unix socket, so public OAuth and the local static runtime credential remain separate and the browser receives no Herdr bearer.

## Maintenance

- Before adding any new MCP tool, judge whether `herdr_call` or existing workstation primitives can express it.
- Every time a benchmark project is compared, update this table's "adopted / partially adopted / not adopted" verdicts instead of copying APIs.
- Production ChatGPT tool-catalog changes must go through a new contract epoch; runtime implementation upgrades are not ABI upgrades.