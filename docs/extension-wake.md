# Browser extension — wake web chats (not MCP)

Audience: anyone loading `extension/` or expecting z.ai / DeepSeek to “have MCP tools”.

## What it is

MV3 extension **herdr → Web wake**: when a bound herdr agent settles, push a message into a bound web chat input and submit.

Sites with injectors: `chat.z.ai`, `chat.deepseek.com`, `claude.ai`, `chatgpt.com`.

SpeaksJSON (`content/webmcp/speaks-json.js`) on z.ai / DeepSeek parses assistant output for delivery confirmation and a **future** reverse path (page → herdr). It does **not** register herdr-mcp tools inside those sites.

## What it is not

| Expectation | Reality |
|---|---|
| Install extension → DeepSeek/z.ai get the same 11 MCP tools as ChatGPT Connector | **No.** Those sites have no ChatGPT-style MCP OAuth connector in this project. |
| Extension “fixes” from server OAuth / schema work apply automatically | **No.** Server MCP handshake ≠ extension wake path. |
| Public Cloudflare URL required for the extension | **No.** Extension talks to **local** `http://127.0.0.1:8772` (`/push/events`) with the static token. |

To **schedule herdr from ChatGPT**, use the MCP connector ([chatgpt-connector.md](./chatgpt-connector.md)).  
To **nudge z.ai / DeepSeek after a herdr agent finishes**, use this extension.

## Setup

1. `chrome://extensions` → Load unpacked → `extension/`
2. Options: URL `http://127.0.0.1:8772`, token from `herdr-mcp token`
3. Open the target chat tab; bind agent ↔ conversation in the popup
4. herdr-mcp must be running (LaunchAgent)

## If you want MCP *inside* z.ai / DeepSeek

That needs one of:

1. The site itself grows an MCP/connector feature (then reuse herdr-mcp OAuth like ChatGPT), or
2. A new extension bridge that turns page tool-calls into `POST /mcp` (SpeaksJSON is a seed, not shipped product)

Neither is the current default path. Do not pretend wake == MCP.

## Tests

- Static / unit: `node tests/manual/extension_smoke.mjs`
- Bind logic: `node tests/manual/background_bind_test.mjs`
- Push plumbing: `node tests/manual/push_sse.mjs`
