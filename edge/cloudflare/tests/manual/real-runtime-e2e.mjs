#!/usr/bin/env node
import assert from "node:assert/strict";
import { HerdrLink } from "../../../../dist/link/client.js";
import { LocalMcpRuntimeTransport } from "../../../../dist/link/local-mcp-transport.js";

const EDGE_HTTP = process.env.EDGE_URL ?? "http://127.0.0.1:8787";
const EDGE_WS = EDGE_HTTP.replace(/^http/, "ws") + "/ws";
const WORKSTATION_ID = process.env.DEMO_WORKSTATION_ID ?? "dev-real-runtime";
const LINK_TOKEN = process.env.LINK_SHARED_SECRET;
const DEV_MCP_BEARER = process.env.DEV_MCP_BEARER_SECRET;
const RUNTIME_TOKEN = process.env.HERDR_MCP_TOKEN;
const CONTRACT_HASH = "sha256:3f23083ae31b977dad21b1ec9d6919c49e1067a27f7b7eea7bdd021b54770c0d";

if (!LINK_TOKEN) throw new Error("LINK_SHARED_SECRET is required");
if (!DEV_MCP_BEARER) throw new Error("DEV_MCP_BEARER_SECRET is required");
if (!RUNTIME_TOKEN) throw new Error("HERDR_MCP_TOKEN is required");

const transport = new LocalMcpRuntimeTransport({
  endpoint: "http://127.0.0.1:8772/mcp",
  bearerToken: RUNTIME_TOKEN,
  contractHash: CONTRACT_HASH,
  contractEpoch: 1,
  runtimeVersion: "0.3.23",
  runtimeCommit: "dev",
  runtimeGeneration: "live-0.3.23",
  herdrVersion: "0.8.2",
  herdrProtocol: "20",
  defaultTimeoutMs: 15_000,
  maxTimeoutMs: 60_000,
});

const health = await transport.getHealth();
assert.equal(health.healthy, true, `local runtime unhealthy: ${health.details ?? "unknown"}`);

const link = new HerdrLink({
  workstationId: WORKSTATION_ID,
  edgeUrl: EDGE_WS,
  linkToken: LINK_TOKEN,
  transport,
  heartbeatMs: 5_000,
  maxSilenceMs: 20_000,
  handshakeTimeoutMs: 5_000,
  requestTimeoutMs: 15_000,
  maxReconnectAttempts: 3,
});

const connectPromise = link.connect();
const deadline = Date.now() + 8_000;
while (link.getStatus().phase !== "online") {
  if (Date.now() > deadline) throw new Error(`link did not become online: ${JSON.stringify(link.getStatus())}`);
  await new Promise((resolve) => setTimeout(resolve, 50));
}

const callResp = await fetch(`${EDGE_HTTP}/mcp`, {
  method: "POST",
  headers: {
    "content-type": "application/json",
    "x-herdr-workstation": WORKSTATION_ID,
    authorization: `Bearer ${DEV_MCP_BEARER}`,
  },
  body: JSON.stringify({
    jsonrpc: "2.0",
    id: "real-runtime-e2e",
    method: "tools/call",
    params: { name: "herdr_inspect", arguments: {} },
  }),
});
assert.equal(callResp.status, 200);
const call = await callResp.json();
assert.equal(call.jsonrpc, "2.0");
assert.equal(call.id, "real-runtime-e2e");
assert.ok(call.result && Array.isArray(call.result.content), "missing MCP CallToolResult content");
const text = call.result.content.find((item) => item?.type === "text")?.text;
assert.equal(typeof text, "string");
const inspected = JSON.parse(text);
const serverVersion = inspected?.workstation_info?.server_version ?? inspected?.build?.server_version;
assert.equal(serverVersion, "0.3.23");
assert.equal(inspected?.build?.pid, 93113);

console.log("real runtime e2e OK", JSON.stringify({
  workstation: WORKSTATION_ID,
  runtime: serverVersion,
  edge: EDGE_HTTP,
  tool: "herdr_inspect",
}));

await link.close({ reason: "real-runtime-e2e-complete", drainMs: 2_000 });
await connectPromise;
