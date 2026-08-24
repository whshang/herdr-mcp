# Quick start

Goal: get from an existing Herdr installation to one working web client without reading the maintainer/runtime documents first.

## 1. Confirm Herdr first

herdr-mcp does not install or replace Herdr. If needed, use the official [Herdr Install](https://herdr.dev/docs/install/) and [Quick start](https://herdr.dev/docs/quick-start/) guides. Then confirm:

```bash
herdr --version
herdr api schema >/dev/null
```

If workspace/tab/pane/agent terminology is unfamiliar, read [Herdr Concepts](https://herdr.dev/docs/concepts/) before continuing.

## 2. Build herdr-mcp

```bash
git clone https://github.com/whshang/herdr-mcp.git
cd herdr-mcp
npm install
npm run build
```

For a local coding agent, you can instead use the [Agent-assisted install](agent-install.md). The agent should first read the target project's `AGENTS.md`, `CLAUDE.md`, and `README.md` when present, then follow the install guide rather than improvising.

## 3. Pick one client path

### ChatGPT

ChatGPT needs the authenticated public Edge. Continue with [Install](install.md), then [Connect ChatGPT](chatgpt-connector.md). The default first deployment uses `workers.dev`; you do not need a custom domain.

### z.ai / DeepSeek

These sites can use the local browser bridge and do not need a fake public MCP connector. Install the Chrome extension/native host, bind the conversation to a workspace, then follow [JSON → MCP bridge](extension-bridge.md).

## 4. Understand automation scope

- ChatGPT **Project** automation is shared by Project id and is gated by the Project automation option.
- Plain ChatGPT `/c/<id>`, z.ai, and DeepSeek have **conversation-scoped Auto**. They can turn Auto on from their HUD even when Project automation is disabled globally.
- A scope defaults to Auto off until you explicitly enable it.

## 5. Verify the boundary that matters

Do not stop at “the process started.” Verify from the client you intend to use:

- local runtime is reachable;
- ChatGPT sees the current 18-tool epoch-2 contract, or the z.ai/DeepSeek bridge can list/call the local catalog;
- the browser extension can observe the bound workspace;
- if Auto is enabled, a real progress/settled event reaches the conversation.

Next: [Install](install.md) for the full manual path, or [Troubleshooting](troubleshooting.md) if one of these boundaries fails.
