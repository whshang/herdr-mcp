# ChatGPT Connector

*Connect Web AI to your local workstation through MCP and OAuth.*

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

4. Complete OAuth in the browser. On first authorization, Herdr shows a short-lived approval request instead of silently granting access; approve it from any computer already enrolled in this Worker or from another Herdr WebChat that was explicitly approved by this Worker.
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

The Edge handles the OAuth boundary. Dynamic Client Registration (DCR) only registers client metadata; it is **not** authorization. Since v0.4.6, a new Connector cannot exchange for a token until an enrolled-device/operator control channel records an explicit approval:

```text
Connector
  │ metadata discovery + DCR
  │ authorize + PKCE
  ▼
Herdr pending approval page
  │ request id + short-lived 6-digit code
  └─ any enrolled computer:
       herdr-mcp connector approve <request-id>
  ▼
authorization code → token → MCP request
```

Devices enrolled in the same Worker have no owner/member hierarchy for this control plane. Worker/operator credentials administer the fleet; an approved Connector receives ordinary MCP access only and cannot approve/revoke other Connectors or pair/revoke devices. A pre-v0.4.6 OAuth token that was issued before explicit approval remains usable for ordinary MCP compatibility until an operator explicitly revokes its client grant. Use `herdr-mcp connector list` to obtain the immutable `connector_id` for current v0.4.6 Connector instances and `herdr-mcp connector revoke <connector-id> --confirm` to revoke one independently. Legacy clients that predate Connector-instance records remain revocable through the compatibility grant tombstone.

The approval code is single-purpose, short-lived, attempt-bounded, and is entered interactively rather than accepted on CLI argv. Disconnecting a Connector in a provider UI is not a substitute for server-side revocation when you intend to remove Herdr authorization.

Troubleshoot:

- public origin consistency;
- OAuth issuer configuration;
- authorization/token exchange;
- audience/resource matching;
- workstation link availability after authentication.

OAuth success does not prove that the workstation is online.

## Tool snapshots and new conversations

ChatGPT uses a reviewed/frozen snapshot of MCP action definitions. A runtime or Edge deployment does not automatically enable newly added actions in an already approved workspace app.

Herdr 0.4.3 separates the two contracts intentionally:

**public ChatGPT contract: epoch 3 / 19 tools; workstation runtime execution contract: epoch 2 / 18 tools.** The extra public action is Edge-local `herdr_devices`; it is never forwarded to a workstation.

Example:

```text
Server: public epoch 3 / 19 tools

Refreshed action set   ✓ can expose epoch 3
Old/frozen action set  → may remain on 18 tools
```

After runtime upgrades:

1. verify Edge/runtime version;
2. do **not** disconnect/delete/re-add the Connector merely because the workstation runtime was upgraded;
3. when the Herdr public action catalog changed, refresh/review/publish the app actions through the workspace controls available on the account, and explicitly enable new actions when required;
4. use a fresh conversation after the action snapshot changes.

Do not reinstall the workstation for a stale tool snapshot. Existing v0.4.2 runtimes continue to execute the epoch-2 18-tool workstation contract; they simply do not gain the v0.4.3 multi-device runtime features until upgraded.

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
