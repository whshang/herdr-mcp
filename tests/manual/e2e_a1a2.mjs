// End-to-end A-1/A-2 through the MCP server on a TEMP port (never 8772/launchd).
// A-1/A-2 semantics are advanced-surface: herdr_wait is only registered under
// HERDR_MCP_ALL_TOOLS=1 (the default 11-tool surface drops it), so spawn with it.
import { execSync, spawn } from "node:child_process";

const PORT = "9799";
const TOKEN = "testtoken";
const BASE = `http://127.0.0.1:${PORT}/mcp`;

// start temp server
const server = spawn("node", ["dist/server.js"], {
  env: { ...process.env, HERDR_MCP_PORT: PORT, HERDR_MCP_TOKEN: TOKEN, HERDR_MCP_ALL_TOOLS: "1" },
  stdio: "ignore",
});
await new Promise((r) => setTimeout(r, 2000));

let sessionId = null;
async function rpc(method, params = {}) {
  const headers = { Authorization: `Bearer ${TOKEN}`, "Content-Type": "application/json", Accept: "application/json, text/event-stream" };
  if (sessionId) headers["Mcp-Session-Id"] = sessionId;
  const res = await fetch(BASE, { method: "POST", headers, body: JSON.stringify({ jsonrpc: "2.0", id: Math.floor(Math.random() * 1e9), method, params }) });
  const sid = res.headers.get("mcp-session-id");
  if (sid) sessionId = sid;
  const text = await res.text();
  const datas = text.split("\n").filter((l) => l.startsWith("data: ")).map((l) => l.slice(6)).map((l) => JSON.parse(l));
  return datas[datas.length - 1];
}
async function tool(name, args) {
  const r = await rpc("tools/call", { name, arguments: args });
  if (r.error) throw new Error(`tools/call ${name}: ${JSON.stringify(r.error)}`);
  return JSON.parse(r.result?.content?.[0]?.text ?? "null");
}

let fail = 0;
function check(name, cond, extra = "") {
  console.log(`  ${cond ? "PASS" : "FAIL"}  ${name}${extra ? ` (${extra})` : ""}`);
  if (!cond) fail++;
}

try {
  await rpc("initialize", { protocolVersion: "2025-03-26", capabilities: {}, clientInfo: { name: "e2e", version: "1" } });
  await rpc("notifications/initialized", {});

  console.log("--- A-1 herdr_call validation ---");
  // invalid params: missing required
  const bad = await tool("herdr_call", { method: "agent.read", params: { target: "wH:p1" } });
  check("herdr_call invalid params -> invalid_params", bad.ok === false && bad.code === "invalid_params" && bad.errors?.some((e) => e.name === "source"), JSON.stringify(bad.errors));
  // valid read-only call passes through
  const good = await tool("herdr_call", { method: "session.snapshot", params: {} });
  check("herdr_call valid -> ok", good.ok === true);
  // unknown param -> warning but still ok
  const warn = await tool("herdr_call", { method: "session.snapshot", params: { bogus: 1 } });
  check("herdr_call unknown param -> warning", warn.ok === true && Array.isArray(warn.warnings) && warn.warnings.length > 0);

  console.log("--- A-2 herdr_inspect via SnapshotCache ---");
  // The cache needs a moment for events to flow; poll until activity timestamps
  // converge (the smoke test proves the pipeline; this asserts it over HTTP).
  let insp = await tool("herdr_inspect", {});
  const deadline = Date.now() + 20_000;
  while ((insp.agents ?? []).filter((a) => a.last_activity_at).length === 0 && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 1000));
    insp = await tool("herdr_inspect", {});
  }
  const withAct = (insp.agents ?? []).filter((a) => a.last_activity_at);
  const withStart = (insp.agents ?? []).filter((a) => a.started_at);
  check("inspect agents have last_activity_at", withAct.length > 0, `agents=${insp.agents?.length} withActivity=${withAct.length}`);
  check("inspect agents have started_at", withStart.length > 0, `withStarted=${withStart.length}`);
  console.log("   sample:", JSON.stringify((insp.agents ?? []).find((a) => a.last_activity_at) ?? (insp.agents ?? [])[0]));

  console.log("--- A-2 herdr_wait uses same cache source ---");
  // wait on a known working pane — should read cache (may return still_running, that's fine; key is no error + consistent source)
  const w = await tool("herdr_wait", { target: "wH:p3", timeout_ms: 1500 });
  check("herdr_wait executes (cache-backed)", w.ok === true || w.reason === "still_running", `ok=${w.ok} reason=${w.reason}`);
} finally {
  server.kill();
}

console.log(`\n=== E2E ${fail === 0 ? "ALL PASSED" : `FAILED (${fail})`} ===`);
process.exit(fail === 0 ? 0 : 1);
