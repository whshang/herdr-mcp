# Quick agent install: one sentence for users, full protocol for your coding Agent

This page is the **end-user onboarding path** for herdr-mcp GA. It is not the maintainer UAT playbook in `docs/_wip/`.

## One sentence to paste into your local coding Agent

Copy this block to Codex, Claude Code, Cursor, Pi, Cline, or any agent that can read URLs and run shell commands:

```text
Install herdr-mcp for me. Read and follow the full protocol at https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/quick-agent-install.md end to end. Use GitHub Releases for the local runtime (not git clone). Pause only for Cloudflare login/API Token creation. Do not echo or commit secrets.
```

The Agent should read this entire document and execute it. For the detailed Cloudflare Token pause flow, it may also consult [Agent-assisted installation](agent-install.md).

## What the user gets

After a successful run:

- Herdr + native `herdr-mcp` runtime installed and healthy
- A personal Cloudflare Edge Worker (`workers.dev` or custom domain)
- Outbound Herdr Link from the workstation to Edge
- ChatGPT Developer mode + custom MCP Connector wired to `/mcp`
- Optional browser extension for long-session continuity (only after ChatGPT works manually)

## Agent contract (short)

1. Execute shell steps directly. Pause only for Cloudflare interactive login / API Token creation, or when choosing among multiple Cloudflare Accounts.
2. Install the **local MCP runtime from GitHub Releases**. Do not use `git clone` + `npm`/`cargo` unless the human explicitly asked to develop herdr-mcp itself.
3. Verify every mutation before continuing (`herdr-mcp doctor`, Link status, Edge `/health`, public `/mcp`).
4. Never echo or write secrets to the repo, logs, screenshots, or shell history.

## Prerequisites

```bash
herdr --version
herdr api schema >/dev/null
```

If Herdr is missing, stop and send the human to <https://herdr.dev>. herdr-mcp does not replace Herdr.

**Platform:** macOS Apple Silicon is the first fully supported GA path. Node.js is only needed temporarily for `npx wrangler` Edge deploy, not for the local runtime.

## Step 1 — Install native runtime

1. Download `herdr-mcp` from <https://github.com/whshang/herdr-mcp/releases> — use the latest stable release ([`v0.4.0`](https://github.com/whshang/herdr-mcp/releases/tag/v0.4.0) or newer stable tag)
2. Place on `PATH` (for example `~/.local/bin/herdr-mcp`) and make executable
3. Run:

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
```

## Step 2 — Choose public Edge URL strategy

Before deploying Edge, decide how ChatGPT will reach the workstation:

```text
Do you own a domain you can point at Cloudflare?
  ├─ YES → prefer Custom Domain for the Connector URL
  │         Example MCP URL: https://herdr-mcp.example.com/mcp
  │         See "Custom domain path" below
  └─ NO  → use workers.dev for first install
            Example MCP URL: https://herdr-edge-device.username.workers.dev/mcp
            In China / restrictive networks, also configure Link proxy (see below)
```

| Situation | Recommended public origin | ChatGPT Connector URL |
|---|---|---|
| Own a domain + Cloudflare zone | Custom Domain | `https://herdr-mcp.example.com/mcp` |
| No domain / fastest first install | `workers.dev` | `https://herdr-edge-device.username.workers.dev/mcp` |
| `workers.dev` blocked (China SNI) | Custom Domain **or** `workers.dev` + proxy | same patterns as above |

Full Custom Domain operations: [Cloudflare Edge deployment](cloudflare-edge-deployment.md#when-to-use-a-custom-domain).

## Step 3 — Cloudflare Token pause (human only)

Open <https://dash.cloudflare.com/profile/api-tokens> and create a token with **Edit Cloudflare Workers** scoped to one Account. Do **not** add DNS Write for the default `workers.dev` bootstrap.

Inject only as temporary process env:

```bash
export CLOUDFLARE_API_TOKEN='...'
```

See [Agent-assisted installation](agent-install.md) §4–§5 for verify/`whoami`/account selection. Unset the token after deploy.

## Step 4 — Deploy Edge

Generate identities in Agent memory (never print): `HERDR_MCP_TOKEN`, `LINK_SHARED_SECRET`, `WORKSTATION_ID`, and:

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

Deploy with `workers_dev = true` and `routes = []` for the default path. Record:

```text
EDGE_ORIGIN=https://${WORKER_NAME}.${ACCOUNT_SUBDOMAIN}.workers.dev
HERDR_EDGE_URL=wss://${WORKER_NAME}.${ACCOUNT_SUBDOMAIN}.workers.dev/ws
MCP_URL=${EDGE_ORIGIN}/mcp
```

Store `LINK_SHARED_SECRET` as a Worker secret. Details: [Agent-assisted installation](agent-install.md) §6.

### Custom domain path (when user owns a domain)

Only when the human has a domain on Cloudflare:

1. Add a Worker route for the hostname (for example `herdr-mcp.example.com/*`)
2. Create DNS pointing to the Worker
3. Set `OAUTH_ISSUER=https://herdr-mcp.example.com` in `wrangler.user.toml`
4. Redeploy and record `MCP_URL=https://herdr-mcp.example.com/mcp`

Do not mix issuer and Connector URL from different origins.

## Step 5 — Install Herdr Link (with network / China notes)

Install the managed Rust Link:

```bash
herdr-mcp link install
herdr-mcp link status
```

Set `HERDR_EDGE_URL` and `HERDR_WORKSTATION_ID` on the Link LaunchAgent to match the deployed Worker.

### Link proxy (workers.dev in China or via system proxy)

Link connects **outbound WSS** to Edge. If ChatGPT works through a local proxy but Link fails with connection reset, configure proxy env before `link install` or on the LaunchAgent:

| Variable | Purpose |
|---|---|
| `HERDR_LINK_PROXY` | Explicit override for Link WSS (highest priority) |
| `HTTPS_PROXY` / `https_proxy` | Standard HTTPS proxy (used for `wss://`) |
| `HTTP_PROXY` / `http_proxy` | Fallback HTTP proxy |
| `ALL_PROXY` / `all_proxy` | Last-resort env proxy (HTTP/HTTPS schemes) |

Example:

```bash
export HERDR_LINK_PROXY=http://127.0.0.1:7890
# or rely on existing https_proxy if ChatGPT already uses it
herdr-mcp link install
```

On macOS, Link also reads system proxy from `scutil --proxy` when env vars are unset.

**Agent behavior:**

1. Probe whether `https_proxy` / `HERDR_LINK_PROXY` / system proxy is available
2. If ChatGPT works but proxy cannot be detected, continue — transparent proxy may still work
3. If `workers.dev` remains unreachable after proxy, present **both** options:
   - set `HERDR_LINK_PROXY` (or system `https_proxy`) and retry Link
   - **or** switch to Custom Domain on a hostname the network can reach

Without proxy, Link uses a direct connection (unchanged default).

## Step 6 — Verify

```bash
herdr-mcp doctor
herdr-mcp link status
curl -fsS "${EDGE_ORIGIN}/health"
curl -s -o /dev/null -w '%{http_code}\n' "${EDGE_ORIGIN}/mcp"
```

`herdr-mcp doctor` should show Link ownership and Edge layers healthy (`edge-reachable`, `oauth-metadata`, `mcp-endpoint`; `401 auth=not-sent` is acceptable).

Unset `CLOUDFLARE_API_TOKEN` when done.

## Step 7 — Connect ChatGPT (manual, practical)

Do this **before** the browser extension.

1. Open ChatGPT → **Settings** → **Apps** / **Connectors** (names vary by plan)
2. Enable **Developer mode**
3. Browse connectors → **+** (top right)
4. Name it `herdr` (or any short name)
5. Connector URL — use your deployed origin:
   - `https://herdr-edge-device.username.workers.dev/mcp`, or
   - `https://herdr-mcp.example.com/mcp`
6. Check **I understand and wish to continue**
7. Complete browser OAuth
8. In chat: add the plugin **or** create a Project and add the plugin there (Project path works better with the browser extension relay later)
9. First prompt in a **new** chat:

```text
分析我的 herdr 里有哪些项目
```

English equivalent: `What projects do I have in Herdr? Inspect only.`

Success means OAuth completes, tools appear, and `herdr_inspect` returns real workstation data.

More detail: [ChatGPT Connector](chatgpt-connector.md).

## Step 8 — Optional browser extension (after ChatGPT works)

Only after Step 7 succeeds.

The extension folder in git checkouts is often hidden (`.cursor`, dotfiles). For **Load unpacked** in `chrome://extensions`:

**Recommended:** copy or symlink to a visible path:

```bash
cp -R extension ~/Documents/herdr-mcp-extension
# or: ln -s "$(pwd)/extension" ~/Documents/herdr-mcp-extension
```

Then in Chrome: Developer mode → **Load unpacked** → select `~/Documents/herdr-mcp-extension`.

**Alternative:** in the file picker press **Cmd+Shift+.** to show hidden files and pick `extension/` from the checkout.

Install Native Messaging when `herdr-mcp doctor` is healthy:

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

See [Browser continuity](browser-continuity.md).

## Final report to the human

Return only non-sensitive facts:

- installed runtime version / generation
- `herdr-mcp doctor` summary
- Link status + `HERDR_EDGE_URL` host
- Cloudflare Account (name + shortened ID)
- Worker name and public origin
- MCP URL used for ChatGPT (`/mcp`)
- whether proxy or custom domain was chosen

Never include `HERDR_MCP_TOKEN`, `LINK_SHARED_SECRET`, or Cloudflare API tokens.

## Maintainer UAT (not this page)

Second-machine maintainer validation: [Clean-machine UAT](clean-machine-uat.md) and archived [Second Mac GA UAT Agent prompt](../../history/ga/second-mac-ga-uat-agent-prompt-en.md). Do not give end users the 34-step UAT prompt.
