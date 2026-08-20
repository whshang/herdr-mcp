# src/server.ts — Node.js rewrite of herdr_mcp/server.py

## Goal
Replace the Python FastMCP server (herdr_mcp/server.py) with a TypeScript
implementation in src/server.ts built on @modelcontextprotocol/sdk + express,
deployable at dist/server.js (binary `herdr-mcp`).

## Locked decisions
- 7 MCP tools with identical semantics to Python: herdr_inspect, herdr_call,
  herdr_wait, herdr_session, herdr_handoff, herdr_parallel, herdr_reap.
- Express HTTP server on HERDR_MCP_PORT (default 8772).
- OAuth DCR endpoints: /.well-known/oauth-authorization-server,
  /oauth/register, /oauth/authorize (auto-redirect), /oauth/token
  (form-urlencoded), /.well-known/mcp.json.
- Bearer auth on /mcp via HERDR_MCP_TOKEN.
- Do NOT touch Python files.
- Read src/herdr.ts (HerdrClient) and replicate tool logic from server.py.

## Steps (DONE)
1. ✅ Read herdr.ts, session.ts, wait.ts, server.py, session.py, wait.py.
2. ✅ Wrote src/server.ts (McpServer + StreamableHTTPServerTransport, stateful
   sessions; 7 tools calling HerdrClient; OAuth DCR routes; Bearer auth on /mcp).
3. ✅ Express routes wired on HERDR_MCP_PORT (default 8772).
4. ✅ Added "lib": ["ES2024"] to tsconfig for Promise.withResolvers in herdr.ts.
5. ✅ npx tsc passes (exit 0).
6. ✅ curl tests: POST /mcp initialize (200, SSE), notifications/initialized (202),
   tools/list (7 tools), tools/call herdr_inspect + herdr_call(real socket), 401 on
   bad/missing token; OAuth well-known/register (201)/authorize (302)/token (200,
   form-urlencoded + refresh).

## Verified evidence
- initialize returns protocolVersion + serverInfo herdr-mcp 0.2.0.
- herdr_inspect returns ok, herdr_version 0.8.0, protocol 19, workspaces/tabs/panes/agents
  live from the real herdr socket — matches Python _project_snapshot schema.
- herdr_call ping returns {ok:true, result:{type:'pong', version:0.8.0, protocol:19}}.
- OAuth token exchange returns HERDR_MCP_TOKEN bearer exactly like Python.
- Python deployment untouched (still owns :8772; Node tested on :9788).
