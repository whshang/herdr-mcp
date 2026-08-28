# herdr-mcp

**A remote control plane that lets Web AI work on a real local development workstation through Herdr.**

ChatGPT can reason about a repository, but the browser cannot see your local files, Git state, shell, long-running processes or Herdr workspaces by itself. herdr-mcp connects those two worlds without exposing the workstation directly to the public Internet.

**Docs:** https://whshang.github.io/herdr-mcp/ · **Source:** https://github.com/whshang/herdr-mcp

Languages: **English** · [简体中文](README.zh.md) · [日本語](README.ja.md)

## What it does

herdr-mcp gives a Web planner five things it normally lacks:

- **persistent local context** — Herdr workspaces, panes and agent lifecycle;
- **deterministic workstation tools** — files, Git, images and shell;
- **delegation** — send bounded reasoning tasks to local Herdr workers when useful;
- **stable remote access** — OAuth/MCP at Cloudflare Edge with an outbound workstation link;
- **browser workspace layer** — continuity back into the Web conversation, a live Chrome Side Panel Control Center, and queued next-turn user intent.

The model is simple:

```text
User
  ↓
ChatGPT / Web AI
  ↓ MCP + OAuth
Cloudflare Edge
  ↓ authenticated routing
herdr-link
  ↓
local herdr-mcp runtime
  ↓
Herdr workspace
  ├─ files / Git / shell
  └─ local agents

Herdr progress
  ↓
browser extension
  ↓
Web conversation resumes
```

## What it is not

herdr-mcp does **not** replace Herdr, create another agent runtime, or turn every Herdr socket method into a public MCP tool.

The Web model remains the planner. Herdr remains the persistent local workspace. Local agents are workers. Git and runtime state are the source of truth.

## Why the public tool surface is small

Herdr exposes a much larger native Socket API than a Web model should carry in every MCP tool catalog.

The public contract therefore keeps high-frequency remote operations first-class and exposes the Herdr long tail dynamically:

```text
frequent work
  → herdr_inspect / herdr_since / herdr_fs_* / herdr_git / herdr_exec* / herdr_prompt

native Herdr long tail
  → herdr_methods → herdr_call
```

Current production contract: **epoch 2 / 18 tools**, including read-only `herdr_skill`.

## Fastest path to a working setup

Prerequisites:

- [Herdr](https://herdr.dev) installed and running;
- a Cloudflare account if ChatGPT will connect over the Internet.

The **local MCP runtime** is a native binary. You do **not** need Node.js or npm to run it. Node remains useful for Cloudflare Edge deploys, the browser extension toolchain, and contributor builds from this repository.

### Install the native runtime (primary)

1. Download the current `herdr-mcp` binary for your platform from [GitHub Releases](https://github.com/whshang/herdr-mcp/releases) — current stable: [`v0.4.0`](https://github.com/whshang/herdr-mcp/releases/tag/v0.4.0).
2. Place it on your `PATH` (for example `~/.local/bin/herdr-mcp`) and make it executable.
3. Install the managed local service, then verify:

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

`install` stages an immutable generation under `~/.config/herdr-mcp/runtime/` and retargets `~/.local/bin/herdr-mcp` to `runtime/current/herdr-mcp`. Day-to-day lifecycle after that:

```bash
herdr-mcp update apply
herdr-mcp update status
herdr-mcp rollback
```

Prefer these top-level commands. Do **not** treat `herdr-mcp service install` as the normal user install path; `service ...` stays advanced/internal. Do **not** clone this repository or run `npm`/`cargo` to install the local MCP runtime.

The current stable release is **`v0.4.0`**. By default, `update.channel = "stable"` discovers non-prerelease releases only. Use `preview` only when you intentionally want prerelease tags.

Verify Herdr before adding Edge:

```bash
herdr --version
herdr api schema >/dev/null
```

For ChatGPT, deploy the Cloudflare Worker on the default `workers.dev` origin, start the managed Link, then add the public `/mcp` URL as a custom MCP App/Connector and complete OAuth. See [Installation](docs/i18n/en/install.md).

Do **not** paste `HERDR_MCP_TOKEN` or a Cloudflare API token into ChatGPT.

### Let a local coding agent install it

If you already use Codex, Claude Code, Pi, DSH, Cline or another local coding agent, give it this one sentence instead of asking it to guess the deployment steps:

```text
Install herdr-mcp for me. Read and follow the full protocol at https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/quick-agent-install.md end to end. Use GitHub Releases for the local runtime (not git clone). Pause only for Cloudflare login/API Token creation. Do not echo or commit secrets.
```

The detailed maintainer-oriented contract remains in [Agent-assisted installation](docs/i18n/en/agent-install.md).

The deterministic Worker-name helper used by that Edge flow is:

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

### Appendix: contributor build from source

Clone this repository only when you are developing herdr-mcp itself. Source builds may still use the Node toolchain for site/extension/Edge packages; that path is not the primary way end users run the MCP runtime. Details live under [Installation](docs/i18n/en/install.md#appendix-developer-from-source).

Full walkthrough: [Quick start](docs/i18n/en/quick-start.md) · [Installation](docs/i18n/en/install.md) · [ChatGPT Connector](docs/i18n/en/chatgpt-connector.md)

## A good first task

After the Connector is ready, open a **new conversation** and ask:

```text
Inspect the current Herdr workspaces and Git status. Read only; do not modify anything.
```

A healthy loop usually looks like:

```text
herdr_inspect
  ↓
herdr_skill
  ↓
herdr_git status
  ↓
herdr_fs_read / grep
  ↓
answer
```

Then try one small edit + test + diff. Delegate to a local agent only when the task benefits from independent reasoning.

## Browser extension

MCP is request-driven:

```text
Web AI → workstation
```

A local agent may keep working after the browser turn ends. The optional MV3 extension adds both the reverse continuity path and a browser-local view of the workstation:

```text
workstation → Web conversation
          ↘ Chrome Side Panel Control Center
```

Current browser-product surfaces include workspace binding, progress/settled wakeups, evidence-first recovery, long-conversation handoff, the read-only/preview-only Browser Control Center, ChatGPT Queue for next-turn user intent, and a bounded JSON→MCP compatibility bridge for sites such as z.ai / DeepSeek.

Browser extension distribution is still gated by G15. True browser mutation remains separately deferred under G16; neither is part of the first-GA platform claim yet. Use the versioned extension Release asset plus the managed Native Messaging installer from the installed runtime, and follow [Browser extension](docs/i18n/en/extension.md) / [Browser Control Center](docs/i18n/en/browser-control-center.md). Do not treat a git-checkout `extension/` directory as the primary end-user path.

## Security model

Key boundaries are explicit:

- the local runtime binds to loopback;
- the workstation creates an **outbound** authenticated WSS connection to Edge;
- ChatGPT uses OAuth at the public Edge;
- browser continuity uses Native Messaging + a mode-`0600` local Unix socket;
- `herdr_fs_*` is constrained by managed Git roots and write/secret-path gates;
- `herdr_exec` is a stronger shell capability and is **not** a sandbox;
- uncertain mutations are inspected before retry rather than blindly repeated.

See [Architecture](docs/i18n/en/architecture.md) and [Best practices](docs/i18n/en/best-practices.md).

## Local runtime CLI

Once the native runtime is installed, day-to-day lifecycle uses these top-level commands:

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

`herdr-mcp service ...` remains available for advanced/internal service control (for example `service install --adopt-node`). Prefer the top-level commands above for normal install, health, update, and rollback.

See [CLI reference](docs/i18n/en/cli-reference.md) and [Runtime A/B](docs/i18n/en/runtime-self-upgrade.md).

## Runtime upgrades without changing the Connector

The public Edge identity and the local runtime are separate release planes.

```text
stable Edge / OAuth / MCP URL
        ↓
persistent herdr-link
        ↓
runtime generation A / B
```

A new local generation can be validated, activated and rolled back without changing the ChatGPT Connector URL, as long as the public contract epoch remains compatible.

See [Runtime A/B](docs/i18n/en/runtime-self-upgrade.md).

## Documentation map

Start here:

- [Overview](docs/i18n/en/overview.md)
- [Design philosophy](docs/i18n/en/design-philosophy.md)
- [Quick start](docs/i18n/en/quick-start.md)
- [Installation](docs/i18n/en/install.md)
- [Clean-machine UAT (G18)](docs/i18n/en/clean-machine-uat.md)
- [Release model](docs/release-model.md) — runtime vs extension vs contract
- [GA release gate](docs/ga-release-gate.md)
- [ChatGPT Connector](docs/i18n/en/chatgpt-connector.md)

Operate the system:

- [Browser continuity](docs/i18n/en/browser-continuity.md)
- [Browser extension](docs/i18n/en/extension.md)
- [Architecture](docs/i18n/en/architecture.md)
- [Best practices](docs/i18n/en/best-practices.md)
- [CLI reference](docs/i18n/en/cli-reference.md)
- [Cloudflare Edge deployment](docs/i18n/en/cloudflare-edge-deployment.md)
- [Runtime A/B](docs/i18n/en/runtime-self-upgrade.md)
- [Troubleshooting](docs/i18n/en/troubleshooting.md)

Maintainer reference:

- [Automation](docs/i18n/en/automation.md)
- [Capability benchmark](docs/i18n/en/capability-benchmark.md)
- [Why Herdr + herdr-mcp](docs/i18n/en/herdr-vs-ecosystem.md)
- [Worker fallbacks](docs/i18n/en/worker-fallbacks.md)
- [Local-agent installation protocol](docs/i18n/en/agent-install.md)

## Development checks

```bash
npm run build
npm test
npm run test:edge
npm run build:site
git diff --check
```

The documentation site is generated from the bilingual logical-document set; new formal pages must exist in both `docs/i18n/en` and `docs/i18n/zh-CN`.
