# herdr-mcp

**Connect ChatGPT / Web AI safely to your local Herdr workstation.**

ChatGPT keeps the plan and decisions, Herdr keeps the real worksite alive, and herdr-mcp connects the browser model to local files, Git, shell, long-running jobs, and agents.

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

## Browser extension: optional, after the Connector works

The browser extension adds long-conversation continuity, the Side Panel Control Center, workspace binding, and queued next-turn messages. It is **not** required for the first ChatGPT-to-workstation connection.

End users install it only from the **Chrome Web Store**:

1. finish the runtime + Connector verification first;
2. open the [Chrome Web Store](https://chromewebstore.google.com/) and search for `Herdr`;
3. choose the official Herdr extension and click **Add to Chrome**;
4. run `herdr-mcp native-host install` and `herdr-mcp native-host status`;
5. future extension updates are delivered through the normal Chrome Web Store update mechanism; no local extension package is required.

> The extension is currently entering its first Chrome Web Store publication flow. Until the listing is live, normal end users should simply skip this optional step rather than install a local development build.

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
