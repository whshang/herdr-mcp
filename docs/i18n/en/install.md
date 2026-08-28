# Installation: from one Herdr workstation to usable Web AI development

> **Recommended for normal users:** do not copy commands from this page one by one. Give the one-line prompt in [Quick agent install](quick-agent-install.md) to Cursor / Codex / Claude Code or another local coding agent. It installs Herdr, herdr-mcp, Edge, and Link, and pauses only for Cloudflare and ChatGPT steps that require the human. This page remains the manual-install and troubleshooting reference.

The goal is to connect a local workstation to ChatGPT / Web AI while keeping source code and real execution on the workstation.

## Before installation

### 1. Herdr must be available

```bash
herdr --version
herdr api schema >/dev/null
```

If Herdr is missing, use the official stable installer.

macOS / Linux:

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"
```

Then run `herdr --version` again. Herdr's own install behavior is authoritative at <https://herdr.dev/docs/install/>.

### 2. Decide which client path you need

- ChatGPT / another public Web AI → Cloudflare Edge + outbound Herdr Link;
- local MCP clients only → loopback runtime can be used without Cloudflare;
- browser extension → optional after the base Connector works, not a first-install prerequisite.

## Supported platform boundary

Current stable runtime is **`v0.4.1`**. The strongest clean-machine qualification evidence remains the `v0.4.0` **macOS Apple Silicon** run. A Windows x64 release binary is available, while Windows end-to-end UAT is still being completed. Linux is not yet claimed as a supported current-stable herdr-mcp runtime surface.

## Step 1: install the native herdr-mcp runtime

Download the newest stable `herdr-mcp` binary for this platform from <https://github.com/whshang/herdr-mcp/releases>, put it on `PATH`, then run:

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

`install` stages an immutable generation under `~/.config/herdr-mcp/runtime/` and points the user PATH entry at `runtime/current/herdr-mcp`. Normal users do not install the local runtime with a git clone, `npm`, or `cargo`.

## Step 2: make the local runtime healthy first

At minimum verify:

```bash
herdr-mcp doctor
herdr-mcp status
```

Do not add public Edge while the local doctor is unhealthy. Fix the local runtime / Herdr layer first.

## Step 3: deploy the public Edge

When ChatGPT needs to reach the workstation over the Internet, use a Cloudflare Worker as the stable OAuth/MCP entry point. Prefer `workers.dev` for the first setup unless a custom domain is an explicit requirement.

The recommended route is to let a coding agent follow [Quick agent install](quick-agent-install.md), because that protocol owns Token scoping, Worker naming, secret injection, account choice, and proxy handling.

For a manual deployment, keep these constraints:

- Cloudflare API Token is an ephemeral process value, not a repository or log value;
- default to `workers_dev = true` and `routes = []`;
- derive the Worker name using the repository helper:

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

- keep `LINK_SHARED_SECRET` as a Worker secret;
- the workstation makes outbound authenticated WSS and does not expose a public local port.

See [Agent-assisted installation](agent-install.md) and [Cloudflare Edge deployment](cloudflare-edge-deployment.md) for the detailed manual contract.

## Step 4: install and verify the Herdr Link

```bash
herdr-mcp link install
herdr-mcp link status
```

If `workers.dev` is unreachable from the workstation network, prefer existing proxy configuration:

```text
HERDR_LINK_PROXY
HTTPS_PROXY / https_proxy
HTTP_PROXY / http_proxy
ALL_PROXY / all_proxy
```

On macOS, Link can also read `scutil --proxy`. Do not expand a connectivity problem into DNS / custom-domain changes before checking the proxy path.

## Step 5: verify the public path

```bash
herdr-mcp doctor
herdr-mcp link status
curl -fsS "${EDGE_ORIGIN}/health"
curl -s -o /dev/null -w '%{http_code}\n' "${EDGE_ORIGIN}/mcp"
```

An unauthenticated `/mcp` response of `401` can be correct. The useful checks are: local runtime healthy, Link connected, Edge `/health` reachable, and OAuth metadata reachable.

## Step 6: add the herdr Connector in ChatGPT

This is a human step. The coding agent should pause and guide the user:

1. open ChatGPT settings → Apps / Connectors;
2. enable Developer mode when the current UI requires it;
3. add a custom MCP Connector named `herdr`;
4. enter the deployed `${MCP_URL}` ending in `/mcp`;
5. complete OAuth in the browser;
6. enable the Connector in a new conversation or Project.

Then do a read-only test:

```text
Inspect my Herdr projects. Read only; do not modify anything.
```

If `herdr_inspect` returns real workstation data, the base loop is usable.

See [ChatGPT Connector](chatgpt-connector.md).

## Step 7: add the browser extension only when continuity is needed

The browser extension adds Side Panel Control Center, workspace binding, long-conversation continuity, and queued next-turn messages. It is not required for the base MCP loop.

End users install only from the **Chrome Web Store**:

1. open <https://chromewebstore.google.com/>;
2. search for `Herdr` and choose the official Herdr extension;
3. click **Add to Chrome**;
4. if the Chrome Web Store listing is not live yet, skip the extension rather than falling back to a developer-mode install;
5. after installation run:

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

Future extension versions are delivered through Chrome's normal Web Store update mechanism. Normal users do not repeatedly download ZIPs, overwrite local extension directories, or manually Reload the extension.

See [Browser extension](extension.md) and [Browser Control Center](browser-control-center.md).

## What “installed” means

At minimum:

- `herdr --version` works;
- `herdr-mcp doctor` is healthy;
- Herdr Link is connected;
- Edge `/health` is reachable;
- ChatGPT OAuth is complete;
- a new conversation can call `herdr_inspect` against the real workstation;
- if the optional extension is installed, `herdr-mcp native-host status` is healthy and Control Center can see workspaces.

## Beyond manual installation

If the goal is simply to get working quickly, return to [Quick agent install](quick-agent-install.md) and let the coding agent execute the protocol.

Read deeper only when needed:

- [Troubleshooting](troubleshooting.md)
- [Architecture](architecture.md)
- [Runtime A/B](runtime-self-upgrade.md)
- [Cloudflare Edge deployment](cloudflare-edge-deployment.md)

Maintainer UAT, GA gates, and release evidence are intentionally outside the normal user installation flow.
