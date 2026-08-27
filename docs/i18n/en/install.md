# Installation: from a Herdr workstation to a usable Web AI development environment

This guide installs the complete path, not just a local process:

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

### 2. Know which client path you need

- **ChatGPT** requires a stable public Edge and OAuth.
- **Local Cursor / curl** can connect directly to `127.0.0.1`.
- **z.ai / DeepSeek browser bridge** uses the extension and Native Messaging on the local machine.

This guide follows the ChatGPT path because it covers the complete architecture.

Node.js is **not** required to run the local MCP runtime. You may still need Node temporarily when deploying the Cloudflare Worker (`npx wrangler`). Building the browser extension from source is an advanced/developer path, not the primary install.

## Supported platforms (first GA recommendation)

- **Officially supported for first GA:** macOS Apple Silicon (managed install / service / update / rollback).
- **Preview artifact:** Windows x64 may still be published as a Release asset; do not claim full managed lifecycle parity until G19 seals it.
- **Not claimed for first GA:** Linux service lifecycle. Presence of a Linux binary in CI matrices does not make Linux a GA platform.

## Step 1: install the native runtime (primary)

Download the current `herdr-mcp` binary for your platform from [GitHub Releases](https://github.com/whshang/herdr-mcp/releases), place it on your `PATH` (for example `~/.local/bin/herdr-mcp`), and make it executable. Then run `herdr-mcp install`: the installer stages an immutable generation under `~/.config/herdr-mcp/runtime/` and retargets `~/.local/bin/herdr-mcp` to `runtime/current/herdr-mcp` so the PATH entry no longer depends on a git checkout.

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

Prefer these top-level commands. Do **not** use `herdr-mcp service install` as the normal user install instruction; `service ...` remains advanced/internal. Do **not** clone this repository or run `npm`/`cargo` to install the local MCP runtime.

While the product is still alpha, keep `update.channel = "preview"` (or leave config absent on an alpha binary) so discovery sees prerelease tags. The `stable` channel discovers non-prerelease releases only.

## Step 2: validate the local runtime first

The managed runtime listens on `127.0.0.1:8772` by default. After the binary is installed and the local service is healthy:

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

`200` or `401` both prove that the HTTP service exists. `401` means it is asking for the local bearer.

The runtime must also reach the Herdr socket. After connecting, `herdr_inspect` should report real workspaces, panes and managed roots.

Day-to-day upgrades:

```bash
herdr-mcp update apply
herdr-mcp update status
```

## Step 3: keep the runtime alive

On macOS, the native binary owns the managed LaunchAgent lifecycle once installed. Use `herdr-mcp status` / `herdr-mcp doctor` for health, and `herdr-mcp update ...` for upgrades. Avoid pointing launchd at a git checkout or `target/*/herdr-mcp`.

Linux / Windows service packaging is narrower today. First-GA recommendation: officially support macOS Apple Silicon only; treat a published Windows binary as preview until G19 seals it. Do not advertise an unsupported Linux lifecycle. Keep the release binary on `PATH` and follow platform-specific notes in the current Release assets.

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

## Appendix: developer from source

Clone + `npm`/`cargo` workflows remain available for people developing herdr-mcp itself. That contributor path is not the primary end-user install path and must not be required to run the local MCP runtime.

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
