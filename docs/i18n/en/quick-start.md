# Quick start: experience the first real remote development loop

This page is the shortest path from “I have Herdr locally” to “ChatGPT can inspect my workstation and keep working after the current browser turn ends.”

For every installation detail, use [Installation](install.md). For the architecture, read [Overview](overview.md) and [Architecture](architecture.md).

## What you are building

```text
ChatGPT / Web AI
  ↓ MCP + OAuth
Cloudflare Edge
  ↓ authenticated workstation routing
herdr-link
  ↓
local herdr-mcp
  ↓
Herdr + Git + shell + agents
```

Optionally, the browser extension adds both a return path and a local Side Panel:

```text
Herdr progress / settled
  ↓
browser extension → current Web conversation
        ↘ Browser Control Center
```

The extension also adds ChatGPT Queue, which saves the next user instruction without interrupting a live reply.

## Prerequisites

You need:

- a working Herdr installation;
- the native `herdr-mcp` binary from [GitHub Releases](https://github.com/whshang/herdr-mcp/releases);
- a Cloudflare account if ChatGPT will connect over the Internet.

A custom domain is not required. Start with `workers.dev`.

Node.js is **not** required to run the local MCP runtime. You may still need Node when deploying Edge with Wrangler.

## 1. Install the local runtime

Download the current release binary for your platform, place `herdr-mcp` on your `PATH`, then verify:

```bash
herdr-mcp doctor
herdr-mcp status
```

Check that something is listening:

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

`200` or `401` proves the local HTTP process is alive. Prefer top-level `doctor` / `status` / `update ...` commands; do not use `herdr-mcp service install` as the normal install path.

## 2. Verify Herdr before adding the Internet

Do not debug Cloudflare while the local runtime cannot see Herdr.

Check:

```bash
herdr --version
herdr api schema >/dev/null
```

The first real MCP-side check should ultimately be `herdr_inspect`: it should show actual workspaces, panes and managed Git roots from your machine.

## 3. Deploy the public Edge

Copy the local Wrangler template:

```bash
cp edge/cloudflare/wrangler.user.example.toml edge/cloudflare/wrangler.user.toml
```

Generate a DNS-safe Worker name:

```bash
node scripts/cloudflare-worker-name.mjs "$(hostname)"
```

Configure the Worker name, workstation identity and OAuth issuer, then deploy:

```bash
cd edge/cloudflare
npx wrangler deploy --config wrangler.user.toml
```

The public origin looks like:

```text
https://<worker>.<account-subdomain>.workers.dev
```

MCP URL:

```text
https://<worker>.<account-subdomain>.workers.dev/mcp
```

Keep the origin stable after ChatGPT is connected.

## 4. Start the workstation link

The workstation connects **outward** to Edge over authenticated WSS. You do not expose port 8772 publicly.

Use the repository `herdr-link` installation/status flow for your local environment, then verify Edge can see the workstation online.

If OAuth works but tools report `workstation offline`, this is the layer to investigate.

## 5. Add the ChatGPT Connector

In ChatGPT Web:

1. enable the available Developer/custom MCP capability for your Workspace;
2. create a custom MCP App/Connector;
3. use `https://<edge-origin>/mcp`;
4. complete OAuth in the browser;
5. open a **new conversation** for validation.

Do not paste `HERDR_MCP_TOKEN` or a Cloudflare API token into ChatGPT.

The current production public contract is **epoch 2 / 18 tools**, including `herdr_skill`.

## 6. Run the first read-only task

Use a prompt such as:

```text
Inspect the current Herdr workspaces and Git status. Read only; do not modify anything.
```

A healthy first loop looks like:

```text
herdr_inspect
  ↓
herdr_skill
  ↓
select a managed Git root
  ↓
herdr_git status
  ↓
herdr_fs_read / grep
  ↓
answer
```

This proves more than a green “connected” badge: it proves the public path reaches the real workstation.

## 7. Try one deterministic edit

After the read-only path is correct, choose a disposable or safe file and ask ChatGPT for a small change plus a verification command.

The desired behavior is:

```text
inspect Git
  ↓
read target file
  ↓
edit/patch
  ↓
run test
  ↓
show diff
```

The Web model should not start a local coding agent merely to edit one known line.

## 8. Try one delegated task

Now choose a task that actually benefits from independent reasoning, for example:

```text
Investigate why this test is failing and implement the narrowest fix. Keep unrelated files unchanged.
```

The Web planner can dispatch a Herdr-native worker, then use `herdr_since`, Git and tests to verify the result.

The agent's final prose is not the source of truth; the repository is.

## 9. Add the browser extension for long work

Install the official **Herdr** extension from the Chrome Web Store. Runtime GitHub Releases do not distribute browser-extension installation packages. If the Store listing is not live yet, skip this optional step. See [Browser extension](extension.md).

Then install the Native Messaging host:

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

Bind the current Web scope to the Herdr workspace doing the work. New Auto scopes default off. First confirm:

- the correct workspace is bound;
- the HUD shows the expected state;
- clicking the Herdr toolbar icon opens Browser Control Center directly and shows the real workspace / panes;
- pane create/remove and working/settled changes appear live;
- ChatGPT Queue can save a next-turn instruction without interrupting the current reply.

Then enable Auto where appropriate.

Manual handoff, where supported, can be started with Auto on or off. During transfer, automatic wakes from the source pause and the target conversation inherits the source Auto state.

See [Browser continuity](browser-continuity.md), [Browser Control Center](browser-control-center.md), and [Wake, recovery and handoff](extension-wake.md).

## Three supported usage routes

### ChatGPT with public Connector

Best when you want a Web model to orchestrate local development from anywhere.

```text
ChatGPT → Edge → workstation
```

### Web AI plus browser bridge

For sites such as z.ai / DeepSeek that do not provide the same native custom MCP connector, the extension has an experimental local JSON→MCP compatibility path. Both site integrations are disabled by default; enable the corresponding switch in **Herdr Settings → Experimental features** and reload the page before testing them.

```text
Web page → extension → Native Messaging → local MCP
```

See [JSON → MCP bridge](extension-bridge.md).

### Local MCP client only

Cursor/curl can connect directly to:

```text
http://127.0.0.1:8772/mcp
```

No Cloudflare Edge is required when the client runs on the same machine.

## What “working” means

A useful acceptance checklist is:

- local runtime is healthy;
- `herdr_inspect` sees real Herdr state;
- Edge sees the workstation online;
- OAuth succeeds;
- a new ChatGPT conversation gets the current tool catalog;
- a read-only real tool call succeeds;
- a small write/test/diff loop succeeds;
- browser continuity can wake the conversation after a long local task if you install the extension.

At that point you have the intended product experience: a Web planner operating a persistent local development workshop rather than a one-shot remote command endpoint.

Next:

- [Installation](install.md) — detailed setup
- [ChatGPT Connector](chatgpt-connector.md) — OAuth, tool snapshots and compatibility
- [Best practices](best-practices.md) — daily orchestration
- [Troubleshooting](troubleshooting.md) — layer-by-layer diagnosis
