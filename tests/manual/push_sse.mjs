#!/usr/bin/env node
/**
 * /push endpoint verification (阶段 1).
 *
 * 1. Spawns dist/server.js on a test port with a test token.
 * 2. Checks auth: /push/* → 401 without token, 200 with.
 * 3. GET /push/state → { agents, server_time }.
 * 4. GET /push/events (SSE via fetch ReadableStream) → hello event + keepalive.
 * 5. --integration: drives a REAL herdr agent (scratch workspace wH-push-test,
 *    kind=pi) through working → settled and asserts an `agent_settled` SSE event
 *    fires; then closes the workspace.
 *
 * Usage:
 *   node tests/manual/push_sse.mjs            # plumbing only (no herdr agent)
 *   node tests/manual/push_sse.mjs --integration
 */
import { spawn } from "node:child_process";
import { once } from "node:events";
import { fileURLToPath } from "node:url";
import * as path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileP = promisify(execFile);
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PORT = 8799;
const TOKEN = "push-test-token";
const BASE = `http://127.0.0.1:${PORT}`;

let server = null;
let failures = 0;

function ok(cond, label, detail = "") {
  if (cond) console.log(`  ✅ ${label}`);
  else { failures++; console.error(`  ❌ ${label} ${detail}`); }
}

async function startServer() {
  server = spawn("node", [path.join(__dirname, "..", "..", "dist", "server.js")], {
    env: { ...process.env, HERDR_MCP_PORT: String(PORT), HERDR_MCP_TOKEN: TOKEN },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let out = "";
  server.stdout.on("data", (d) => { out += d; if (process.env.SERVER_OUTPUT) process.stdout.write(d); });
  server.stderr.on("data", (d) => { out += d; if (process.env.SERVER_OUTPUT) process.stdout.write(d); });
  const deadline = Date.now() + 8000;
  while (Date.now() < deadline) {
    if (out.includes("listening on")) return;
    if (server.exitCode !== null) throw new Error(`server exited ${server.exitCode}: ${out}`);
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("server did not start in time");
}

async function fetchJson(url, token) {
  const r = await fetch(url, { headers: token ? { Authorization: `Bearer ${token}` } : {} });
  const body = await r.text();
  return { status: r.status, body };
}

/** Read SSE stream until a predicate matches (or timeout). Returns matched event data. */
async function readSseUntil(url, token, predicate, { timeoutMs = 30000 } = {}) {
  const r = await fetch(url, { headers: token ? { Authorization: `Bearer ${token}` } : {} });
  if (!r.ok || !r.body) throw new Error(`SSE fetch failed: ${r.status}`);
  const reader = r.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  const deadline = Date.now() + timeoutMs;
  const seen = [];
  while (Date.now() < deadline) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    let idx;
    while ((idx = buf.indexOf("\n\n")) >= 0) {
      const block = buf.slice(0, idx); buf = buf.slice(idx + 2);
      const evLine = block.split("\n").find((l) => l.startsWith("event:"));
      const dataLine = block.split("\n").find((l) => l.startsWith("data:"));
      const event = evLine ? evLine.slice(6).trim() : null;
      const data = dataLine ? JSON.parse(dataLine.slice(5).trim()) : null;
      seen.push({ event, data });
      if (predicate({ event, data })) return { seen, match: { event, data } };
    }
  }
  return { seen, match: null };
}

async function integrationAgentFlow() {
  console.log("\n[integration] driving a real herdr agent (scratch workspace)…");
  const label = `wH-push-test-${Date.now().toString(36)}`;
  // Create a scratch workspace; the CLI prints one-line JSON envelopes.
  const ws = await execFileP("herdr", ["workspace", "create", "--label", label]);
  let wsId = null;
  try {
    const env = JSON.parse((ws.stdout || "").trim());
    wsId = env?.result?.workspace?.workspace_id ?? env?.result?.workspace_id ?? null;
  } catch { /* fall through */ }
  console.log("  workspace create:", (ws.stdout || ws.stderr || "").trim().slice(0, 160));
  // Poll workspace list until the new workspace is visible, then grab its pane.
  let paneId = null;
  for (let i = 0; i < 20 && !paneId; i++) {
    await new Promise((r) => setTimeout(r, 500));
    try {
      const { stdout: wsList } = await execFileP("herdr", ["workspace", "list"]);
      const wsEnv = JSON.parse(wsList.trim());
      const wsRec = (wsEnv?.result?.workspaces ?? []).find((w) => w.label === label);
      if (!wsRec) continue;
      wsId = wsRec.workspace_id;
      const { stdout: paneOut } = await execFileP("herdr", ["pane", "list"]);
      const paneEnv = JSON.parse(paneOut.trim());
      const paneRec = (paneEnv?.result?.panes ?? []).find((p) => p.workspace_id === wsId);
      if (paneRec) paneId = paneRec.pane_id;
    } catch { /* retry */ }
  }
  ok(!!paneId, "found a pane_id for the scratch workspace", `wsId=${wsId}`);
  if (!paneId) return;

  // Start a pi agent in that pane (30s readiness timeout) and watch the pane-scoped SSE.
  const settleP = readSseUntil(`${BASE}/push/events?pane=${paneId}`, TOKEN, (m) => m.event === "agent_settled" && m.data?.pane === paneId, { timeoutMs: 120000 });
  try {
    const start = await execFileP("herdr", ["agent", "start", "push-test", "--kind", "pi", "--pane", paneId, "--timeout", "30000"]);
    console.log("  agent start:", (start.stdout || start.stderr || "").trim().slice(0, 200));
  } catch (e) {
    console.error("  agent start failed (is a pi agent runnable in this herdr?):", e.message);
    console.error("  — skipping integration settle assertion, plumbing still verified.");
    return;
  }

  // Prompt it to do a tiny job and finish.
  await execFileP("herdr", ["agent", "prompt", "push-test", "Reply with exactly: DONE-PUSH-TEST"]);
  console.log("  prompt sent; waiting for working→settled SSE event…");
  const settle = await settleP;
  if (settle.match) {
    ok(true, `agent_settled fired (status=${settle.match.data.status}, agent=${settle.match.data.agent})`);
    console.log("    data:", JSON.stringify({ agent: settle.match.data.agent, pane: settle.match.data.pane, status: settle.match.data.status, workspace: settle.match.data.workspace, output_len: (settle.match.data.output || "").length }));
  } else {
    ok(false, "agent_settled never fired within 120s", `seen=${settle.seen.map((s) => s.event).join(",")}`);
  }
  return label;
}

async function main() {
  const integration = process.argv.includes("--integration");
  console.log("=== /push endpoint verification ===");
  await startServer();
  console.log(`  server on :${PORT} (token=${TOKEN})`);

  // 1. auth
  let r = await fetchJson(`${BASE}/push/state`, "");
  ok(r.status === 401, "no token → 401", `got ${r.status}`);
  r = await fetchJson(`${BASE}/push/events`, "");
  ok(r.status === 401, "no token SSE → 401", `got ${r.status}`);

  // 2. state
  r = await fetchJson(`${BASE}/push/state`, TOKEN);
  ok(r.status === 200, "state → 200", `got ${r.status}`);
  let st = JSON.parse(r.body);
  ok(Array.isArray(st.agents), "state.agents is an array", `keys=${Object.keys(st).join(",")}`);

  // 3. SSE hello + keepalive
  const helloP = readSseUntil(`${BASE}/push/events?agent=push-test`, TOKEN, (m) => m.event === "hello");
  const hello = await helloP;
  ok(!!hello.match, "SSE hello received");
  if (hello.match) {
    ok(hello.match.data.protocol === "herdr-mcp-push/v1", "hello.protocol correct");
    ok(Array.isArray(hello.match.data.agents), "hello.agents is an array");
    ok(hello.match.data.filters.agent === "push-test", "hello.filters echoes ?agent=");
  }
  // keepalive within ~16s
  const ka = await readSseUntil(`${BASE}/push/events`, TOKEN, (m) => m.event === null && m.data === null, { timeoutMs: 18000 });
  ok(!!ka.match, "keepalive comment within 18s");

  // 4. optional real transition
  if (integration) {
    const label = await integrationAgentFlow();
    // cleanup
    try {
      const { stdout: wsList } = await execFileP("herdr", ["workspace", "list"]);
      const wsEnv = JSON.parse(wsList.trim());
      const recs = (wsEnv?.result?.workspaces ?? []).filter((w) => w.label === label);
      for (const rec of recs) {
        await execFileP("herdr", ["workspace", "close", rec.workspace_id]);
      }
      console.log(`  scratch workspace closed (${recs.map((r) => r.workspace_id).join(",") || "none left"})`);
    } catch (e) { console.error("  cleanup close failed:", e.message); }
  }

  server.kill();
  await once(server, "exit").catch(() => {});
  console.log(`\n=== ${failures === 0 ? "ALL PASS" : failures + " FAILURES"} ===`);
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((e) => { console.error(e); process.exit(1); });
