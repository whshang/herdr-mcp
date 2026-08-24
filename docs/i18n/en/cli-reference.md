# CLI reference

herdr-mcp ships two command surfaces: the `herdr-mcp` bash CLI (macOS, LaunchAgent-oriented) and a small set of Node `bin/` maintenance tools. The npm `bin` is `dist/server.js` — the server itself, not the CLI.

## herdr-mcp (macOS CLI)

```bash
herdr-mcp              # menu
herdr-mcp status
herdr-mcp connector
herdr-mcp start | stop | restart   # LaunchAgent
herdr-mcp logs [-f]
herdr-mcp token | url
herdr-mcp lang [en|zh|ja]   # UI language (first run: system; default en)
herdr-mcp watchdog install  # every 120s: restart MCP if down; TaskGroup = log only
herdr-mcp watchdog status
```

After code changes: `npx tsc && herdr-mcp restart` (or restart the `node dist/server.js` process).

## bin/ maintenance tools

| Command | Purpose |
|---|---|
| `bin/herdr-cloudflare-token` | Create a least-privilege Cloudflare Account API Token for Herdr (Workers Routes Write on the zone, Workers Scripts Write on the account). Writes `~/.config/herdr-mcp/cloudflare-cutover.env` (`0600`), never prints the token. See [cloudflare-edge-token](cloudflare-edge-token.md). |
| `bin/herdr-cloudflare-dns-token` | Issue the narrow DNS token used only by the one-shot migration path, not by daily operations. |
| `bin/herdr-cloudflare-domain` | Attach/detach a Custom Domain to the deployed Worker via the Cloudflare Workers Domains API. See [cloudflare-edge-deployment](cloudflare-edge-deployment.md). |
| `bin/herdr-custom-domain-cutover` | One-time cutover of a legacy CNAME/Tunnel hostname to the Worker Custom Domain. |
| `bin/herdr-runtime-generation` | Manage runtime A/B generations: `status`, `register --generation <id>`, `activate --generation <id>`, `rollback`, `remove --generation <id>`. See [runtime-self-upgrade](runtime-self-upgrade.md). |
| `bin/herdr-self-update` | Supervised self-upgrade reusing the runtime-generation machinery; refuses a cross-contract-epoch migration. |
| `bin/herdr-link` | Workstation → Edge WSS sidecar (LaunchAgent). The workstation only ever establishes an outbound authenticated connection. |

## Environment variables

| Variable | Default | Meaning |
|---|---|---|
| `HERDR_MCP_PORT` | `8772` | Local Express port (bound to `127.0.0.1`). |
| `HERDR_MCP_TOKEN` | — | Static Bearer for Cursor / curl. Never give this to ChatGPT — use OAuth. |
| `HERDR_MCP_BASE_URL` | — | Public Edge origin; keep it exactly consistent with `OAUTH_ISSUER`. |
| `HERDR_SOCKET_PATH` | `~/.config/herdr/herdr.sock` | Herdr daemon socket. |
| `HERDR_MCP_AGENT_ALLOW` | — | `*` shows every pane; by default Claude/OMP/Codex are soft-hidden from `inspect`/`since`. |
| `HERDR_SKILL_NETWORK` | — | `0` forces the bundled skill copy instead of fetching upstream `SKILL.md`. |

Related reading: [architecture](architecture.md) (environment and gates), [cloudflare-edge-token](cloudflare-edge-token.md) (token workflow), [runtime-self-upgrade](runtime-self-upgrade.md) (generation lifecycle).