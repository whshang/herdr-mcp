# ChatGPT Connector: connecting Web AI to your local workstation

herdr-mcp gives ChatGPT access to a real development environment without exposing your machine directly.

```text
ChatGPT
  │ OAuth + MCP
  ▼
Cloudflare Edge
  │ authenticated WSS
  ▼
herdr-link / herdr-mcp
  │
  ├─ files / Git / shell
  └─ Herdr workspaces / panes / agents
```

This page explains the ChatGPT side: connection, OAuth, tool snapshots and troubleshooting. For deployment, see [Installation](install.md). For the complete architecture, see [Architecture](architecture.md).

## Three different meanings of "connected"

A connector can be connected at one layer and still fail at another.

### OAuth success

ChatGPT knows the identity of the MCP service and has authorization.

### MCP handshake success

ChatGPT completes initialization and receives `tools/list`.

### Workstation success

Tool calls can travel through Edge and reach the actual Herdr workstation.

A green connector status only proves one layer. The reliable validation is a fresh conversation calling `herdr_inspect`.

## Add the MCP Connector

The exact ChatGPT UI evolves. The general flow is:

1. Enable Developer mode where available.
2. Add a custom MCP App / Connector.
3. Enter:

```text
https://<your-edge-origin>/mcp
```

4. Complete OAuth in the browser.
5. Create a new conversation for validation.

Never paste `HERDR_MCP_TOKEN` into ChatGPT. Public ChatGPT access uses OAuth. Static bearer is for local clients such as curl or Cursor.

Organization policies may require administrator approval for custom apps. herdr-mcp does not bypass ChatGPT workspace governance.

## Stable origin and OAuth issuer

The public origin is an identity, not just a URL.

Recommended:

```text
HERDR_MCP_BASE_URL=https://herdr-edge.example.workers.dev
MCP URL=https://herdr-edge.example.workers.dev/mcp
```

`HERDR_MCP_BASE_URL` does not include `/mcp`.

Keep the Edge origin stable. Local runtime generations can upgrade behind it without changing the ChatGPT connector.

## OAuth flow

The Edge handles the OAuth boundary:

```text
ChatGPT
  │ metadata discovery
  │ authorize + PKCE
  │ token
  ▼
MCP request
```

Troubleshoot:

- public origin consistency;
- OAuth issuer configuration;
- authorization/token exchange;
- audience/resource matching;
- workstation link availability after authentication.

OAuth success does not prove that the workstation is online.

## Tool snapshots and new conversations

ChatGPT caches MCP tool definitions per conversation.

Current production contract:

**contract epoch 2 / 18 tools, including `herdr_skill`.**

Example:

```text
Server: epoch 2 / 18 tools

New conversation       ✓ sees epoch 2
Old conversation       → may keep old snapshot
```

After runtime upgrades:

1. verify Edge/runtime version;
2. refresh the connector if the UI provides that action;
3. create a new conversation.

Do not reinstall the workstation for a stale tool snapshot.

## Why the catalog is intentionally small

Herdr exposes many native Socket API methods. Registering every method as an MCP tool would consume context and make selection harder.

The public catalog focuses on common remote work:

- `herdr_inspect`
- `herdr_since`
- `herdr_fs_*`
- `herdr_git`
- `herdr_exec*`
- `herdr_prompt`

Advanced native capabilities remain available through dynamic discovery.

## First validation request

Use a safe first request:

```text
Inspect the current Herdr workspaces and Git state. Read only; do not modify anything.
```

Expected:

1. `herdr_inspect` returns real workstation data;
2. `herdr_skill` can provide current guidance;
3. managed Git roots are visible;
4. file/Git operations work.

## Permission confirmations

ChatGPT may show confirmation UI for actions. Those controls belong to ChatGPT's safety layer.

The browser extension can handle clearly identifiable page-level Allow actions under strict conditions, but cannot bypass workspace policy or browser/system permission dialogs.

See [Browser continuity](browser-continuity.md) and [Extension wake](browser-continuity.md).

## Why browser continuity exists

MCP is request-driven. After ChatGPT sends a task to a local Agent, the browser conversation does not automatically wake when the Agent finishes later.

```text
ChatGPT → MCP → Herdr Agent

Agent finishes later

(no automatic browser turn without another channel)
```

The extension provides the reverse direction:

```text
Herdr events → browser → ChatGPT conversation
```

Connector solves Web AI reaching the workstation. Browser continuity solves the workstation reaching the conversation.

## Troubleshooting map

| Symptom | Check |
|---|---|
| Cannot add connector | public URL, OAuth metadata, workspace policy |
| OAuth works but no tools | tools/list, schema, connector refresh, old conversation snapshot |
| Tools exist but workstation offline | herdr-link, runtime health, identity |
| File operations fail | managed root, permissions, gates |
| Agent finishes but browser stops | extension binding and continuity settings |

## Minimum acceptance

A real ChatGPT integration should satisfy:

- OAuth completes;
- a fresh conversation receives the current catalog;
- `herdr_inspect` sees the workstation;
- a managed project can be read;
- a safe command can execute;
- permissions behave as expected;
- browser continuity works for long-running tasks when installed.
