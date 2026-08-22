/**
 * In-memory ring buffer of recent MCP tools/call events.
 * Used by the browser extension to detect ChatGPT turns that claim work
 * without ever hitting the local connector (talk-without-tools).
 */
export type McpToolCallRecord = {
  at: number;
  tool: string;
  call: string | null;
  ua: string;
  status: number;
};

const MAX_RECORDS = 2000;
const records: McpToolCallRecord[] = [];

/** Record one finished tools/call (call from the access-log middleware). */
export function recordMcpToolCall(rec: McpToolCallRecord): void {
  records.push(rec);
  if (records.length > MAX_RECORDS) {
    records.splice(0, records.length - MAX_RECORDS);
  }
}

export type McpActivityQuery = {
  since_ms: number;
  until_ms: number;
  /** Substring match on User-Agent (default: openai-mcp for ChatGPT connector). */
  ua_includes?: string;
};

export type McpActivityResult = {
  since: string;
  until: string;
  since_ms: number;
  until_ms: number;
  ua_includes: string | null;
  count: number;
  tools: { at: string; tool: string; call: string | null; ua: string; status: number }[];
};

export function queryMcpActivity(q: McpActivityQuery): McpActivityResult {
  const since = Number(q.since_ms);
  const until = Number(q.until_ms);
  const uaInc = q.ua_includes === undefined ? "openai-mcp" : q.ua_includes;
  const hits = records.filter((r) => {
    if (!Number.isFinite(since) || !Number.isFinite(until)) return false;
    if (r.at < since || r.at > until) return false;
    if (uaInc && !r.ua.toLowerCase().includes(uaInc.toLowerCase())) return false;
    return true;
  });
  return {
    since: new Date(since).toISOString(),
    until: new Date(until).toISOString(),
    since_ms: since,
    until_ms: until,
    ua_includes: uaInc || null,
    count: hits.length,
    tools: hits.slice(-50).map((r) => ({
      at: new Date(r.at).toISOString(),
      tool: r.tool,
      call: r.call,
      ua: r.ua,
      status: r.status,
    })),
  };
}

/** Test helper: clear the ring buffer. */
export function resetMcpActivityForTests(): void {
  records.length = 0;
}
