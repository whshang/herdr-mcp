# herdr-mcp

**Turn the reasoning capacity you already have in ChatGPT / Web AI into a persistent local development environment.**

Web AI subscriptions can provide substantially more usable reasoning capacity than many API or local-agent workflows, and MCP gives those browser models a standard way to call software on your machines. Herdr-MCP starts from that opportunity instead of introducing another model subscription.

**MCP gives Web AI hands. Herdr gives those hands a persistent workplace. The optional browser extension closes the loop.**

ChatGPT can inspect and modify code, use Git, run commands and tests, or delegate long-running work to local coding agents. Herdr keeps workspaces, terminals, processes and agents alive independently of any single chat, so parallel work and conversation handoff do not require rebuilding the local worksite.

**Docs:** https://whshang.github.io/herdr-mcp/ · **Source:** https://github.com/whshang/herdr-mcp

Languages: **English** · [简体中文](README.zh.md) · [日本語](README.ja.md)

## Fastest setup: paste one prompt into your coding agent

Use Cursor, Codex, Claude Code, Pi, Cline, or another local coding agent that can read URLs and run commands:

```text
Install and configure Herdr and herdr-mcp for me. First read and follow this guide end to end: https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/quick-agent-install.md .

Install the local herdr-mcp runtime from GitHub Releases, not from a git clone. Pause only when I personally need to sign in/create a Cloudflare API Token, or when I need to add the herdr Connector/app in ChatGPT. Automate and verify everything else.
```

The local herdr-mcp runtime is a native binary; normal users do **not** need Node.js or npm to run it. Node may be used temporarily by the Agent only for Cloudflare/Wrangler bootstrap.

The guide tells the agent to:

1. check Herdr and install the official stable build if it is missing;
2. install the latest stable `herdr-mcp` runtime from GitHub Releases;
3. deploy Cloudflare Edge, configure the workstation Link, and verify public `/health` and `/mcp`;
4. pause only for Cloudflare sign-in / API Token creation;
5. pause only for adding the `herdr` Connector in ChatGPT and completing OAuth;
6. finish with `herdr-mcp doctor` and a real MCP smoke test.

Prefer to do it manually? See [Installation](docs/i18n/en/install.md).

## First real test

In a new ChatGPT conversation with the `herdr` Connector enabled, send:

```text
Inspect my Herdr projects. Read only; do not modify anything.
```

A healthy setup lets ChatGPT see real workspaces, panes, agents, Git state, and project files through the MCP tools.

### Artifacts and visual work in v0.4.2

The v0.4.2 runtime extends the same workstation boundary to visual and file-import work. `herdr_fs_image` lets ChatGPT inspect PNG/JPEG/GIF/WebP assets directly from managed projects, and the built-in planner policy routes artifacts over the shortest safe path: managed local files go through the direct `herdr_fs_*` tools, safe signed HTTPS URLs are imported directly with `herdr-mcp artifact import --signed-url`, directly consumable MCP/Connector file references are consumed directly, and only the remaining cross-boundary transfers use a private, short-lived Cloudflare R2 generic artifact relay. The Rust runtime imports with HTTPS/SSRF, size, MIME/signature, digest, managed-root, dirty-file, and busy-agent checks before anything is written to the repository. The public MCP catalog stays at 18 tools.

The browser extension is deliberately **not** part of any file/artifact path. Its job is conversation continuity and browser control; artifact transport belongs to the runtime + direct signed import + private artifact relay.

## Browser extension: optional, after the Connector works

The browser extension adds long-conversation continuity, the Side Panel Control Center, workspace binding, and queued next-turn messages. It is **not** required for the first ChatGPT-to-workstation connection.

End users install it only from the **Chrome Web Store**:

1. finish the runtime + Connector verification first;
2. open the [Herdr Chrome Web Store item](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp); while the first listing is still a draft, this direct page can show **Item not available**;
3. choose the official Herdr extension and click **Add to Chrome**;
4. run `herdr-mcp native-host install` and `herdr-mcp native-host status`;
5. future extension updates are delivered through the normal Chrome Web Store update mechanism; no local extension package is required.

> The Store item is currently in its first publication flow. Until the listing is published, the direct item page can be unavailable and Store search may return no result. Normal end users should skip this optional step rather than install a local development build.

See [Browser extension](docs/i18n/en/extension.md) and [Browser Control Center](docs/i18n/en/browser-control-center.md).

## Current support boundary

- stable herdr-mcp: `v0.4.1`;
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
herdr-mcp update status
herdr-mcp rollback
herdr-mcp uninstall
```

`herdr-mcp service ...` is an **advanced / internal** service-control surface, not the normal install path. On `0.4.1+`, `herdr-mcp scan --json` refreshes the evidence-backed inventory of locally startable Herdr agent kinds; see the [CLI reference](docs/i18n/en/cli-reference.md#capability-discovery-scan) for details.

## Read more only when you need it

- [Quick agent install](docs/i18n/en/quick-agent-install.md) — recommended onboarding protocol for a coding agent;
- [Installation](docs/i18n/en/install.md) — manual setup;
- [ChatGPT Connector](docs/i18n/en/chatgpt-connector.md) — OAuth / MCP connection;
- [Browser extension](docs/i18n/en/extension.md) — Chrome Web Store install and continuity;
- [Browser extension privacy](docs/i18n/en/privacy.md) — extension data handling and Limited Use;
- [Troubleshooting](docs/i18n/en/troubleshooting.md) — doctor, Link, Edge, OAuth;
- [Architecture](docs/i18n/en/architecture.md) — Runtime / Edge / Link / Extension boundaries.

Maintainer release gates, UAT, CI, Runtime A/B, and historical GA evidence remain documented, but they are intentionally outside the first-install path.
