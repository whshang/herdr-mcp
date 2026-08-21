# herdr-mcp

Expose [herdr](https://herdr.dev) as an MCP server so remote clients (ChatGPT, Claude, Cursor) can control local panes and agents.

中文文档见 [README.zh.md](README.zh.md).

## Endpoints

| Use | URL |
|---|---|
| Public MCP (ChatGPT / Claude) | `https://xxxx.trycloudflare.com/mcp` |
| Local MCP | `http://127.0.0.1:8772/mcp` |
| Browser extension push | `http://127.0.0.1:8772/push/events` |

Auth for connectors: **OAuth (DCR)** — leave API key empty. Static Bearer is for local curl / Cursor only (`herdr-mcp token`).

## Connect

### ChatGPT / Claude

1. Add connector with MCP URL: `https://xxxx.trycloudflare.com/mcp`
2. OAuth — do **not** paste a token
3. Start a **new** chat after connecting

```bash
herdr-mcp connector   # prints the same URL
```

### Cursor (this machine)

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

Remote / another machine: same public `/mcp` URL with Bearer, or OAuth where the client supports it.

## CLI

```bash
herdr-mcp              # menu
herdr-mcp status
herdr-mcp connector
herdr-mcp start | stop | restart
herdr-mcp logs [-f]
herdr-mcp token | url
```

## Default tools

`herdr_methods` · `herdr_inspect` · `herdr_call` · `herdr_since` · `herdr_prompt` · `herdr_fs_read` · `herdr_fs_list` · `herdr_fs_grep` · `herdr_fs_write` · `herdr_fs_edit` · `herdr_exec`

Mutations stay inside managed git roots. Optional: `HERDR_MCP_READONLY=1`, `HERDR_MCP_WRITE_ROOTS=/a,/b`.

## Browser extension

Folder: `extension/` (MV3). Load unpacked in `chrome://extensions`.

Wakes a bound web chat when a herdr agent settles (`/push/events`). Configure URL + token in the extension options. Sites: z.ai, deepseek, claude.ai, chatgpt.com.

## Ops

```bash
npx tsc && herdr-mcp restart
herdr-mcp logs -f
```

LaunchAgent: `dev.herdr-mcp.server`. Sessions: `~/.config/herdr-mcp/sessions/`.
