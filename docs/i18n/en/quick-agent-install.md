# Quick agent install: one user prompt, one complete agent protocol

This is the **recommended end-user onboarding path** for herdr-mcp. It is written for a local coding agent to execute, not for the human to copy commands one by one.

## One prompt to paste into your local coding agent

Give this whole block to Cursor, Codex, Claude Code, Pi, Cline, or another local coding agent that can read URLs and run commands:

```text
Install and configure Herdr and herdr-mcp for me. First read and follow this guide end to end: https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/quick-agent-install.md .

Install the local herdr-mcp runtime from GitHub Releases, not from a git clone. Pause only when I personally need to sign in/create a Cloudflare API Token, or when I need to add the herdr Connector/app in ChatGPT. Automate and verify everything else.
```

The agent must read this page completely before mutating the machine. More detailed Cloudflare credential handling remains in [Agent-assisted installation](agent-install.md).

## What the user gets when this is complete

- Herdr installed and healthy;
- the native stable `herdr-mcp` runtime installed from GitHub Releases;
- a personal Cloudflare Edge Worker (`workers.dev`, or a custom domain only when intentionally selected);
- an outbound workstation Herdr Link;
- ChatGPT Developer mode + a custom `herdr` MCP Connector pointing at `/mcp`;
- an optional Chrome Web Store browser extension after the base Connector works.

## Agent contract

1. Run automatable shell steps directly. Pause only for interactive Cloudflare login/API Token creation, an account choice that cannot be resolved safely, or the human ChatGPT Connector/OAuth step.
2. **Install the local herdr-mcp runtime from GitHub Releases.** Do not `git clone` + `npm`/`cargo` unless the user explicitly asked to develop herdr-mcp itself.
3. Verify after mutations: Herdr health, `herdr-mcp doctor`, Link status, Edge `/health`, public `/mcp`, and final ChatGPT MCP access.
4. Do not echo or persist secrets in repositories, ordinary logs, screenshots, or shell history.
5. If a mutation returns ambiguously, verify actual state before retrying it.

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

1. Download the newest stable `herdr-mcp` binary for this platform from <https://github.com/whshang/herdr-mcp/releases> (`v0.4.2` is the current stable tag at this snapshot; always prefer the newest stable tag).
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

Use the simplest safe path for the first installation:

- no intentional custom-domain requirement → use `workers.dev`;
- user explicitly owns and wants to use a Cloudflare-managed domain → a custom domain may be configured;
- if `workers.dev` connectivity is blocked, first use the Link proxy support described below before expanding DNS scope.

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

If a custom domain is intentionally selected, the OAuth issuer and Connector URL must use the same origin.

## Step 5 — install the Herdr Link

Install the managed Rust Link:

```bash
herdr-mcp link install
herdr-mcp link status
```

If `workers.dev` is blocked on the workstation network, use the existing proxy configuration where possible. Supported priority:

```text
HERDR_LINK_PROXY
HTTPS_PROXY / https_proxy
HTTP_PROXY / http_proxy
ALL_PROXY / all_proxy
```

On macOS the Link can also discover the system proxy via `scutil --proxy`.

Do not turn a connectivity problem into an unnecessary DNS/custom-domain mutation before checking the proxy path.

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

## Step 8 — optional Chrome Web Store browser extension

Only after Step 7 succeeds.

End users install only from the Chrome Web Store. Do **not** substitute a local development build or require a herdr-mcp git checkout.

1. open <https://chromewebstore.google.com/>;
2. search for `Herdr` and choose the official Herdr extension;
3. click **Add to Chrome**;
4. if the Chrome Web Store listing is not live yet, skip this optional step rather than falling back to a local development build.

After the extension is installed, register the local Native Messaging host:

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

Future extension versions are delivered through Chrome's normal Chrome Web Store update mechanism. Normal users do not repeatedly download ZIP files or manually Reload an unpacked extension.

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
