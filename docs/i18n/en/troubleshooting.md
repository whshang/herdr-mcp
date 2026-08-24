# Troubleshooting

A symptom-first checklist for the common failure modes. When in doubt, start a new conversation after reconnecting — a large share of “0 tools” reports are stale snapshots, not a dead service.

## ChatGPT shows 0 tools, or an old tool count

- **Start a new conversation** after reconnecting; an old conversation keeps its old tool snapshot.
- Verify the same origin is used for the MCP URL **and** `OAUTH_ISSUER` (no `/mcp` suffix in the environment variable).
- Check Edge health, `herdr-link` connectivity and OAuth discovery (`/.well-known/oauth-authorization-server`).
- Current production contract is epoch 2 with **18 tools including `herdr_skill`**. If ChatGPT still shows the epoch‑1 17‑tool list, the conversation/Connector cache is stale. The runtime version comes from `/.well-known/mcp.json` / `initialize.serverInfo.version`.

Hard requirements and diagnostics: [chatgpt-connector](chatgpt-connector.md).

## The MCP Connector card keeps popping up

Verify the extension is loaded, the current tab is `chatgpt.com`, and the content-script version is ≥ 0.1.3 (old tabs refresh after the extension reloads). See [extension](extension.md).

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