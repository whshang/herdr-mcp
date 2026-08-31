# Quick Agent installation protocol

> **Executor: Agent.** This page is not a command list for a human to copy and is not a prompt template that needs to be wrapped for a named coding product. When assigned a herdr-mcp installation task, the Agent reads and executes this protocol directly.

This is the concise ordinary-workstation installation protocol. The full Cloudflare permission, secret-handling, Link, and failure-boundary contract lives in [Agent installation](agent-install.md). Maintainer UAT is outside this protocol.

## Execution constraints

1. Read this page before mutation; execute deterministic shell, file, and state checks directly.
2. Pause only for genuinely interactive human actions: Cloudflare sign-in/API Token creation, an account choice that cannot be inferred safely, and ChatGPT Connector/OAuth authorization.
3. Install the ordinary workstation PROD runtime from published GitHub Releases. Unless the task explicitly concerns herdr-mcp development, do not substitute a git checkout, `target/*`, or a local build.
4. If network, login state, or third-party availability blocks the requested path, stop and report the blocker. Do not build a proxy, switch network nodes, rewrite system proxy settings, or invent a bypass.
5. Verify actual state after important mutations. If delivery is uncertain, inspect state before retrying; never blind-retry a mutation.
6. Never put secrets in the repository, ordinary logs, screenshots, shell history, or the final report.

## What the user gets when this is complete

- Herdr installed and healthy;
- the native stable `herdr-mcp` runtime installed from GitHub Releases;
- a personal Cloudflare Edge Worker (`workers.dev`, or a custom domain only when intentionally selected);
- an outbound workstation Herdr Link;
- ChatGPT Developer mode + a custom `herdr` MCP Connector pointing at `/mcp`;
- an optional browser extension using a supported STORE / STANDALONE / DEV channel after the base Connector works.

## Prerequisite — install Herdr if it is missing

Check first:

```bash
herdr --version
herdr api schema >/dev/null
```

If Herdr is missing, install the official stable build instead of sending the human away to research another guide.

macOS / Linux:

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"
```

Then re-run:

```bash
herdr --version
herdr api schema >/dev/null
```

Pause only if Herdr still does not become healthy. herdr-mcp does not replace Herdr, but this onboarding protocol installs it when needed.

**Current herdr-mcp support boundary:** the most complete clean-machine evidence is on macOS Apple Silicon. Windows x64 has a release binary but Windows end-to-end UAT is still being completed. Linux is not yet claimed as a supported current-stable herdr-mcp runtime product surface.

Node.js is only a temporary Edge deployment dependency through Wrangler; it is not a local herdr-mcp runtime dependency.

## Step 1 — install the native herdr-mcp runtime

1. Download the newest stable `herdr-mcp` binary for this platform from <https://github.com/whshang/herdr-mcp/releases> (`v0.4.2` is the current published stable tag at this snapshot; always prefer the newest published stable tag).
2. Put it on `PATH` (for example `~/.local/bin/herdr-mcp`) and make it executable when the platform requires it.
3. Run:

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

The normal user path must not depend on a git checkout.

## Step 2 — choose the public Edge URL strategy

Choose one canonical public origin during the first installation and keep it for OAuth, MCP, and Link WSS:

- `workers.dev` is the zero-DNS bootstrap path when it is reachable from the workstation network;
- use a Custom Domain from the start only when the user has already selected that path or an existing installation policy/configuration makes that intent explicit;
- preserve an already-configured workstation proxy path when it is part of the user's environment; do not create or change proxy/network settings as an automatic workaround.

If a connectivity probe fails while choosing or verifying the origin, stop and ask the user before changing this decision.

Example Connector URLs:

```text
https://herdr-edge-device.username.workers.dev/mcp
https://herdr-mcp.example.com/mcp
```

## Step 3 — Cloudflare Token pause (human action)

Open <https://dash.cloudflare.com/profile/api-tokens> and guide the human to create a token using the current **Edit Cloudflare Workers** template, limited to the intended account. The default `workers.dev` bootstrap does not need DNS Write.

Use the token only as an ephemeral process environment value. Never echo it. Never commit it. Never put it in a normal `.env` file or shell history.

If more than one Cloudflare account is available and the intended account cannot be inferred safely, ask the human which account to use.

## Step 4 — deploy Edge

Use a temporary Edge source checkout only for deployment; it must never become the installed runtime path.

The canonical Worker name is produced by the repository helper:

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

Use a user Wrangler config with:

```toml
workers_dev = true
routes = []
```

Deploy and configure the Link secret using the detailed commands in [Agent-assisted installation](agent-install.md). The expected public shape is:

```text
EDGE_ORIGIN=https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev
HERDR_EDGE_URL=wss://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev/ws
MCP_URL=<EDGE_ORIGIN>/mcp
```

If a custom domain is intentionally selected, the OAuth issuer and Connector URL must use the same origin. Persist the selected origin before installing/reloading Link so every generated LaunchAgent derives the same WSS endpoint:

```bash
herdr-mcp config set-edge-origin "$EDGE_ORIGIN"
```

## Step 5 — install the Herdr Link

Install the managed Rust Link:

```bash
herdr-mcp link install
herdr-mcp link status
```

The Link can recognize an already-configured proxy path with this priority:

```text
HERDR_LINK_PROXY
HTTPS_PROXY / https_proxy
HTTP_PROXY / http_proxy
ALL_PROXY / all_proxy
```

On macOS the Link can also discover the existing system proxy via `scutil --proxy`. This is observation/reuse, not permission to mutate network settings.

If the selected Edge origin is still unreachable, **stop and ask the user**. Do not set a new proxy, change the system proxy, switch network nodes, or move to a custom domain without explicit user direction.

## Step 6 — verify the local and public path

Run:

```bash
herdr-mcp doctor
herdr-mcp link status
curl -fsS "${EDGE_ORIGIN}/health"
curl -s -o /dev/null -w '%{http_code}\n' "${EDGE_ORIGIN}/mcp"
```

Expected behavior:

- runtime/service/Native Messaging ownership checks are healthy for the installed surface;
- Link is connected;
- `/health` is reachable;
- unauthenticated `/mcp` may correctly return `401`;
- Cloudflare bootstrap credentials are removed from the environment after deployment.

## Step 7 — add the herdr Connector in ChatGPT (human action)

Pause and guide the human through ChatGPT:

1. open ChatGPT settings / Connectors or Apps;
2. enable Developer mode when required by the current ChatGPT UI;
3. add a custom MCP Connector named `herdr`;
4. enter the deployed `${MCP_URL}` ending in `/mcp`;
5. complete OAuth in the browser;
6. enable the Connector in a new conversation or Project.

Then test with:

```text
Inspect my Herdr projects. Read only; do not modify anything.
```

Success means the public tool list is available and `herdr_inspect` returns the real workstation.

See [ChatGPT Connector](chatgpt-connector.md) for UI detail.

## Step 8 — choose the optional browser-extension channel

Only after Step 7 succeeds. Extension channels are separate from the Runtime DEV/PROD model:

- **STORE** — default ordinary-user path; fixed Chrome Web Store identity and Store updates.
- **STANDALONE** — v0.4.3+ GitHub/manual path; fixed non-Store identity, independent of the unpacked directory path.
- **DEV** — source-development path only; Load unpacked from a repo/worktree `extension/` directory with a path-derived identity.

First inspect what the installed runtime actually supports. Current stable v0.4.2 has Store/DEV Native Host ownership; do not pretend it supports standalone. On v0.4.3+ when `native-host use standalone` is explicitly available, choose in this order:

1. no user preference and Store is available → **STORE**;
2. Store is unavailable or the user explicitly requests GitHub/independent distribution, and the runtime supports it → **STANDALONE**;
3. the task explicitly concerns extension/source development → **DEV**.

Use only the official Store build for STORE, a fixed-identity release package for STANDALONE, and never use DEV as an ordinary-user fallback. After installing/selecting the chosen channel, synchronize Native Messaging and run:

```bash
herdr-mcp native-host status
```

The status must identify the expected active channel/extension identity and show the Native Host runtime consistent with the active runtime generation.

See [Browser extension](extension.md) and [Browser continuity](browser-continuity.md).

## Final report to the human

Return only non-sensitive facts:

- installed Herdr version;
- installed herdr-mcp version / generation;
- `herdr-mcp doctor` summary;
- Link state and Edge hostname;
- Cloudflare account name plus shortened ID when useful;
- Worker name and public `/mcp` URL;
- whether a proxy or intentional custom domain was used;
- whether the optional Chrome Web Store extension was installed or skipped.

Never include `HERDR_MCP_TOKEN`, `LINK_SHARED_SECRET`, or the Cloudflare API Token.

## Maintainer UAT is not this page

Clean-machine release qualification belongs in [Clean-machine UAT](clean-machine-uat.md) and archived GA evidence. Do not give those maintainer scripts to a normal end user.
