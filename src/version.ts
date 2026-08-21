/**
 * herdr-mcp server version — single source of truth for the MCP identity.
 *
 * Bumped together with package.json so ChatGPT / OpenAI connector clients
 * see a NEW serverInfo.version / mcp.json version and re-run tools/list,
 * which is the client-side tool-registry cache-invalidation signal.
 * (The previous 0.2.0 was constant across the 22->9 tool surface change, so
 * cached clients never refreshed; see MCP tool-registry cache-invalidation.)
 */
export const SERVER_VERSION = "0.3.0";
export const SERVER_NAME = "herdr-mcp";
