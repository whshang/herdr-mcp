# Overview

herdr-mcp connects **Web AI reasoning** to a **persistent, observable, human-takeover-friendly local development workstation**.

The shortest product model has three layers:

```text
ChatGPT / Web AI
      │ MCP + OAuth
      ▼
Cloudflare Edge
      │ authenticated workstation link
      ▼
herdr-mcp + Herdr workstation
      ├─ files / Git / shell / images
      ├─ workspace / pane / agent state
      └─ optional local workers
```

The browser extension is an optional fourth layer. It returns local progress to the correct Web conversation and exposes a Chrome Side Panel Control Center; standard MCP does not depend on it.

## What you actually get

- Web AI can read and modify real Git projects instead of only producing code snippets.
- Workspace, PTY, process, and agent lifetimes belong to Herdr, not to one chat turn.
- The Web planner performs deterministic small work directly and delegates only tasks that benefit from an independent local worker.
- The workstation connects outward to the public Edge, so the development machine needs no public inbound port.
- Mutation semantics, managed roots, OAuth, Native Messaging, and browser continuity have explicit boundaries.

This is not another Coding Agent. Herdr is the persistent workplace; herdr-mcp is the remote control plane that lets a Web planner operate that workplace.

## Herdr and herdr-mcp responsibilities

**Herdr** owns workspace / tab / pane / agent / session concepts, PTYs, the native CLI, Socket API, and local agent lifecycle. Treat the [Herdr documentation](https://herdr.dev/docs/) as authoritative for those behaviors.

**herdr-mcp** owns:

- the MCP contract for ChatGPT / Web AI;
- Cloudflare Edge, OAuth, and the workstation link;
- file, Git, shell, and image capabilities inside managed Git roots;
- planner-facing state summaries, mutation semantics, and agent delegation;
- optional Browser Continuity, Control Center, and the experimental JSON → MCP bridge.

For why this division is preferable to tmux/cmux/ACP or other coding MCP approaches, read [Ecosystem and architecture comparison](herdr-vs-ecosystem.md). For the deeper design principles, read [Design Philosophy](design-philosophy.md). For the complete technical path, read [Architecture](architecture.md).

## Where to start

- **Let an Agent perform installation directly:** [Agent install](agent-install.md)
- **Understand manual installation and operations:** [Installation](install.md)
- **Run the first real task after installation:** [Quick start](quick-start.md)
- **Connect ChatGPT:** [ChatGPT Connector](chatgpt-connector.md)
- **Learn the normal working style:** [Best practices](best-practices.md)
- **Add long-running Web continuity when needed:** [Browser extension](extension.md)
- **Diagnose a failure:** [Troubleshooting](troubleshooting.md)
