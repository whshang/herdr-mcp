# Local-Agent install and `workers.dev` deployment

This is an execution contract for a **local coding Agent**, not a list of commands for the human to copy. The human should only need to perform Cloudflare interactive login/API-Token creation; the Agent owns environment checks, clone, build, local service setup, Cloudflare Worker deployment, the outbound WSS Link, and verification.

> Current boundary: fully automated background-service setup is macOS-first. The Node server itself supports macOS / Linux / Windows. On Linux/Windows the Agent may clone/build/deploy the Edge, but should integrate long-running local processes with the user's existing process manager instead of inventing unsupported systemd/Task Scheduler files.

## 0. Agent contract

1. Execute work directly; do not offload automatable shell steps to the human. Pause only for Cloudflare interactive login/API-Token creation or selection among multiple Cloudflare Accounts.
2. Preserve existing work. If an existing checkout is dirty, never `reset --hard`, `clean -fd`, or overwrite user changes; use the safe current checkout or clone a side directory.
3. First install uses `workers.dev` only. Do not create a Custom Domain, DNS record, Cloudflare Tunnel, or mutate an existing zone.
4. Treat the Cloudflare Token as a high-sensitivity credential. Never echo it or write it to the repo, `.env`, ordinary logs, screenshots, or shell history. Prefer process-environment injection; if a temporary file is unavoidable, use mode `0600` and delete it immediately after deployment.
5. Verify every mutation before continuing. On an error, determine whether the mutation already committed before retrying.

## 1. Prerequisites

Run `git --version`, `node --version`, `npm --version`, `herdr --version`, and `herdr api schema >/dev/null`. Require Node.js `>=20`, a working `herdr` binary, and the Herdr socket (default `~/.config/herdr/herdr.sock`, or an explicit `HERDR_SOCKET_PATH`). If Herdr itself is not installed/running, stop and direct the user to <https://herdr.dev>; herdr-mcp does not replace Herdr.

## 2. Clone/update and build

For a new install:

```bash
git clone https://github.com/whshang/herdr-mcp.git ~/herdr-mcp
cd ~/herdr-mcp
npm ci
npm run build
```

For an existing checkout, inspect `git status --short` first. A clean checkout may `git fetch origin main` + `git pull --ff-only`; preserve a dirty checkout.

## 3. Generate local identities without printing secrets

Generate in Agent memory: `HERDR_MCP_TOKEN`, `LINK_SHARED_SECRET`, and a hostname-derived `WORKSTATION_ID` limited to `[A-Za-z0-9_.-]` and 64 chars. Generate `WORKER_NAME` only through the repository helper; the Agent must not invent its own hostname slug:

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

`WORKER_NAME` and `WORKSTATION_ID` intentionally use different grammars. The helper lowercases the hostname, safely handles every character outside `[a-z0-9-]` (including `.`, `_`, whitespace, and non-ASCII input), collapses/trims `-`, and keeps the complete Worker name at or below 63 characters. The result must match `^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$`; for example `MacBook.local` becomes `herdr-edge-macbook-local`. Use strong randomness such as `openssl rand -hex 32` for secrets; never include secrets in the final report.

## 4. Only human pause: Cloudflare API Token

Open <https://dash.cloudflare.com/profile/api-tokens> when browser control is available; otherwise give the user that URL.

The simplest supported path is Cloudflare's current **Edit Cloudflare Workers** template, scoped to the single Account used for this install. Cloudflare's Wrangler CI/CD documentation uses this template. Do **not** add DNS Write.

For a tighter custom token, retain at least Account → **Workers Scripts → Write/Edit**, Account → **Account Settings → Read**, User → **Memberships → Read**, and User → **User Details → Read**. `workers.dev` bootstrap does not need Zone/DNS permissions.

References: <https://developers.cloudflare.com/fundamentals/api/get-started/create-token/>, <https://developers.cloudflare.com/fundamentals/api/reference/template/>, <https://developers.cloudflare.com/workers/ci-cd/external-cicd/github-actions/>.

Tell the user the secret is shown once and ask them to paste it only into the current local-Agent session; prefer a dedicated secret-input channel when available.

## 5. Cloudflare preflight after the Token arrives

Inject it only as temporary `CLOUDFLARE_API_TOKEN`, never as a literal command-line argument. Verify `GET https://api.cloudflare.com/client/v4/user/tokens/verify`, then run `npx wrangler whoami` under `edge/cloudflare`.

- one Account → select automatically;
- multiple Accounts → ask only which Account name to use;
- invalid/under-scoped Token → stop mutations and explain the missing permission.

After selection, keep the account ID only in the current deployment process environment:

```bash
export CLOUDFLARE_ACCOUNT_ID="$ACCOUNT_ID"
```

Do not write a personal `account_id` into tracked Wrangler config. Subsequent `wrangler deploy` and `wrangler secret put` inherit this temporary environment.

With `ACCOUNT_ID`, fetch `GET /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain`. Reuse an existing account subdomain and **never rename it**. `ACCOUNT_SUBDOMAIN` is the selected Cloudflare Account's `workers.dev` subdomain returned by the Cloudflare API. It is not the workstation username, hostname, or the Cloudflare Account display name. The Worker origin always combines two independent values as `<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`.

If no account subdomain exists, create one only because there is no old value; use `herdr-<short-account-id>` plus a random suffix on collision. API reference: <https://developers.cloudflare.com/api/resources/workers/subresources/subdomains/>.

Only after GET explicitly confirms that no subdomain exists, create one with:

```text
PUT /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain
Content-Type: application/json

{"subdomain":"<candidate>"}
```

GET it again afterward and require the returned value to match before deploying the Worker.

## 6. Generate Wrangler config and create the Worker

Copy `edge/cloudflare/wrangler.user.example.toml` to ignored `wrangler.user.toml`, then set `name`, `DEFAULT_WORKSTATION_ID`, and `OAUTH_ISSUER=https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`. Keep `workers_dev = true` and `routes = []`.

Deploy:

```bash
cd edge/cloudflare
npx wrangler deploy --config wrangler.user.toml
```

Never overwrite a pre-existing Worker unless this install can prove it owns it; choose a machine-specific/random-suffixed name instead. Then store the WSS shared secret as a Cloudflare Worker secret:

```bash
printf '%s' "$LINK_SHARED_SECRET" | npx wrangler secret put LINK_SHARED_SECRET --config wrangler.user.toml
```

No Zone/DNS mutation is required.

## 7. macOS local MCP LaunchAgent

Render `deploy/dev.herdr-mcp.server.plist.example` to `~/Library/LaunchAgents/dev.herdr-mcp.server.plist`, replacing its placeholders with the current Node path, repo path, HOME, `HERDR_MCP_TOKEN`, Herdr socket, and `HERDR_MCP_BASE_URL=https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`. Then:

```bash
launchctl bootout "gui/$UID/dev.herdr-mcp.server" >/dev/null 2>&1 || true
launchctl bootstrap "gui/$UID" "$HOME/Library/LaunchAgents/dev.herdr-mcp.server.plist"
launchctl enable "gui/$UID/dev.herdr-mcp.server"
mkdir -p ~/.local/bin
ln -sf "$PWD/bin/herdr-mcp" ~/.local/bin/herdr-mcp
```

Verify `127.0.0.1:8772/mcp` and never print `HERDR_MCP_TOKEN` in the final report.

Before asking the user to load the browser extension, install its Chrome Native Messaging host:

```bash
bin/herdr-extension-host install
bin/herdr-extension-host status
```

The host manifest contains no long-lived secret. The installer derives Chromium's stable unpacked-extension id from the absolute `<repo>/extension` path and restricts the host to that exact `chrome-extension://<id>/` origin. This preserves the existing unpacked-extension identity when the same directory is reloaded. On macOS it registers the host for Chrome plus detected Chromium-family profiles including Chromium, Brave, Edge, and ego lite. Current extension builds send bounded request/stream messages to the native host, which reaches herdr-mcp through `~/.config/herdr-mcp/extension.sock` (mode `0600`). No Herdr bearer is returned to or stored by the extension. The host can still use the existing LaunchAgent token internally when talking to an older runtime that has no IPC socket. Do not copy `HERDR_MCP_TOKEN` into extension storage during a normal install.

## 8. macOS persistent Herdr Link

Store `LINK_SHARED_SECRET` in Keychain under `herdr-edge-link-<WORKSTATION_ID>`. The command text must reference the environment variable rather than a literal secret:

```bash
export HERDR_LINK_KEYCHAIN_SERVICE="herdr-edge-link-$WORKSTATION_ID"
security add-generic-password -U -a "$(id -un)" -s "$HERDR_LINK_KEYCHAIN_SERVICE" -w "$LINK_SHARED_SECRET"

HERDR_EDGE_URL="wss://$WORKER_NAME.$ACCOUNT_SUBDOMAIN.workers.dev/ws" \
HERDR_WORKSTATION_ID="$WORKSTATION_ID" \
HERDR_LINK_KEYCHAIN_SERVICE="$HERDR_LINK_KEYCHAIN_SERVICE" \
bin/herdr-link install
```

The script resolves Node from PATH; `HERDR_NODE_BIN` can override it.

## 9. Verify the closed loop

Verify local `server/discover`, `herdr-mcp status`, `bin/herdr-extension-host status`, `bin/herdr-link status`, Worker `/health`, final `/mcp`, OAuth discovery, and that no Custom Domain/DNS/Tunnel was created.

## 10. Clean up the bootstrap Token

Unset `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`, then delete temporary credential files. Do not copy the Token into project config. Recommend revocation if it was one-time; otherwise move it to a dedicated secret manager/CI secret.

## 11. Final report

Return only non-sensitive facts: the absolute repository directory, the absolute browser-extension directory (`<repo>/extension`), local MCP status, Herdr Link status, Cloudflare Account name + shortened ID, Worker name, `workers.dev` origin, `/health`, and `/mcp`.

Then give the browser-extension install steps: open `chrome://extensions`, enable Developer mode, choose **Load unpacked**, and select the reported `extension` directory. The installed Native Messaging host carries current extension traffic over the runtime's mode-`0600` Unix socket; the user does not copy `HERDR_MCP_TOKEN` into Options and current Options has no Herdr Token field.

Finally guide the user to enable ChatGPT Developer mode, create a custom MCP Connector with `/mcp`, and complete OAuth. Never paste the local `HERDR_MCP_TOKEN` or Cloudflare Token into ChatGPT.
