# herdr-mcp

MCP HTTP gateway so ChatGPT (and other web LLMs) can drive local [Herdr](https://herdr.dev): inspect panes and agents, edit git-managed projects, run shells, and prompt cheap on-machine workers. A Chrome extension types progress / continue back into the bound web chat.

[Herdr](https://herdr.dev) is a terminal multiplexer for coding agents. This repo is the door for **remote** clients that cannot see your socket or disk. It does **not** re-wrap Herdr’s ~90 native methods as MCP tools.

**This repo does not:** replace Herdr; give DeepSeek a fake OAuth connector; send the extension over the public tunnel (it talks to `127.0.0.1` only).

**Languages (same as herdr):** [English](README.md) (default on GitHub) · [简体中文](README.zh.md) · [日本語](README.ja.md).  
CLI / browser extension: first install follows system language (`en` / `zh` / `ja`); unknown → English. Change anytime: `herdr-mcp lang`, or extension Options → Language.

## Architecture (you ↔ web ↔ MCP ↔ herdr; extension as reverse channel)

Top to bottom: you → web chat → (herdr-mcp and chrome-extension **same row**) → Herdr panes → local agents.  
Agents’ progress / settled events reach the extension; the extension ↻ types into the web chat. Details: [docs/extension-wake.md](docs/extension-wake.md) (中文).

**Orchestration (web plans, local stays cheap):**

- Prefer `herdr_fs_*` / `herdr_git` / `herdr_exec` (no local-agent API).
- If reasoning is required, `herdr_prompt` a cheap/fast worker (`pi`, `flash`, `cline`, `opencode`, `anti`) or auditor (`droid`, `grok`) — do not route through local Claude/OMP/main.
- `inspect`/`since` soft-hide Claude/OMP/Codex by default. Prompting by known pane still works. `HERDR_MCP_AGENT_ALLOW=*` shows all.
- Session start: `herdr_inspect` → `herdr_skill` (once) → work.

```mermaid
flowchart TB
  You[You]
  Web[Web chat<br/>e.g. ChatGPT]
  MCP[herdr-mcp]
  Ext[herdr-mcp-chrome-extension]
  Herdr[Herdr panes]
  Agents[Local cheap workers<br/>pi / flash · edit / test]

  You --> Web
  Web -->|call MCP| MCP
  MCP --- Ext
  MCP -->|reach herdr| Herdr
  Herdr -->|dispatch| Agents
  Agents -.->|progress / settled| Ext
  Ext -.->|type into web chat| Web
```

## Platforms and start

The **Node server** runs wherever Herdr does: **macOS / Linux / Windows** (Node.js 20+). It does not scan Herdr’s install directory — it connects to the API socket (default `~/.config/herdr/herdr.sock`, override `HERDR_SOCKET_PATH`) and runs `herdr api schema` from `PATH`.

Two ways to run:

| Path | Who | How |
|---|---|---|
| Foreground | any OS | `node dist/server.js` (below) |
| `herdr-mcp` CLI | **macOS** LaunchAgent | `bin/herdr-mcp start` / `status` / `logs` / `watchdog` |

`npm` `bin` is `dist/server.js`, not the bash CLI. On macOS you may `ln -sf …/bin/herdr-mcp ~/.local/bin/herdr-mcp`. systemd / Task Scheduler are out of scope.

```bash
export HERDR_MCP_TOKEN="$(openssl rand -hex 16)"   # or reuse an existing token
export HERDR_MCP_PORT=8772
# for a public Connector (no /mcp suffix):
# export HERDR_MCP_BASE_URL=https://xxxx.trycloudflare.com
node dist/server.js
```

## Install (zero to working)

### 0. Prerequisites

- [herdr](https://herdr.dev) installed and running
- Node.js 20+ (`node -v`)
- For ChatGPT: public HTTPS via `cloudflared` ([Quick Tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/do-more-with-tunnels/trycloudflare/)) or your own domain

### 1. Download and build

```bash
git clone https://github.com/whshang/herdr-mcp.git
cd herdr-mcp
npm install
npx tsc
mkdir -p ~/.config/herdr-mcp
```

### 2. Start the local MCP server

```bash
export HERDR_MCP_TOKEN="$(openssl rand -hex 16)"
echo "token=$HERDR_MCP_TOKEN"   # keep for Cursor / the browser extension
node dist/server.js
# optional check: curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

### 3. Connect ChatGPT (recommended: free Cloudflare)

In another terminal:

```bash
cloudflared tunnel --url http://127.0.0.1:8772
# note https://xxxx.trycloudflare.com
```

Restart MCP with that origin (**no** `/mcp`):

```bash
export HERDR_MCP_BASE_URL=https://xxxx.trycloudflare.com
export HERDR_MCP_TOKEN=...   # same as before
node dist/server.js
```

#### Add the Connector in ChatGPT **web** (not the chat UI / desktop app)

You **cannot** add an MCP server from inside a ChatGPT client chat. Use the website:

1. Open [https://chatgpt.com/#settings/Plugins](https://chatgpt.com/#settings/Plugins), turn on **Developer mode**
2. Open [https://chatgpt.com/plugins#settings/Connectors?create-connector=true](https://chatgpt.com/plugins#settings/Connectors?create-connector=true)
3. Enter a name and the MCP URL `https://xxxx.trycloudflare.com/mcp` (same as `HERDR_MCP_BASE_URL` + `/mcp`)
4. Click login and wait for the redirect back (this server’s OAuth flow is effectively login-free — **do not** paste an API key / token)
5. Start a **new** chat after connecting (old chats keep a stale tool snapshot)

#### Errors or tools missing

If you see:

- `Error fetching OAuth configuration` / `MCP server https://xxx.trycloudflare.com/mcp does not implement OAuth`
- `There was a problem connecting xxx. Try again later.`
- Or the connector shows as added but tools never appear

First confirm `HERDR_MCP_BASE_URL` matches the HTTPS origin ChatGPT uses, `cloudflared` is still running, and `herdr-mcp status` shows the public URL reachable. If that looks fine, **reconnect a few times** in the plugins / connectors UI — usually ChatGPT cache or network. Hard requirements: [docs/chatgpt-connector.md](docs/chatgpt-connector.md) (中文).

#### Model access

Free ChatGPT is limited to **GPT-5.5-mini**. Plus (or higher) lets you use stronger models in chat to drive projects on your machine via the connector.

Quick Tunnel hostnames change when you restart `cloudflared` — update `HERDR_MCP_BASE_URL` and restart MCP. For a stable hostname, use a named Cloudflare tunnel or your own domain.

### 4. Cursor (optional, this machine)

`~/.cursor/mcp.json` — local only (do not also enable the public URL in the same profile):

```json
{
  "mcpServers": {
    "herdr-mcp-local": {
      "url": "http://127.0.0.1:8772/mcp",
      "headers": {
        "Authorization": "Bearer <paste: herdr-mcp token>"
      }
    }
  }
}
```

### 5. Browser extension (optional)

Folder: `extension/` (MV3). Chrome shows the name **herdr → Web wake**. Load unpacked in `chrome://extensions`. Options: `http://127.0.0.1:8772` + the same static token. See [Browser extension](#browser-extension).

## Endpoints

| Use | URL |
|---|---|
| Local MCP | `http://127.0.0.1:8772/mcp` |
| Public MCP | `{HERDR_MCP_BASE_URL}/mcp` |
| Extension SSE | `http://127.0.0.1:8772/push/events` |
| Extension snapshot | `http://127.0.0.1:8772/push/state` |

Auth for connectors: **OAuth (DCR / CIMD)**. Static Bearer is for local curl / Cursor / the extension (`herdr-mcp token`). Never paste that token into the ChatGPT connector UI.

## CLI (macOS)

```bash
herdr-mcp              # menu
herdr-mcp status
herdr-mcp connector
herdr-mcp start | stop | restart   # LaunchAgent
herdr-mcp logs [-f]
herdr-mcp token | url
herdr-mcp lang [en|zh|ja]   # UI language (first run: system; default en)
herdr-mcp watchdog install  # every 120s: restart MCP if down; TaskGroup = log only
herdr-mcp watchdog status
```

After code changes: `npx tsc && herdr-mcp restart` (or restart the `node dist/server.js` process).

## Default tools (why these 18)

Herdr’s native surface is a large Unix-socket API (`herdr api schema`, ~90 methods). herdr-mcp does **not** re-wrap every method as an MCP tool (that burns context and duplicates herdr). Instead:

| Layer | MCP tools | Relation to herdr |
|---|---|---|
| Skill | `herdr_skill` | Read-only copy of upstream Herdr `SKILL.md` (this process fetches GitHub; ChatGPT does not). |
| Passthrough | `herdr_methods`, `herdr_call` | Thin gate to the **native** socket API. Discover with `herdr_methods`, then call any method via `herdr_call`. |
| Remote orchestration | `herdr_inspect`, `herdr_since`, `herdr_prompt` | Small helpers for web clients that only run when the user sends a message (no local polling loop). Built on snapshot/events/`agent.prompt` — not new herdr features. |
| Remote workstation | `herdr_fs_*`, `herdr_exec` / `herdr_exec_*`, `herdr_git` | **Not** herdr tools. The MCP client runs off-machine and cannot see your disk; these fill that gap under managed git roots. |

| Tool | What it does |
|---|---|
| `herdr_skill` | Read-only: prefer latest `SKILL.md` from herdr **master**; if the network fails, use the bundled copy (`assets/herdr-agent-SKILL.md`). Call once per session before `herdr_call` / `herdr_prompt`. `HERDR_SKILL_NETWORK=0` forces bundled. |
| `herdr_methods` | List live herdr socket methods + parameter schemas (cached reflection). Use before unknown `herdr_call`s. |
| `herdr_call` | Call any herdr method with validated params (`{ method, params }`). Covers panes, workspaces, agents, etc. without one MCP tool per method. |
| `herdr_inspect` | One-shot: connection health + workspaces / tabs / panes / agents (cwd, status), plus `workstation_info`, `boot_id`, and `exec_sessions`. Usual first call. |
| `herdr_since` | Cheap digest since a cursor — resume a conversation without re-dumping full state (`boot_id` / `cursor_reset` across MCP restarts). |
| `herdr_prompt` | Deliver via socket `agent.prompt` (default fire-and-forget; strongly prefer `idempotency_key`; track with `herdr_since` / `herdr_inspect`). Prefer cheap workers; do not hand planning/delegation to local Claude/OMP. |
| `herdr_fs_read` | Read a file inside a managed git project on the workstation. |
| `herdr_fs_list` | List a directory under a managed root (skips `.git` / secret-ish names). |
| `herdr_fs_grep` | Search file contents under a managed root (`rg` when available). |
| `herdr_fs_write` | Create / overwrite a file (`overwrite:true` required to replace; dirty / busy gates). |
| `herdr_fs_edit` | Exact unique string replace in a file (same gates as write). |
| `herdr_fs_patch` | coding-tools-style `*** Begin Patch` multi-file patch (`dry_run`). |
| `herdr_fs_image` | Read an image under a managed root; return MCP image content. |
| `herdr_git` | Deterministic `status` / `diff` / `log` (do not spend a local agent on this). |
| `herdr_exec` | Short shell in the workspace’s visible `herdr-mcp:utility` pane. If control-plane TaskGroup blocks pane ops **before** the command is sent, falls back to a local zsh (`backend:local_fallback`) — never double-runs after delivery. |
| `herdr_exec_start` / `read` / `kill` | Long background shell sessions (local process, not the utility pane). |

Optional: `HERDR_MCP_ALL_TOOLS=1` adds advanced/deprecated lifecycle tools (30 total). Mutations stay in managed git roots; `HERDR_MCP_READONLY=1` / `HERDR_MCP_WRITE_ROOTS=/a,/b` tighten writes.

## Environment

| Variable | Default | Role |
|---|---|---|
| `HERDR_MCP_TOKEN` | empty | Static Bearer for `/mcp` and `/push` (Cursor / curl / extension). |
| `HERDR_MCP_PORT` | `8772` | Listen port. |
| `HERDR_MCP_BASE_URL` | empty | Public origin for ChatGPT, **no** `/mcp` suffix. Required for OAuth `iss`/`aud`. |
| `HERDR_SOCKET_PATH` | `~/.config/herdr/herdr.sock` | Herdr API socket. |
| `HERDR_MCP_READONLY` | off | Block mutations including `herdr_prompt` (`herdr_fs_patch` `dry_run` still allowed). |
| `HERDR_MCP_WRITE_ROOTS` | all managed roots | CSV of roots that may be written. |
| `HERDR_MCP_ALL_TOOLS` | off | Register 30 tools instead of 18. |
| `HERDR_MCP_AGENT_ALLOW` | workers + auditors | `*` shows Claude/OMP/Codex in inspect/since; comma list overrides. |
| `HERDR_SKILL_NETWORK` | on | `0` = bundled `SKILL.md` only. |

More OAuth / skill / state dirs: [docs/architecture.md](docs/architecture.md#environment-variables).

## Trust

A connected ChatGPT session can read and write files under managed git roots and run shell via `herdr_exec`. The extension uses the same static token on localhost; do not put that token in the ChatGPT connector form. Secret-path checks apply to `herdr_fs_*` only — a shell can still `cat .env`. Use `HERDR_MCP_READONLY` / `HERDR_MCP_WRITE_ROOTS` to tighten.

## Browser extension

Folder: `extension/` (MV3, Chrome name **herdr → Web wake**). Load unpacked in `chrome://extensions`. Sites: chatgpt.com, claude.ai, chat.deepseek.com, chat.z.ai.

Two jobs ([docs/extension.md](docs/extension.md), 中文) — they share the local token; they are **not** equally finished:

1. **Progress nudge (shipping)** — bind the current chat to a herdr **workspace**; any agent in that space with new output / settle can type into the chat; full settle only when none remain working. ChatGPT in-page “Allow” cards are auto-clicked. Optional: after a ChatGPT turn, a configured small model may submit a continue message (extension ≥0.1.20).
2. **JSON → MCP (incomplete)** — on DeepSeek / z.ai the content script can parse assistant `{"tool":...}`. It does **not** yet call local `/mcp` or paste results back. Plan: [docs/extension-bridge.md](docs/extension-bridge.md).

Same local `127.0.0.1:8772` token. Not a substitute for ChatGPT’s OAuth connector. Defaults: progress check every **60s**, unchanged-summary fallback **20 min** (`progressTickSec` / `progressFallbackSec`).

## Docs

| Doc | Topic |
|---|---|
| [CHANGELOG.md](CHANGELOG.md) | Version / tool-surface changes |
| [docs/architecture.md](docs/architecture.md) | herdr vs MCP layers, gates, env (中文) |
| [docs/chatgpt-connector.md](docs/chatgpt-connector.md) | ChatGPT OAuth / wire / schema / permission cards (中文) |
| [docs/extension.md](docs/extension.md) | Extension overview (中文) |
| [docs/extension-wake.md](docs/extension-wake.md) | Track A: progress nudge (中文) |
| [docs/extension-bridge.md](docs/extension-bridge.md) | Track B: JSON→MCP, not finished (中文) |
| [tests/README.md](tests/README.md) | Default vs manual tests |

Process notes live in `docs/_wip/` (gitignored).

## Ops

```bash
npx tsc          # rebuild after code changes
# restart whatever process runs node dist/server.js
```

Sessions: `~/.config/herdr-mcp/sessions/`.
