# Overview

## Start with the capability already available in Web AI

The value of ChatGPT and other Web AI products is not only model quality: subscriptions can also provide substantial usable reasoning capacity under a different quota and pricing model from APIs or local coding agents. Those limits change by account, model and product policy, so Herdr-MCP does not promise a fixed multiplier.

MCP is the architectural unlock. Once a Web AI host can call an HTTP service through MCP, the strong model in the browser can reach software running in your own environment instead of remaining a chat-only surface.

The simplest design is Web AI → MCP → files / Git / shell. Herdr-MCP goes one step further and connects MCP to a persistent, observable, human-takeover-friendly Herdr worksite.

> **MCP gives Web AI hands. Herdr gives those hands a persistent workplace. The browser extension closes the loop.**

That is also why Herdr-MCP does not reinvent another coding agent. The Web AI can perform deterministic small operations directly and delegate complex or parallel work to replaceable local agents. A browser conversation can end or hand off while the workspace, PTYs, processes, agents and worktrees remain alive.

## What you get

```text
You
│
▼
ChatGPT / Web AI
│  MCP + OAuth
▼
Cloudflare Edge
│  authenticated WSS
▼
herdr-link + herdr-mcp runtime
│
├─ files / search / patch / Git / shell / images
├─ Herdr workspace / pane / agent state
└─ Pi / Grok / other local agents

Browser extension ── continuity / Queue ──► Web conversation
                 └── live workspace / pane state ──► Chrome Side Panel
```

For routine work, ChatGPT can inspect a project and perform deterministic operations much like a local coding agent. When a task benefits from independent reasoning, parallel implementation, or review, it can delegate that task to an agent running inside Herdr. Because Herdr retains the workspaces and panes, the remote planner can keep track of what exists, who is working, and what changed.

## Why Herdr is the foundation

A filesystem/shell MCP can execute commands, but it does not naturally represent a long-lived development workstation. Herdr already exposes a Socket API and a broad terminal-control surface while maintaining stable workspace, tab, pane, agent, and session state. Open terminals, agents, and working directories therefore have identities that a remote model can inspect, resume, intervene in, and schedule against.

That provides several important properties:

- **Persistent workspaces** — terminals and agent sessions do not exist only for the duration of one Web request.
- **Observability** — the remote model can inspect workspaces, panes, agent state, and recent events.
- **Delegation** — suitable work can be sent to Pi, Grok, or another local agent and followed through completion.
- **Recovery** — Herdr owns the long-lived work state, so a reconnect or restart can rediscover the current development environment.
- **No second terminal platform** — herdr-mcp uses Herdr's Socket API and adds only the remote filesystem, Git, shell, authentication, and planner-facing layers that Web clients lack.

## Herdr vs. herdr-mcp

Herdr owns the local development workstation: workspace / tab / pane / agent / session concepts, PTYs, its native CLI, Socket API, and agent automation. Treat the [Herdr documentation](https://herdr.dev/docs/) as authoritative for those behaviors.

herdr-mcp owns the remote connection and orchestration layer:

- the MCP contract exposed to ChatGPT;
- Cloudflare Edge, OAuth, and the persistent workstation link;
- controlled file, Git, shell, and image access inside managed Git projects;
- planner-oriented state summaries and agent delegation;
- browser progress delivery, timeout recovery, auto-continue, and long-conversation rollover;
- the local JSON → MCP bridge for z.ai and DeepSeek.

## Tool design

The current production public contract is **epoch 2 / 18 tools**. It deliberately does not duplicate every Herdr Socket API method as a separate MCP tool.

The surface has four roles:

1. `herdr_inspect` / `herdr_since` cheaply establish the current working state.
2. `herdr_fs_*` / `herdr_git` / `herdr_exec*` perform deterministic workstation operations directly.
3. `herdr_prompt` delegates work that benefits from independent reasoning or parallel execution.
4. `herdr_methods` / `herdr_call` discover and invoke advanced native Herdr Socket API capabilities when needed.

`herdr_skill` supplies operating policy matched to the current Herdr / herdr-mcp environment. The remote model does not need 90+ native methods permanently occupying its tool list and context.

## Security boundary

The workstation initiates the authenticated connection to the Edge; it does not need a public inbound port. The ChatGPT Connector uses OAuth, and the local static bearer should not be copied into ChatGPT. File tools are constrained to managed Git roots discovered through Herdr and skip common secret-like filenames. Mutation can be further restricted with `HERDR_MCP_READONLY` and `HERDR_MCP_WRITE_ROOTS`.

Shell execution is intentionally powerful. Granting it means trusting the remote model to execute code on that development workstation. Treat the workstation, Cloudflare identity, and ChatGPT account as parts of the security boundary.

## Where to start

- First deployment: [Quick start](quick-start.md)
- Let a local agent install it: [Agent install](agent-install.md)
- Connect ChatGPT: [ChatGPT Connector](chatgpt-connector.md)
- Understand the full path: [Architecture](architecture.md)
- Learn the normal workflow: [Best practices](best-practices.md)
- Configure the browser workspace layer: [Browser extension](extension.md) and [Browser Control Center](browser-control-center.md)
- Diagnose installation and runtime failures: [Troubleshooting](troubleshooting.md)
