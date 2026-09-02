# Manual install

*From one Herdr workstation to usable Web AI development.*

> **Role: manual/operator reference.** The primary herdr-mcp installation protocol is written directly for the executing Agent; see [Agent install](agent-install.md) and [Agent installation](agent-install.md). Use this page for manual inspection, troubleshooting, or understanding each stage. It no longer provides a product-specific prompt to copy into a named coding agent.

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

The current stable runtime is the GitHub `Latest` stable Release at <https://github.com/whshang/herdr-mcp/releases>. The stable macOS TCC broker has completed cross-generation authorization verification; Developer ID signing is optional hardening. The v0.4.3+ install path preserves the same broker compatibility revision and ensures the fixed broker exists before the rotating runtime service starts. An interactive first install opens **Full Disk Access** for that broker; macOS still requires the user to grant the permission explicitly. `herdr-mcp permissions setup` reopens the same panel and `herdr-mcp permissions verify` checks access. Runtime generation updates never rewrite a same-revision broker. The strongest clean-machine qualification evidence remains the `v0.4.0` **macOS Apple Silicon** run. A Windows x64 release binary is available, while Windows end-to-end UAT is still being completed. Linux is not yet claimed as a supported current-stable herdr-mcp runtime surface.

On macOS, v0.4.3 also separates production device credentials from rotating runtime code. Keychain reads/writes go through the fixed `~/.config/herdr-mcp/herdr-mcp-credential-helper`. An existing installation may require one explicit macOS Keychain approval when this helper is introduced; that preflight happens before service/Link mutation and fails safely if the prompt is ignored or denied. Ordinary runtime updates preserve the same helper compatibility revision, so a new `runtime/generations/rust-*` binary does not become a new Keychain client on every upgrade. This credential helper is independent from the Full Disk Access/TCC broker above.

If this machine is still running **v0.4.2**, upgrade once with `herdr-mcp update apply`. The v0.4.2 binary intentionally treats bare `herdr-mcp update` as a read-only check and prints that same `next_action`. From v0.4.3 onward, bare `herdr-mcp update` performs the upgrade directly. Pairing and the ChatGPT Connector do not need to be recreated for either path.

## Step 1: install the native herdr-mcp runtime

Download the newest stable `herdr-mcp` binary for this platform from <https://github.com/whshang/herdr-mcp/releases>, put it on `PATH`, then run:

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

`install` stages an immutable generation under `~/.config/herdr-mcp/runtime/` and points the user PATH entry at `runtime/current/herdr-mcp`. Normal users do not install the local runtime with a git clone, `npm`, or `cargo`.

On macOS the service remains a normal user LaunchAgent; `sudo` is not required and does not grant TCC access. The permission target is the stable `~/.config/herdr-mcp/tcc-broker/herdr-mcp-broker`, not a rotating `runtime/generations/rust-*` binary. A non-interactive install prepares the broker but does not open System Settings; run `herdr-mcp permissions setup` once at a user terminal if Full Disk Access is not already granted.

## Step 2: make the local runtime healthy first

At minimum verify:

```bash
herdr-mcp doctor
herdr-mcp status
```

Do not add public Edge while the local doctor is unhealthy. Fix the local runtime / Herdr layer first.

## Step 3: deploy the public Edge

When ChatGPT needs to reach the workstation over the Internet, use a Cloudflare Worker as the stable OAuth/MCP entry point. Prefer `workers.dev` for the first setup unless a custom domain is an explicit requirement.

For automated installation, the executing Agent follows [Agent install](agent-install.md) / [Agent installation](agent-install.md) directly; those protocols own Token scoping, Worker naming, secret injection, account choice, and the boundary for network blockers.

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

The extension has three identities: **STORE / STANDALONE / DEV**. The v0.4.2 Native Host supports Store/DEV ownership; v0.4.3+ adds the fixed-identity STANDALONE path for GitHub/manual distribution.

- STORE: default ordinary-user path, fixed Chrome Web Store identity and Store updates;
- STANDALONE: v0.4.3+, fixed non-Store identity for independent/GitHub distribution;
- DEV: source development only, Load unpacked from repo/worktree `extension/`, with a path-derived identity.

After installing/selecting a supported channel, run:

```bash
herdr-mcp native-host status
```

Require the active channel, extension identity, and Native Host runtime generation to match the intended installation. Do not use DEV as the ordinary-user fallback, and do not call a GitHub/manual fixed-identity package "dev".

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

## Automated execution entry point

For automated installation, the Agent reads [Agent install](agent-install.md) directly. Use [Agent installation](agent-install.md) when the full permissions, security, and failure-boundary contract is needed.

Read deeper only when needed:

- [Troubleshooting](troubleshooting.md)
- [Architecture](architecture.md)
- [Runtime A/B](runtime-self-upgrade.md)
- [Cloudflare Edge deployment](cloudflare-edge-deployment.md)

Maintainer UAT, GA gates, and release evidence are intentionally outside the normal user installation flow.

## Repair, reinstall, and uninstall

On v0.4.3+, use the product-level lifecycle commands rather than manually deleting launchd files or runtime directories:

```bash
herdr-mcp reinstall
herdr-mcp uninstall
```

`reinstall` repairs/replaces the managed Rust runtime while retaining configuration and credentials; generations follow normal service GC, preserving the active/rollback-safe set. `uninstall` removes strongly owned local herdr-mcp runtime/config state. The default instance covers its service, owned daily auto-update scheduler, Link/watchdogs, Native Messaging host, managed CLI link, and config root; a named instance removes only its own service/watchdogs/config. Product uninstall arms a tiny durable update-fence tombstone under the user cache outside the config root before teardown, so a queued silent updater cannot resurrect the removed service after the config directory is gone. That tombstone is cleared only by an explicit successful install/reinstall. It intentionally preserves Herdr itself (`herdr`, the Herdr service/socket/config), as well as separately managed browser/Cloudflare/Keychain/TCC authorization state. Run these lifecycle mutations from an independent terminal, not from a managed `herdr_exec` session that depends on the service being changed.
