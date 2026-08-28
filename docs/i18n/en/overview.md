# Overview

herdr-mcp lets ChatGPT and other Web AI work directly on a local software project: read and edit code, search repositories, inspect Git, run commands, observe Herdr workspaces, and delegate to local coding agents when independent reasoning is useful. Data and execution stay on your workstation; the public side provides a stable, authenticated MCP entry point.

It addresses a concrete gap. Web models have strong reasoning and large conversational context, but cannot normally see your terminals, repositories, or running agents. Local coding agents can operate the machine, but are usually isolated inside individual terminal sessions. Herdr supplies persistent workspaces, real PTYs, agent state, and a Socket API; herdr-mcp turns that environment into a compact control plane designed for remote models.

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
