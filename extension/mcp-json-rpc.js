// mcp-json-rpc.js — small stateless JSON-RPC client for the browser JSON bridge.
// The local Herdr MCP endpoint accepts sessionless tools/list and tools/call requests,
// so the extension does not need to emulate a Connector session.

export function parseMcpJsonResponseText(text) {
  const trimmed = String(text || "").trim();
  if (!trimmed) return null;

  // Streamable HTTP may return SSE for tools/list. Parse complete SSE events and
  // return the last JSON payload; tools/call normally returns application/json.
  const events = trimmed.split(/\r?\n\r?\n/);
  let parsed = null;
  for (const event of events) {
    const dataLines = event
      .split(/\r?\n/)
      .filter((line) => /^data:\s?/.test(line))
      .map((line) => line.replace(/^data:\s?/, ""));
    if (!dataLines.length) continue;
    const payload = dataLines.join("\n").trim();
    if (!payload || payload === "[DONE]") continue;
    try { parsed = JSON.parse(payload); } catch (_) {}
  }
  if (parsed !== null) return parsed;
  return JSON.parse(trimmed);
}

export async function callMcpJsonRpc({
  baseUrl,
  token,
  method,
  params = {},
  timeoutMs = 90000,
  fetchFn = globalThis.fetch,
  requestId = `browser-json-${Date.now()}`,
}) {
  const base = String(baseUrl || "").trim().replace(/\/+$/, "");
  const bearer = String(token || "").trim();
  if (!base) return { ok: false, error: "mcp-url-missing" };
  if (typeof fetchFn !== "function") return { ok: false, error: "fetch-unavailable" };

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    let response;
    try {
      response = await fetchFn(`${base}/mcp`, {
        method: "POST",
        headers: {
          ...(bearer ? { Authorization: `Bearer ${bearer}` } : {}),
          "Content-Type": "application/json",
          Accept: "application/json, text/event-stream",
          "Mcp-Protocol-Version": "2025-11-25",
          "X-Herdr-Client": "browser-json-bridge/1",
        },
        body: JSON.stringify({ jsonrpc: "2.0", id: requestId, method, params }),
        signal: controller.signal,
      });
    } catch (e) {
      return {
        ok: false,
        error: controller.signal.aborted ? "mcp-timeout" : "mcp-unreachable",
        detail: String(e?.message || e || ""),
      };
    }

    const text = await response.text();
    let payload = null;
    try { payload = parseMcpJsonResponseText(text); } catch (e) {
      return { ok: false, error: "mcp-malformed-response", status: response.status, detail: String(e?.message || e) };
    }
    if (!response.ok) {
      return {
        ok: false,
        error: `mcp-http-${response.status}`,
        status: response.status,
        detail: payload?.error?.message || "",
      };
    }
    if (payload?.error) {
      return {
        ok: false,
        error: `mcp-rpc-${payload.error.code ?? "error"}`,
        detail: String(payload.error.message || ""),
        data: payload.error.data,
      };
    }
    return { ok: true, result: payload?.result ?? null };
  } finally {
    clearTimeout(timer);
  }
}
