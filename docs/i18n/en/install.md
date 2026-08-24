# Install

Get a remote planner (ChatGPT or another web model) talking to your local Herdr workstation through herdr-mcp. The supported flow is: install and start the local MCP server first, then deploy the Cloudflare Edge and connect ChatGPT — the Edge is what ChatGPT actually talks to, so deploy it before creating the Connector.

See [architecture](architecture.md) for the full system boundary, and [chatgpt-connector](chatgpt-connector.md) for the Connector contract in detail.

## Prerequisites

- [herdr](https://herdr.dev) installed and running (the server connects to Herdr's API socket, it does not scan an install directory).
- Node.js 20+ (`node -v`).
- For ChatGPT: a Cloudflare Worker endpoint on `workers.dev` (default, no custom domain required). A Custom Domain is optional for a stable long-lived origin; direct `cloudflared` exposure is kept only as a legacy migration path.

## 1. Download and build

```bash
git clone https://github.com/whshang/herdr-mcp.git
cd herdr-mcp
npm install
npx tsc
mkdir -p ~/.config/herdr-mcp
```

## 2. Start the local MCP server

```bash
export HERDR_MCP_TOKEN="$(openssl rand -hex 16)"
echo "token=$HERDR_MCP_TOKEN"   # keep for Cursor / the browser extension
node dist/server.js
# optional check: curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

The server listens on `127.0.0.1:8772` by default (`HERDR_MCP_PORT` overrides it). The static token above is for Cursor / curl — **never paste it into ChatGPT**, which authenticates through OAuth at the Edge instead.

### macOS: run as a LaunchAgent

```bash
ln -sf "$PWD/bin/herdr-mcp" ~/.local/bin/herdr-mcp
herdr-mcp start     # LaunchAgent
herdr-mcp status
herdr-mcp logs [-f]
herdr-mcp watchdog install   # restart MCP if down, every 120s
```

`npm`'s `bin` is `dist/server.js`, not the bash CLI; on macOS you may link the CLI into `~/.local/bin` as above. After code changes: `npx tsc && herdr-mcp restart` (or restart the `node dist/server.js` process).

## 3. Deploy the Cloudflare Edge

The supported default does not require your own domain — the worker runs on your account's `workers.dev` hostname:

```bash
cp edge/cloudflare/wrangler.user.example.toml edge/cloudflare/wrangler.user.toml
# edit worker name, workstation id and OAUTH_ISSUER for your workers.dev origin
cd edge/cloudflare
npx wrangler deploy --config wrangler.user.toml
```

The result is a stable origin, for example:

```text
https://herdr-edge.<your-account-subdomain>.workers.dev/mcp
```

If you own a Cloudflare zone, a Custom Domain such as `herdr.example.com` is **recommended but optional** — validate the Worker on `workers.dev` first, then attach it. See [cloudflare-edge-deployment](cloudflare-edge-deployment.md) and [cloudflare-edge-token](cloudflare-edge-token.md).

## 4. Connect ChatGPT

1. Open ChatGPT settings and enable **Developer mode**.
2. Create a custom MCP connector.
3. Enter the Edge MCP URL: `https://<worker>.<account>.workers.dev/mcp` (or your Custom Domain + `/mcp`).
4. Complete OAuth in the browser; do not paste the local Herdr token into ChatGPT.
5. Start a new chat after connecting so it receives a fresh tool snapshot.

Runtime releases can switch behind the persistent Edge/Link without changing the Connector — see [runtime-self-upgrade](runtime-self-upgrade.md).

## Verify the install

- Local: `curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/` returns `200` or `401` (401 means it is up and asking for a token).
- Edge: open the worker's `/health` and confirm the workstation Link is connected.
- In ChatGPT: the connected conversation should list 18 tools, including `herdr_skill`. If tools are missing, see [troubleshooting](troubleshooting.md) — the usual cause is a stale conversation, not a dead server.