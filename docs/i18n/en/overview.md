# Overview

herdr-mcp is the remote/web gateway for [Herdr](https://herdr.dev). Herdr owns local terminal workspaces, panes, agents, sessions, and its native CLI/socket API; herdr-mcp adds the pieces a remote web model cannot get directly: a compact MCP surface, authenticated Cloudflare Edge transport for ChatGPT, safe workstation file/Git/shell access, and a browser extension for progress, recovery, handoff, and JSON-to-MCP compatibility.

If you are new to Herdr itself, start with the official [Herdr documentation](https://herdr.dev/docs/), especially [Install](https://herdr.dev/docs/install/), [Quick start](https://herdr.dev/docs/quick-start/), and [Concepts](https://herdr.dev/docs/concepts/). herdr-mcp deliberately does not duplicate those guides.

## What this project adds

```text
ChatGPT / z.ai / DeepSeek
        |
        | MCP or browser JSON bridge
        v
herdr-mcp remote control plane
        |
        | Herdr socket + managed workstation access
        v
Herdr workspaces / panes / agents
```

- **ChatGPT Connector:** a stable MCP/OAuth endpoint through Cloudflare Worker + persistent workstation link.
- **Remote workstation tools:** targeted project file, Git, shell, image, workspace, pane, and agent operations without exposing arbitrary local disk access.
- **Browser continuity:** progress push-back, reply recovery, manual/automatic handoff, and conversation-scoped automation.
- **z.ai / DeepSeek compatibility:** JSON tool calls are bridged to the local MCP runtime through Chrome Native Messaging and a mode-`0600` Unix socket; the browser stores no Herdr bearer.

## Know the boundary

Use the upstream Herdr docs when the question is about:

- workspace / tab / pane / agent / session concepts;
- installing or updating the Herdr binary;
- native Herdr CLI commands;
- raw socket API methods and events;
- running or automating agents inside Herdr.

Useful upstream references: [Agents](https://herdr.dev/docs/agents/), [Agent automation](https://herdr.dev/docs/agent-automation/), [CLI reference](https://herdr.dev/docs/cli-reference/), [Socket API](https://herdr.dev/docs/socket-api/), and [Agent skill file](https://herdr.dev/docs/agent-skill/).

Use these herdr-mcp docs when the question is about the web/remote layer: connecting ChatGPT, deploying the Edge, browser automation, remote-planner safety, or maintaining the local MCP runtime.

## Choose your next step

- First install: [Quick start](quick-start.md).
- ChatGPT is the main client: [Connect ChatGPT](chatgpt-connector.md).
- z.ai / DeepSeek is the main client: [JSON → MCP bridge](extension-bridge.md).
- You want the browser to keep long work moving: [Browser extension](extension.md).
- Something is broken: [Troubleshooting](troubleshooting.md).
