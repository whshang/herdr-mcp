# Architecture

Audience: contributors deciding whether a capability belongs in MCP or in the native herdr API.

## The remote control plane

<section class="diagram" aria-label="Architecture">
  <div>ChatGPT / web model</div><span>↓ MCP + OAuth</span>
  <div>Cloudflare Worker / Durable Object</div><span>↓ authenticated WSS</span>
  <div>persistent herdr-link</div><span>↓ local MCP</span>
  <div>runtime generation A / B</div><span>↓ Herdr socket + workstation</span>
  <div>panes · agents · fs · git · shell</div>
</section>

## Two processes

| Process | Role |
|---|---|
| **herdr** | Local terminal multiplexer + agent runtime. The Unix socket API is large (`herdr api schema`, ~90 methods). |
| **herdr-mcp** | HTTP MCP facade (Streamable HTTP + OAuth) that lets **remote** clients drive herdr and the workstation. |

herdr-mcp does **not** turn every herdr method into an MCP tool. That would burn context and duplicate the native schema.

## Tool surface: production contract epoch 2 is 18 tools

The MCP tool surface is **fixed**; the live herdr schema only serves `herdr_methods` / `herdr_call`. Runtime 0.3.32 freezes the public ChatGPT catalog at **contract epoch 2 / 18 tools**, including the read-only `herdr_skill`. The exact epoch-1 / 17-tool 0.3.23 catalog remains tracked only for supervised rollback and old-session compatibility; it is no longer the production target.

| Layer | Tools | Notes |
|---|---|---|
| Skill | `herdr_skill` | Local process pulls the upstream Herdr `SKILL.md` (master, not pinned); on failure uses `assets/herdr-agent-SKILL.md`. ChatGPT does not reach GitHub. |
| Passthrough | `herdr_methods`, `herdr_call` | Reflects and invokes native socket methods |
| Remote orchestration | `herdr_inspect`, `herdr_since`, `herdr_prompt` | Glance / incremental read / prompt delivery suited to chat-style clients |
| Remote workstation | `herdr_fs_*`, `herdr_exec` / `herdr_exec_*`, `herdr_git` | **Not** herdr capabilities — the remote client itself has no disk |

`HERDR_MCP_ALL_TOOLS=1` adds advanced/deprecated lifecycle tools (`herdr_wait`, `herdr_reap`, sessions, etc., 30 total). Turn it off for ChatGPT to save context. Start a normal epoch-2 session with `herdr_inspect` → `herdr_skill` (once) → work.

## Design rules

1. **One correct path per current product** — no reserved configuration for an imagined second client.
2. **Changes** are confined to the managed git root; optional `HERDR_MCP_READONLY` / `HERDR_MCP_WRITE_ROOTS`.
3. **Delivery is uncertain** — do not blindly retry a non-idempotent prompt after a transport failure; check with inspect/since first. Mutations default to `herdr_prompt` (omit `wait`) with an `idempotency_key`; state via `herdr_since` / `herdr_inspect`.
4. **Version is a cache key** — bump `src/version.ts` + `package.json` when the tool surface or handshake semantics change.
5. **The web model orchestrates** — planning and scheduling live in the web model; locally prefer `herdr_fs_*` / `herdr_exec`; when an agent is needed, call a cheap worker directly — local Claude/OMP/main must not act as an intermediate conductor.
6. **Soft agent hiding** — `herdr_inspect` / `herdr_since` list only execution agents (`pi`/`cline`/`opencode`/`anti`) and auditors (`droid`/`grok`) by default; Claude/OMP/Codex do not appear in the list. `herdr_prompt` does **not** block. `HERDR_MCP_AGENT_ALLOW=*` shows everything; a comma list overrides the default.
7. **Production epoch2 = 18 tools** — 30 with `HERDR_MCP_ALL_TOOLS=1`; `inspect` includes `boot_id` + `exec_sessions` + `agent_skill` state. Epoch1 is a legacy compatibility profile only and explicitly reports `herdr_skill` as hidden. `HERDR_MCP_READONLY=1` blocks mutations including `herdr_prompt` (except `dry_run` of `herdr_fs_patch`).
8. **Workstation robustness (≥0.3.17)** — a failed `commitAtomic` deletes files added by that attempt; the exec journal kills only orphans that still carry `HERDR_MCP_EXEC_ID`; `exec_read stream=both` interleaves write order; `fs_read` byte truncation returns only complete lines.
9. **`herdr_exec` control-plane degradation (≥0.3.18)** — a utility pane that keeps hitting TaskGroup **before** `send_text` automatically switches to local zsh (`backend:local_fallback`); once delivered, it is never re-sent nor degraded (avoids double execution).
10. **`herdr_git` local degradation (≥0.3.20)** — when `session.snapshot` / managed-roots gates are unavailable due to TaskGroup, real git roots under `$HOME` (or `HERDR_MCP_WRITE_ROOTS`) still run local `git` directly (with `warnings`); read-only RPCs such as `pane.read` transparently retry against TaskGroup, up to 4 attempts.
11. **`herdr_inspect` / `liveSnapshot` list degradation (≥0.3.21)** — when `session.snapshot` hits TaskGroup and the cache is insufficient, fall back to composing `workspace.list` + `pane.list` + `agent.list`, with `warnings` containing `snapshot_failed_used_list_apis`; do not treat a control-plane anomaly as a repository blocker.
12. **Unix socket read timeout + 60s RPC cap (≥0.3.23)** — every socket RPC sets `setTimeout` after connect (no longer waits forever on `session.snapshot`); `herdr_exec` / `herdr_wait` / `herdr_prompt` waits are all **≤60s**; `herdr api schema` startup warm-up + stale cache (tools/list does not depend on live herdr); SnapshotCache bootstrap prefers bounded snapshots and falls back to list APIs (aligned with coding-tools-mcp: **fixed MCP tool surface**, live only as runtime).

## Error semantics (`herdr_call` / `herdr_prompt` / read-only aggregates)

| `failure` / `failure_phase` | Meaning | Blind retry? |
|---|---|---|
| `herdr_transport` | Real connection/socket problem | Depends on the method; mutations still verify first |
| `agent_status_wait_timeout` / `post_submission_status_wait` | Timed out waiting for agent status after delivery (common with `agent.prompt` and `wait`) | **No** — inspect/since first |
| `herdr_internal` / `control_plane_taskgroup` (or `snapshot_refresh`) | Daemon control-plane TaskGroup / ExceptionGroup blip; **not** a missing pane, not a prompt delivery timeout. `herdr_prompt` ≥0.3.22 also falls in this class (with `delivery_uncertain`), no longer just bare `UNKNOWN` | Read-only retryable; **`agent.prompt` must not be blindly retried** — inspect/since first |

| `herdr_error` | Other daemon business errors | Depends on `retryable` |

## Transient control-plane failures (ExceptionGroup / TaskGroup)

Scope: ChatGPT ↔ herdr-mcp ↔ herdr daemon/socket ↔ workspace/pane/agent state layers.  
**Not** business repository code, and not the Claude/OMP runtime.

Typical symptom: the agent is still `working`, but `inspect` / `since` / `pane.read` / `fs_*` intermittently fail; seconds later the same request succeeds. The root cause is in the herdr daemon's concurrent aggregation (snapshot / events.subscribe / socket reconnect): when one child task throws, it is not isolated and the whole RPC is wrapped as a bare `ExceptionGroup`.

### Local watchdog (macOS LaunchAgent, ≥0.3.22, ships with 0.3.26)

`herdr-mcp watchdog install` registers a LaunchAgent (default every 120s). Linux / Windows have no equivalent CLI.

- If the MCP process is gone or the local `/mcp` is not 200/401: after consecutive failures reach a threshold, `herdr-mcp restart` (default 10-minute cooldown, **no daily limit**)
- If `agent.list` / `workspace.list` hit TaskGroup or the socket is missing: **log only**; do not restart the herdr daemon, do not re-deliver `herdr_prompt`
- State: `~/.config/herdr-mcp/watchdog.state.json`; log: `watchdog.log`

What herdr-mcp itself can do (≥0.3.12, exec degradation ≥0.3.18):

- read-only auto-retry (up to 2 control-plane blips)
- `failure=herdr_internal` + `code=snapshot_refresh_failed` + `retryable=true` (expands child exception text instead of letting a bare ExceptionGroup be the only information)
- `fs_*` / inspect use SnapshotCache as much as possible when snapshot fails
- `herdr_exec`: TaskGroup **before** `send_text` → retry, then `backend:local_fallback`; if already delivered, return a structured error and forbid degradation/re-send
- `herdr_git` (≥0.3.20): when snapshot/managed-roots fail, `spawn git` directly for git roots under local `$HOME` (not through a pane)
- `herdr_inspect` / `liveSnapshot` (≥0.3.21): on snapshot failure compose `workspace.list` (+pane/agent.list) with `warnings`
- after a failed mutation, still require inspect/since first; no blind retries

What cannot be eliminated: the real root cause of unisolated/unflattened TaskGroups in the daemon needs a herdr upstream fix. `pane.read` / full `session.snapshot` may still blip occasionally; orchestration should keep preferring `herdr_fs_*` / `herdr_git` / `herdr_exec` (TaskGroup before utility delivery → `local_fallback` / standalone local session), and treat control-plane blips as non-blocking.

A successful `herdr_prompt` includes `state_observation: { changed: true\|false\|"unknown", fresh }`; without `wait`, an unchanged snapshot is `"unknown"` (not "not delivered"). The compatibility field `state_changed` is retained.

## Remote workstation gates

| Gate | `herdr_fs_*` | `herdr_exec` |
|---|---|---|
| readonly / write_roots | Yes | Yes |
| secret-path (path validation) | Yes | **No** (a free shell can `cat .env`; use fs for file IO) |
| working agent | edit/write refused by default, `confirm_busy` gets through | refused by default, `confirm_busy` gets through |
| dirty confirm | edit/write have it | **No** (running commands on a dirty tree is normal) |

## Transport

- MCP: `POST/GET/DELETE` on `/mcp` (ChatGPT probing also uses an issuer-root `/` alias)
- Auth: OAuth JWT (connector) or a static `HERDR_MCP_TOKEN` (Cursor / curl / extension). Do not paste the static token into the ChatGPT connector UI.
- Push (extension, same Bearer):
  - `GET /push/events` SSE (supports `?workspace=`)
  - `GET /push/state` current agent / workspace / pane snapshot
  - `GET /push/mcp-activity` recent `tools/call` counts (in-process ring buffer; the current extension nudge uses a small-model decision, not a "zero tools" heuristic)

## Environment variables

Environment variables. The `HERDR_MCP_HOST` in the plist example is **not** read by the Node process; the service listens on a port and is reached via tunnel/local loopback. Version evolution: [CHANGELOG.md](../../../CHANGELOG.md).

| Variable | Default | Effect |
|---|---|---|
| `HERDR_MCP_TOKEN` | empty | Static Bearer for `/mcp` and `/push` |
| `HERDR_MCP_PORT` | `8772` | Listening port |
| `HERDR_MCP_BASE_URL` | empty | Public origin, **without** a `/mcp` suffix; OAuth `iss`/`aud` |
| `HERDR_SOCKET_PATH` | `~/.config/herdr/herdr.sock` | herdr API socket |
| `HERDR_MCP_READONLY` | off | Blocks mutations (incl. `herdr_prompt`; except `fs_patch` `dry_run`) |
| `HERDR_MCP_WRITE_ROOTS` | all managed roots | Roots allowed for writes, CSV |
| `HERDR_MCP_ALL_TOOLS` | off | 18 → 30 tools |
| `HERDR_MCP_AGENT_ALLOW` | workers + auditors | `*` or comma list; affects inspect/since lists, does not block `herdr_prompt` |
| `HERDR_MCP_STATE_DIR` | `~/.config/herdr-mcp` | exec journal / sessions |
| `HERDR_MCP_OAUTH_DIR` | `~/.config/herdr-mcp/oauth` | JWT keys and client registrations |
| `HERDR_MCP_OAUTH_ACCESS_TTL_S` | `86400` | access token TTL |
| `HERDR_MCP_OAUTH_REFRESH_TTL_S` | `2592000` | refresh token TTL |
| `HERDR_MCP_PUSH_DEBUG` | off | `/push` debug logging |
| `HERDR_MCP_BUILD_COMMIT` / `HERDR_MCP_BUILT_AT` | `dev` / startup time | `inspect.workstation_info` |
| `HERDR_SKILL_URL` | herdr master `SKILL.md` raw URL | `herdr_skill` upstream |
| `HERDR_SKILL_CACHE_SEC` | `3600` | skill cache |
| `HERDR_SKILL_FETCH_TIMEOUT_MS` | `15000` | skill fetch timeout |
| `HERDR_SKILL_NETWORK` | on | `0` = use only the bundled copy |

## Related documentation

- [CHANGELOG.md](../../../CHANGELOG.md) — versions and tool surface
- [capability-benchmark.md](./capability-benchmark.md) — adoption and "not adopted" decisions vs. official Herdr / other Herdr MCP implementations / coding-tools-mcp
- [extension.md](./extension.md) — extension overview (A continuity + B local JSON→MCP both available)
- [chatgpt-connector.md](./chatgpt-connector.md) — ChatGPT OAuth, schema, permission cards
- [extension-wake.md](./extension-wake.md) — track A: workspace observation + global manual/per-Project mode + Project automation switch + progress/settled push-back + Manual continue/monitor/LLM analysis + independent Manual handoff + timeout recovery / safe Project rollover
- [extension-bridge.md](./extension-bridge.md) — track B: JSON→MCP (parsing exists, closed loop does not)