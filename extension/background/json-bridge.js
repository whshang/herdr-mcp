// json-bridge.js — browser JSON bridge transport (z.ai / DeepSeek without an MCP
// Connector). The page only receives tool schemas and results. The bearer token
// stays inside the extension service worker; the caller passes its mcpBaseUrl in
// explicitly so this module owns no configuration state.
import { localHerdrBatchFetch, localHerdrFetch } from "../local-auth.js";
import { callMcpJsonRpc, parseMcpJsonResponseText } from "../mcp-json-rpc.js";

const JSON_BRIDGE_MAX_BATCH_CALLS = 24;
const JSON_BRIDGE_MAX_PARALLEL = 4;
const JSON_BRIDGE_NATIVE_BATCH_REPROBE_MS = 60_000;
let jsonBridgeNativeBatchUnsupportedUntil = 0;
export async function jsonBridgeRpc(mcpBaseUrl, method, params = {}, nativeTimeoutMs = 90_000) {
  return callMcpJsonRpc({
    baseUrl: mcpBaseUrl,
    method,
    params,
    fetchFn: (url, init) => localHerdrFetch(url, { ...init, nativeTimeoutMs }),
  });
}

async function jsonBridgeNativeBatch(mcpBaseUrl, calls, nativeTimeoutMs = 90_000) {
  if (Date.now() < jsonBridgeNativeBatchUnsupportedUntil) {
    return { ok: false, error: "unsupported_message" };
  }
  const base = String(mcpBaseUrl || "").trim().replace(/\/+$/, "");
  if (!base) return { ok: false, error: "mcp-url-missing" };
  const prefix = `browser-json-batch-${Date.now()}`;
  const requests = calls.map((call, index) => ({
    input: `${base}/mcp`,
    init: {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Accept: "application/json, text/event-stream",
        "Mcp-Protocol-Version": "2025-11-25",
        "X-Herdr-Client": "browser-json-bridge/1",
      },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: `${prefix}-${index}`,
        method: "tools/call",
        params: { name: call.tool, arguments: call.args },
      }),
      nativeTimeoutMs,
    },
  }));
  const batch = await localHerdrBatchFetch(requests);
  if (!batch?.ok) {
    if (batch?.error === "unsupported_message") {
      jsonBridgeNativeBatchUnsupportedUntil = Date.now() + JSON_BRIDGE_NATIVE_BATCH_REPROBE_MS;
    }
    return batch || { ok: false, error: "native-host-request-batch-failed" };
  }
  jsonBridgeNativeBatchUnsupportedUntil = 0;
  const responses = [];
  for (const item of batch.responses) {
    if (!item?.ok || !item.response) {
      responses.push(item || { ok: false, error: "native-host-request-failed" });
      continue;
    }
    const response = item.response;
    const text = await response.text();
    let payload = null;
    try {
      payload = parseMcpJsonResponseText(text);
    } catch (error) {
      responses.push({ ok: false, error: "mcp-malformed-response", status: response.status, detail: String(error?.message || error) });
      continue;
    }
    if (!response.ok) {
      responses.push({ ok: false, error: `mcp-http-${response.status}`, status: response.status, detail: payload?.error?.message || "" });
      continue;
    }
    if (payload?.error) {
      responses.push({
        ok: false,
        error: `mcp-rpc-${payload.error.code ?? "error"}`,
        detail: String(payload.error.message || ""),
        data: payload.error.data,
      });
      continue;
    }
    responses.push({ ok: true, result: payload?.result ?? null });
  }
  return { ok: true, responses };
}

export function normalizeJsonBridgeBatch(calls) {
  if (!Array.isArray(calls) || calls.length === 0) {
    return { ok: false, error: "json-bridge-batch-empty" };
  }
  if (calls.length > JSON_BRIDGE_MAX_BATCH_CALLS) {
    return { ok: false, error: "json-bridge-batch-too-large" };
  }
  const normalized = [];
  for (const call of calls) {
    const tool = String(call?.tool || "").trim();
    const args = call?.args;
    if (!tool || tool.length > 128 || !args || typeof args !== "object" || Array.isArray(args)) {
      return { ok: false, error: "json-bridge-batch-invalid" };
    }
    normalized.push({ tool, args });
  }
  return { ok: true, calls: normalized };
}

const JSON_BRIDGE_DEDUPE_READ_TOOLS = new Set([
  "herdr_inspect",
  "herdr_since",
  "herdr_fs_read",
  "herdr_fs_list",
  "herdr_fs_grep",
  "herdr_fs_image",
  "herdr_methods",
  "herdr_skill",
]);

export async function runJsonBridgeBatch(mcpBaseUrl, calls) {
  const responses = new Array(calls.length);
  const uniqueCalls = [];
  const indexesByKey = new Map();
  let previousRead = null;
  calls.forEach((call, index) => {
    const dedupe = JSON_BRIDGE_DEDUPE_READ_TOOLS.has(call.tool);
    const serialized = dedupe ? `${call.tool}\n${JSON.stringify(call.args)}` : null;
    if (dedupe && previousRead?.serialized === serialized) {
      indexesByKey.get(previousRead.key)?.push(index);
      return;
    }
    const key = `${index}\n${call.tool}`;
    indexesByKey.set(key, [index]);
    uniqueCalls.push({ key, call });
    previousRead = dedupe ? { serialized, key } : null;
  });
  const nativeBatch = await jsonBridgeNativeBatch(mcpBaseUrl, uniqueCalls.map(({ call }) => call));
  if (nativeBatch?.ok && Array.isArray(nativeBatch.responses)) {
    uniqueCalls.forEach(({ key }, uniqueIndex) => {
      const response = nativeBatch.responses[uniqueIndex];
      for (const index of indexesByKey.get(key) || []) responses[index] = response;
    });
    return responses;
  }
  const fallbackAllowed = ["unsupported_message", "native_message_too_large"].includes(String(nativeBatch?.error || ""));
  if (!fallbackAllowed) {
    const failure = nativeBatch || { ok: false, error: "native-host-request-batch-failed" };
    uniqueCalls.forEach(({ key }) => {
      for (const index of indexesByKey.get(key) || []) responses[index] = failure;
    });
    return responses;
  }
  for (let offset = 0; offset < uniqueCalls.length; offset += JSON_BRIDGE_MAX_PARALLEL) {
    const chunk = uniqueCalls.slice(offset, offset + JSON_BRIDGE_MAX_PARALLEL);
    const chunkResponses = await Promise.all(chunk.map(async ({ call }) => {
      const result = await jsonBridgeRpc(mcpBaseUrl, "tools/call", {
        name: call.tool,
        arguments: call.args,
      });
      return result.ok ? { ok: true, result: result.result } : result;
    }));
    chunk.forEach(({ key }, chunkIndex) => {
      const response = chunkResponses[chunkIndex];
      for (const index of indexesByKey.get(key) || []) responses[index] = response;
    });
  }
  return responses;
}
