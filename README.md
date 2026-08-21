# herdr-mcp

Expose [herdr](https://herdr.dev) as an MCP server so remote clients (ChatGPT, Claude, Cursor) can control local panes and agents.

中文文档见 [README.zh.md](README.zh.md).

## Endpoints

| Use | URL |
|---|---|
| Local MCP | `http://127.0.0.1:8772/mcp` |
| Public MCP (recommended) | `https://<subdomain>.trycloudflare.com/mcp` |
| Browser extension push | `http://127.0.0.1:8772/push/events` |

**Default public path for others:** Cloudflare’s free Quick Tunnel (`*.trycloudflare.com`). No custom domain required. Set `HERDR_MCP_BASE_URL` to that HTTPS origin (no `/mcp` suffix) so OAuth discovery matches the URL ChatGPT/Claude use.

Auth for connectors: **OAuth (DCR)** — leave API key empty. Static Bearer is for local curl / Cursor only (`herdr-mcp token`).

## Connect

### 1. Public URL (free Cloudflare)

```bash
# terminal A — MCP server already running on :8772
cloudflared tunnel --url http://127.0.0.1:8772
# → https://xxxx.trycloudflare.com

# put the origin into LaunchAgent / env (no /mcp):
# HERDR_MCP_BASE_URL=https://xxxx.trycloudflare.com
herdr-mcp restart
herdr-mcp connector   # prints …/mcp for ChatGPT & Claude
```

Quick Tunnel URLs change when you restart `cloudflared`. For a stable hostname, use a named Cloudflare tunnel (still free) or your own domain — optional, not required.

### 2. ChatGPT / Claude

1. MCP URL: `https://xxxx.trycloudflare.com/mcp` (from `herdr-mcp connector`)
2. OAuth — do **not** paste a token
3. Start a **new** chat after connecting (old chats keep a stale tool snapshot)

Hard-won ChatGPT requirements (OAuth, stateless transport, schema pitfalls): [docs/chatgpt-connector.md](docs/chatgpt-connector.md).

### 3. Cursor (this machine)

`~/.cursor/mcp.json` — local only (do not also enable the public URL in the same profile; Cursor dedupes identical tool surfaces):

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

## CLI

```bash
herdr-mcp              # menu
herdr-mcp status
herdr-mcp connector
herdr-mcp start | stop | restart
herdr-mcp logs [-f]
herdr-mcp token | url
```

## Default tools (why these 11)

herdr’s native surface is a large Unix-socket API (`herdr api schema`, ~90 methods). herdr-mcp does **not** re-wrap every method as an MCP tool (that burns context and duplicates herdr). Instead:

| Layer | MCP tools | Relation to herdr |
|---|---|---|
| Passthrough | `herdr_methods`, `herdr_call` | Thin gate to the **native** socket API. Discover with `herdr_methods`, then call any method via `herdr_call`. |
| Remote orchestration | `herdr_inspect`, `herdr_since`, `herdr_prompt` | Small helpers for web clients that only run when the user sends a message (no local polling loop). Built on snapshot/events/`agent.prompt` — not new herdr features. |
| Remote workstation | `herdr_fs_*`, `herdr_exec` | **Not** herdr tools. The MCP client runs off-machine and cannot see your disk; these fill that gap under managed git roots. |

| Tool | What it does |
|---|---|
| `herdr_methods` | List live herdr socket methods + parameter schemas (cached reflection). Use before unknown `herdr_call`s. |
| `herdr_call` | Call any herdr method with validated params (`{ method, params }`). Covers panes, workspaces, agents, etc. without one MCP tool per method. |
| `herdr_inspect` | One-shot: connection health + workspaces / tabs / panes / agents (cwd, status). Usual first call. |
| `herdr_since` | Cheap digest since a cursor — resume a conversation without re-dumping full state. |
| `herdr_prompt` | Deliver a prompt to a herdr agent via socket `agent.prompt` (not typing into the pane). Prefer with `idempotency_key`. |
| `herdr_fs_read` | Read a file inside a managed git project on the workstation. |
| `herdr_fs_list` | List a directory under a managed root (skips `.git` / secret-ish names). |
| `herdr_fs_grep` | Search file contents under a managed root (`rg` when available). |
| `herdr_fs_write` | Create / overwrite a file (dirty / busy gates; `confirm_dirty` / `confirm_busy`). |
| `herdr_fs_edit` | Exact unique string replace in a file (same gates as write). |
| `herdr_exec` | Run a shell command in the workspace’s visible `herdr-mcp:utility` pane (observable, not headless). |

Optional: `HERDR_MCP_ALL_TOOLS=1` adds advanced/deprecated lifecycle tools. Mutations stay in managed git roots; `HERDR_MCP_READONLY=1` / `HERDR_MCP_WRITE_ROOTS=/a,/b` tighten writes.

## Browser extension

Folder: `extension/` (MV3). Load unpacked in `chrome://extensions`.

**Wake only** — when a herdr agent settles, write into a bound web chat (`/push/events`). Sites: z.ai, deepseek, claude.ai, chatgpt.com.

This is **not** the ChatGPT MCP connector. z.ai / DeepSeek do not receive the 11 MCP tools via the extension. Details: [docs/extension-wake.md](docs/extension-wake.md).

## Docs

| Doc | Topic |
|---|---|
| [docs/architecture.md](docs/architecture.md) | herdr vs MCP tool layers |
| [docs/chatgpt-connector.md](docs/chatgpt-connector.md) | ChatGPT OAuth + wire + schema |
| [docs/extension-wake.md](docs/extension-wake.md) | Browser wake extension |
| [tests/README.md](tests/README.md) | Default vs manual tests |

Process notes live in `docs/_wip/` (gitignored).

## Ops

```bash
npx tsc && herdr-mcp restart
herdr-mcp logs -f
```

LaunchAgent: `dev.herdr-mcp.server`. Sessions: `~/.config/herdr-mcp/sessions/`.
