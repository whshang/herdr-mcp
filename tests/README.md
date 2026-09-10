# tests/

## Default suite (`npm test`)

Runs `npm run build`, then the built-runtime regression lane (`tests/*.test.mjs`) plus `tests/manual/background_bind_test.mjs`. CI/release gates that already built `dist/` use `npm run test:built` to avoid a duplicate TypeScript build.

| File | Covers |
|---|---|
| `transport.test.mjs` | Streamable HTTP, openai-mcp stateless, sessions, discover, schema hygiene, version cache-key |
| `fs_browse.test.mjs` | Default 18 tools (includes `herdr_skill`), `herdr_fs_list` / `herdr_fs_grep` / write gates, `overwrite` schema |
| `herdr-skill.test.mjs` | Skill pointer + bundled fallback metadata |
| `timeouts.test.mjs` | RPC timeout clamp (≤60s) |
| `atomic-files.test.mjs` | `commitAtomic` rollback (new-file cleanup + backup restore) |
| `exec-sessions.test.mjs` / `exec-both-order.test.mjs` | closed/signal + `stream=both` interleave |
| `local-exec.test.mjs` | `herdr_exec` TaskGroup fallback backend (`runLocalShell`) |
| `agent-visibility.test.mjs` / `patch.test.mjs` / `prompt-semantics.test.mjs` | allowlist, patch parse, TaskGroup classification |

## Manual / integration (`tests/manual/`)

Most files here are not in `npm test`; `background_bind_test.mjs` is the deliberate exception. Run the others explicitly when needed:

| File | When |
|---|---|
| `mcp_activity_smoke.mjs` | `/push/mcp-activity` ring buffer |
| `extension_smoke.mjs` | Extension static + pure logic |
| `background_bind_test.mjs` | Binding state machine with chrome mocks |
| `e2e_a1a2.mjs` | Advanced surface (`HERDR_MCP_ALL_TOOLS=1`; default 18-tool surface drops wait/reap) wait semantics |
| `l45_reap_project.mjs` / `test_reap_safety.mjs` / `test_p0crit.py` | Reap / project gates (advanced tools) |
| `smoke_schema.mjs` / `smoke_state.mjs` | Ad-hoc schema/state probes |

Prefer adding regressions to `*.test.mjs` so CI/`npm test` catches them.
