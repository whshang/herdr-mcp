# herdr-mcp

**Turn the reasoning capacity you already have in ChatGPT / Web AI into a persistent local development environment.**

Web AI subscriptions can provide substantially more usable reasoning capacity than many API or local-agent workflows, and MCP gives those browser models a standard way to call software on your machines. Herdr-MCP starts from that opportunity instead of introducing another model subscription.

**MCP gives Web AI hands. Herdr gives those hands a persistent workplace. The optional browser extension closes the loop.**

ChatGPT can inspect and modify code, use Git, run commands and tests, or delegate long-running work to local coding agents. Herdr keeps workspaces, terminals, processes and agents alive independently of any single chat, so parallel work and conversation handoff do not require rebuilding the local worksite.

**Docs:** https://whshang.github.io/herdr-mcp/ · **Source:** https://github.com/whshang/herdr-mcp

Languages: **English** · [简体中文](README.zh.md) · [日本語](README.ja.md)

## Agent-first setup

The canonical installation path is an **execution protocol written directly for an Agent**:

- [Agent install](docs/i18n/en/agent-install.md) — authoritative end-to-end Agent execution contract.

An execution-capable Agent should read the protocol itself, perform deterministic checks and mutations directly, and pause only for genuinely interactive human authorization or choices. Ordinary workstation PROD installs use published GitHub Releases for herdr-mcp, not a repo checkout. If network/login/third-party availability blocks the requested path, the Agent should stop and report the blocker rather than inventing proxy or bypass infrastructure.

The local herdr-mcp runtime is a native binary; normal users do **not** need Node.js or npm to run it. Node may be used temporarily by the Agent only for Cloudflare/Wrangler bootstrap.

The protocol covers Herdr installation, stable herdr-mcp installation, Cloudflare Edge, the workstation Link, ChatGPT Connector/OAuth, `herdr-mcp doctor`, and a real MCP smoke test.

For operator/manual reference, see [Installation](docs/i18n/en/install.md).

## Add a new computer to an existing Worker (v0.4.3+)

This path is **v0.4.3+ only**. Current stable is still **v0.4.2**, so the commands below are not yet available on a stock install. The Agent must **fail closed**: if the installed CLI does not expose `herdr-mcp worker pair` / `herdr-mcp worker connect`, stop and report the version/capability blocker instead of improvising.

This is **not** a fresh Worker deployment. If you say “connect this new computer to my existing Worker”, the Agent must **not** create a new Cloudflare Worker, Durable Object namespace, OAuth app/client, Connector, or copy the legacy global `LINK_SHARED_SECRET`. It joins the Worker you already have.

### On the already-authorized existing macOS computer

Start a short-lived pairing session (default 10 minutes):

```bash
herdr-mcp worker pair
```

This prints a **pairing address** and a **6-digit verification code** (formatted `123 456`). The code is the intended short-lived pairing credential; it expires in 10 minutes and is single-use.

### On the new computer — paste this prompt into a Coding Agent

```text
Read and follow https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/en/existing-worker-connect.md to connect this computer to my existing Herdr Worker. Pairing address: <pairing-address>  Verification code: <code>
```

Replace `<pairing-address>` and `<code>` with the values printed by `herdr-mcp worker pair`. The canonical document owns all version/capability checks, install-from-Release rules, the macOS-only boundary, secret handling, verification, and recovery.

After both devices are paired, the same existing ChatGPT Connector/Worker should see both devices through the multi-device public surface. This is expected v0.4.3 behavior pending release/UAT; formal two-device GA/UAT has not yet passed.

## First real test

In a new ChatGPT conversation with the `herdr` Connector enabled, send:

```text
Inspect my Herdr projects. Read only; do not modify anything.
```

A healthy setup lets ChatGPT see real workspaces, panes, agents, Git state, and project files through the MCP tools.

On v0.4.3+, `workstation_offline` is a Link/Edge reachability condition, not a browser-extension error. Edge absorbs short reconnects before surfacing the error; if it still must return one, the MCP result carries explicit retry/delivery metadata, while the workstation Link keeps local reconnect/backoff and prolonged-offline recycle. See [Troubleshooting](docs/i18n/en/troubleshooting.md) for the exact mutation replay rules.

### Artifacts and visual work in v0.4.2

The v0.4.2 runtime extends the same workstation boundary to visual and file-import work. `herdr_fs_image` lets ChatGPT inspect PNG/JPEG/GIF/WebP assets directly from managed projects, and the built-in planner policy routes artifacts over the shortest safe path: managed local files go through the direct `herdr_fs_*` tools, safe signed HTTPS URLs are imported directly with `herdr-mcp artifact import --signed-url`, directly consumable MCP/Connector file references are consumed directly, and only the remaining cross-boundary transfers use a private, short-lived Cloudflare R2 generic artifact relay. The Rust runtime imports with HTTPS/SSRF, size, MIME/signature, digest, managed-root, dirty-file, and busy-agent checks before anything is written to the repository. The public MCP catalog stays at 18 tools.

The browser extension is **not a generic file relay**. In v0.4.2 it has one narrow authenticated source-capture role for images generated in the current ChatGPT Web conversation: ChatGPT cookies and the short-lived bearer stay in browser memory, the extension resolves the conversation's `image_asset_pointer`/`file_id`, fetches the image only from the allowlisted HTTPS `chatgpt.com` download endpoint, and sends only validated image bytes plus non-secret metadata to the local Native Host. Cookies, bearer tokens, Authorization headers, and download URLs never cross that boundary. OpenCLI and Ego Browser were UAT/research aids only, not product dependencies; all other artifact routing remains runtime + direct import + the private R2 fallback.

## Browser extension: optional, after the Connector works

The browser extension adds long-conversation continuity, the Side Panel Control Center, workspace binding, and queued next-turn messages. It is **not** required for the first ChatGPT-to-workstation connection.

In v0.4.2, a user who manually opens another conversation inside the same bound ChatGPT Project can simply say **“continue”** or **“resume”** without supplying an internal continuity ID. Herdr first searches the durable Continuity Journal with stable conversation / Project / workspace identity. It auto-resumes only when that identity identifies exactly one active chain; otherwise it shows bounded candidate evidence and asks the user to confirm. Recency or text similarity alone never selects a chain. See [Browser continuity](docs/i18n/en/browser-continuity.md).

The browser-extension distribution model is intentionally separate from the Runtime DEV/PROD model:

| Extension channel | Intended use | Identity/update source |
| --- | --- | --- |
| **STORE** | default ordinary-user install | fixed Chrome Web Store identity + Store updates |
| **STANDALONE** | GitHub/manual install without Store dependency | fixed non-Store identity + deterministic package; first-class in v0.4.3+ |
| **DEV** | extension/source development only | unpacked repo/worktree path; path-derived identity |

Current stable v0.4.2 has Store/DEV Native Host ownership. v0.4.3 adds the fixed-identity **STANDALONE** path; v0.4.2 is not repacked or retagged to retrofit it. An Agent must not call a GitHub/manual standalone package "dev", and must not use a path-derived DEV build as the fallback for an ordinary install.

For the current stable Store path: finish runtime + Connector verification, install the [Herdr Chrome Web Store item](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp) when available, then run `herdr-mcp native-host install` and `herdr-mcp native-host status`. A v0.4.3+ Agent may instead select STANDALONE when the runtime explicitly advertises that channel and Store installation is unavailable or the user requests independent distribution.

See [Browser extension](docs/i18n/en/extension.md) and [Browser Control Center](docs/i18n/en/browser-control-center.md).

## Current support boundary

- stable herdr-mcp: `v0.4.2`;
- public MCP contract: epoch 2 / 18 tools;
- strongest clean-machine evidence: macOS Apple Silicon;
- Windows x64 release binary is available, while Windows end-to-end UAT is still being completed;
- Linux runtime is not claimed as a supported current-stable product surface yet.

## Local runtime CLI

Most users can let their coding agent manage this lifecycle. The stable top-level user commands are:

```bash
herdr-mcp install
herdr-mcp status
herdr-mcp doctor
herdr-mcp update check
herdr-mcp update apply
herdr-mcp update auto
herdr-mcp update status
herdr-mcp rollback
herdr-mcp reinstall
herdr-mcp uninstall
```

On macOS v0.4.3+, the default production instance installs `dev.herdr-mcp.auto-update`, a background launchd trigger that runs on load and then once every 86,400 seconds. `update auto` reaches GitHub only when the **compiled runtime channel is `prod`**, `[update] check = true`, and the release channel is `stable`; named instances, DEV runtimes, and `preview` all skip before any network request. A strictly newer Stable Release is queued through the same SHA-256 + GitHub Sigstore/SLSA verified, detached, rollback-safe updater used by `update apply`. `service uninstall` first arms a durable update fence and removes the owned scheduler so an already-running detached worker cannot resurrect the service; an explicit successful `install`/`reinstall` clears that fence. Set `[update] check = false` to disable the network check.

`herdr-mcp reinstall` is the repair/replacement path: it re-applies the managed Rust service lifecycle while preserving configuration and credentials; runtime generations follow the normal service GC policy, which retains the active/rollback-safe set rather than promising every historical generation. `herdr-mcp uninstall` is the product-level local runtime/config cleanup path: the default instance removes only strongly-owned herdr-mcp service, auto-update scheduler, Link/watchdog, Native Messaging host, user CLI and config state, while a named instance removes only its own service/watchdogs/config. Before deleting the config root it leaves one tiny update-fence tombstone in the user cache; detached workers must honor that tombstone, and only a later explicit successful install/reinstall clears it. Both deliberately preserve the independent `herdr` executable/service/socket/config and separately managed Keychain/TCC/browser/Cloudflare authorization state. `herdr-mcp service uninstall` remains the narrower advanced service primitive.

For **herdr-mcp source development** on v0.4.3+, the runtime plane is explicitly split into DEV and PROD:

```bash
herdr-mcp dev status
herdr-mcp dev sync
herdr-mcp dev rollback
```

`dev sync` is the deliberate dogfood path: it builds the current clean checkout as `0.4.3-dev`, embeds source commit/dirty provenance, pins the existing PROD binary and SHA-256 recovery source, then reuses the transactional service lifecycle so the server, Native Host and `dev.herdr-mcp.link-prod` converge on one managed DEV generation. `dev status` is read-only. `dev rollback` returns to the pinned PROD binary; repeated DEV syncs do not redefine PROD as the previous DEV generation. `dev sync --dry-run` previews the transaction without mutating runtime state, and dirty source is refused unless `--allow-dirty` is explicit.

These DEV commands are for maintainers/source developers, not an alternate ordinary-user install path. Runtime **DEV / PROD** is also separate from browser-extension **DEV / STANDALONE / STORE** identity.

`herdr-mcp service ...` is an **advanced / internal** service-control surface, not the normal install path. On `0.4.1+`, `herdr-mcp scan --json` refreshes the evidence-backed inventory of locally startable Herdr agent kinds; see the [CLI reference](docs/i18n/en/cli-reference.md#capability-discovery-scan) for details.

## Read more only when you need it

- [Installation](docs/i18n/en/install.md) — manual setup;
- [ChatGPT Connector](docs/i18n/en/chatgpt-connector.md) — OAuth / MCP connection;
- [Browser extension](docs/i18n/en/extension.md) — STORE / STANDALONE / DEV identities and continuity;
- [Browser extension privacy](docs/i18n/en/privacy.md) — extension data handling and Limited Use;
- [Troubleshooting](docs/i18n/en/troubleshooting.md) — doctor, Link, Edge, OAuth;
- [Architecture](docs/i18n/en/architecture.md) — Runtime / Edge / Link / Extension boundaries.

Maintainer release gates, UAT, CI, Runtime A/B, and historical GA evidence remain documented, but they are intentionally outside the first-install path.
