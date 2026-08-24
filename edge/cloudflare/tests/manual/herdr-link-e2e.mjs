#!/usr/bin/env node
/**
 * Local-only end-to-end smoke using the real HerdrLink client.
 *
 * Requires:
 *   npm run build
 *   npx wrangler dev --config edge/cloudflare/wrangler.toml --port 8787 --local \
 *     --var LINK_SHARED_SECRET:dev-only-link-secret-change-me
 *
 * Exercises:
 *   Worker -> Durable Object -> real HerdrLink WSS -> injected fake runtime
 *   -> canonical tool_result -> Durable Object -> public MCP response.
 */

import assert from "node:assert/strict";
import { HerdrLink } from "../../../../dist/link/client.js";
import { PUBLIC_CONTRACT_EPOCH, PUBLIC_CONTRACT_HASH } from "../../../../dist/link/daemon.js";

const EDGE_HTTP = process.env.EDGE_URL ?? "http://127.0.0.1:8787";
const EDGE_WS = EDGE_HTTP.replace(/^http/, "ws") + "/ws";
const WORKSTATION_ID = process.env.DEFAULT_WORKSTATION_ID ?? "dev-real-link";
const SECRET = process.env.LINK_SHARED_SECRET ?? "dev-only-link-secret-change-me";
const CONTRACT_HASH = process.env.CONTRACT_HASH ?? PUBLIC_CONTRACT_HASH;

const calls = { dispatch: [], cancel: [] };

const transport = {
  name: "e2e-fake-runtime",
  async getRuntimeInfo() {
    return {
      runtime_version: "0.3.32-e2e",
      runtime_commit: "e2e",
      runtime_generation: "gen-e2e",
      contract_epoch: PUBLIC_CONTRACT_EPOCH,
      contract_hash: CONTRACT_HASH,
      herdr_version: "0.8.2",
      herdr_protocol: "20",
    };
  },
  async getHealth() {
    return { healthy: true, details: "e2e fake runtime" };
  },
  async dispatchRequest(req) {
    calls.dispatch.push(req);
    return {
      ok: true,
      result: {
        e2eServed: true,
        operation: req.operation,
        canonicalRequestId: req.request_id,
      },
    };
  },
  async cancelRequest(requestId, reason) {
    calls.cancel.push([requestId, reason]);
  },
};

async function until(fn, timeoutMs = 5000) {
  const start = Date.now();
  for (;;) {
    const value = fn();
    if (value) return value;
    if (Date.now() - start > timeoutMs) throw new Error("timed out waiting for condition");
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

async function main() {
  const healthResponse = await fetch(`${EDGE_HTTP}/health`);
  assert.equal(healthResponse.status, 200);

  const link = new HerdrLink({
    workstationId: WORKSTATION_ID,
    edgeUrl: EDGE_WS,
    linkToken: SECRET,
    transport,
    heartbeatMs: 1000,
    handshakeTimeoutMs: 3000,
    requestTimeoutMs: 5000,
    maxSilenceMs: 10_000,
    maxPending: 8,
  });

  link.start();
  await until(() => link.getStatus().phase === "online");
  const online = link.getStatus();
  console.log("link online:", JSON.stringify({
    phase: online.phase,
    workstation_id: online.workstation_id,
    protocol_version: online.protocol_version,
    runtime: online.runtime,
  }));

  const callResponse = await fetch(`${EDGE_HTTP}/mcp`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-herdr-workstation": WORKSTATION_ID,
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "tools/call",
      params: { name: "herdr_inspect", arguments: {} },
    }),
  });
  const call = await callResponse.json();
  console.log("tools/call:", callResponse.status, JSON.stringify(call));

  assert.equal(callResponse.status, 200);
  assert.equal(call.jsonrpc, "2.0");
  assert.equal(call.id, 1);
  assert.equal(call.error, undefined);
  assert.equal(call.result?.isError, undefined);
  assert.equal(call.result?.structuredContent?.e2eServed, true);
  assert.equal(call.result?.structuredContent?.operation, "herdr_inspect");
  assert.equal(calls.dispatch.length, 1);
  assert.equal(calls.dispatch[0].operation, "herdr_inspect");
  assert.equal(typeof calls.dispatch[0].request_id, "string");

  await link.close({ reason: "e2e_complete", drainMs: 250 });
  console.log("herdr-link e2e OK");
}

main().catch((error) => {
  console.error("herdr-link e2e failed:", error);
  process.exitCode = 1;
});
