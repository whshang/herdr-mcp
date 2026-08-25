# Installation: from a Herdr workstation to a usable Web AI development environment

This guide installs the complete path, not just a Node process:

```text
ChatGPT
  ↓ OAuth + MCP
Cloudflare Edge
  ↓ authenticated WSS
herdr-link / herdr-mcp
  ↓
Herdr + managed Git projects
```

For the shortest path, start with [Quick Start](quick-start.md). To let a local coding agent perform most setup work, use [Agent-assisted installation](agent-install.md).

## Before installation

### 1. Herdr is already working

herdr-mcp builds on Herdr and does not install or replace it.

```bash
herdr --version
herdr api schema >/dev/null
```

If this fails, fix Herdr first using the [official installation guide](https://herdr.dev/docs/install/).

### 2. Node.js 20+

```bash
node -v
```

### 3. Know which client path you need

- **ChatGPT** requires a stable public Edge and OAuth.
- **Local Cursor / curl** can connect directly to `127.0.0.1`.
- **z.ai / DeepSeek browser bridge** uses the extension and Native Messaging on the local machine.

This guide follows the ChatGPT path because it covers the complete architecture.

## Step 1: clone and build

```bash
git clone https://github.com/whshang/herdr-mcp.git
cd herdr-mcp
npm install
npm run build
mkdir -p ~/.config/herdr-mcp
```

For an existing checkout, inspect Git state before updating. Do not overwrite unknown local changes.

## Step 2: validate the local runtime first

The runtime listens on `127.0.0.1:8772` by default. The static token is for local clients and administration; it is not a ChatGPT credential.

```bash
export HERDR_MCP_TOKEN="$(openssl rand -hex 16)"
export HERDR_MCP_PORT=8772
node dist/server.js
```

In another terminal:

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

`200` or `401` both prove that the HTTP service exists. `401` means it is asking for the local bearer.

The runtime must also reach the Herdr socket. After connecting, `herdr_inspect` should report real workspaces, panes and managed roots.

## Step 3: keep the runtime alive

### macOS

The project provides a LaunchAgent-oriented CLI:

```bash
ln -sf "$PWD/bin/herdr-mcp" ~/.local/bin/herdr-mcp
herdr-mcp start
herdr-mcp status
herdr-mcp logs
```

Optional watchdog:

```bash
herdr-mcp watchdog install
herdr-mcp watchdog status
```

The watchdog checks the herdr-mcp runtime. Transient Herdr control-plane errors do not cause aggressive daemon restarts.

### Linux / Windows

The Node runtime supports these platforms, but the repository currently does not provide an equivalent one-command service manager. Keep `node dist/server.js` alive with your normal service mechanism.

## Step 4: deploy a stable public Edge

ChatGPT cannot reach your loopback address. The recommended design uses Cloudflare Worker / Durable Object as the public endpoint while the workstation creates an outbound authenticated WSS connection.

Use `workers.dev` for the first deployment. It requires no custom domain and keeps DNS out of initial debugging.

### Generate a Worker name

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
printf '%s\n' "$WORKER_NAME"
```

A Worker name is a DNS label. A hostname such as `MacBook.local` should not be copied verbatim; the helper normalizes it.

### Prepare Wrangler configuration

```bash
cp edge/cloudflare/wrangler.user.example.toml edge/cloudflare/wrangler.user.toml
```

Fill in the template fields for:

- Worker name;
- workstation identity;
- `OAUTH_ISSUER` / public origin;
- other deployment values required by the template.

Deploy:

```bash
cd edge/cloudflare
npx wrangler deploy --config wrangler.user.toml
```

The public origin looks like:

```text
https://herdr-edge-xxx.<account-subdomain>.workers.dev
```

The MCP endpoint is:

```text
https://herdr-edge-xxx.<account-subdomain>.workers.dev/mcp
```

See [Cloudflare Edge Token](cloudflare-edge-token.md) for least-privilege credential handling and [Cloudflare Edge deployment](cloudflare-edge-deployment.md) for Worker / Durable Object / Link details.

## Step 5: verify the workstation link

A deployed Worker only proves that the Edge exists. The Edge must also be able to route to your workstation.

Check that:

1. the local runtime is healthy;
2. `herdr-link` is running;
3. workstation identity matches the Edge configuration;
4. Edge `/health` reports the workstation online;
5. runtime generation/version is expected.

OAuth may succeed even while the workstation is offline, so treat public Edge health and workstation reachability as separate layers.

## Step 6: create the ChatGPT Connector

In ChatGPT:

1. enable Developer mode;
2. create a custom MCP Connector;
3. enter `https://<worker>.<account>.workers.dev/mcp`;
4. complete OAuth in the browser;
5. create a **new conversation** for validation.

Do not paste `HERDR_MCP_TOKEN` into ChatGPT. The public Connector authentication boundary is OAuth.

ChatGPT caches tool snapshots. See [ChatGPT Connector](chatgpt-connector.md) for why an old conversation can continue to expose an old contract after the server has been upgraded.

## Step 7: perform a real validation

Start with a safe read-only request:

```text
Inspect the current Herdr workspaces and project state. Read only; do not modify anything.
```

Expected behavior:

1. `herdr_inspect` runs successfully;
2. the model sees real workspaces / managed Git roots;
3. it may load `herdr_skill` once;
4. it can use `herdr_git` or `herdr_fs_read` on real project state.

Then test a small reversible edit and a test command.

The current production public contract is **epoch 2 / 18 tools**. If a new conversation still exposes 17 tools, investigate Connector/tool snapshot caching before reinstalling the runtime.

## Step 8: install the browser extension when continuity matters

MCP solves "ChatGPT reaches the workstation." If you also want the browser to resume when local agents complete, recover stalled replies, or hand off very long conversations, install the browser extension.

See [Browser extension](extension.md). The extension uses Native Messaging and local IPC; it does not store the Herdr bearer in browser state.

## When to add a Custom Domain

`workers.dev` is enough to validate the complete Connector path. A Custom Domain is useful when:

- you want a long-lived OAuth issuer under a domain you control;
- your team has central domain governance;
- you want to decouple the public identity from the Cloudflare account subdomain.

Validate the complete flow on `workers.dev` first, then migrate the stable origin separately.

## Local clients: bypass Cloudflare

Local Cursor / curl can connect directly to:

```text
http://127.0.0.1:8772/mcp
```

using the local static bearer. This path is also useful for separating runtime failures from Edge failures.

## What "installed" means

A complete ChatGPT installation satisfies all of these:

- Herdr socket works;
- herdr-mcp runtime works;
- Edge is deployed;
- workstation link is online;
- OAuth succeeds;
- a new ChatGPT conversation receives the epoch-2 catalog;
- `herdr_inspect` sees the real workstation;
- one real file/Git/test operation works.

A successful Worker deployment or a Connector marked "connected" is only one layer of that validation.

Use [Troubleshooting](troubleshooting.md) to diagnose failures from local runtime → Link → Edge → OAuth → MCP → ChatGPT snapshot.
