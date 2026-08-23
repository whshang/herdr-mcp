#!/usr/bin/env node
/** Verify one-active-link fencing against a local Wrangler dev edge. */

import assert from "node:assert/strict";
import { HerdrLink } from "../../../../dist/link/client.js";

const EDGE_HTTP = process.env.EDGE_URL ?? "http://127.0.0.1:8787";
const EDGE_WS = EDGE_HTTP.replace(/^http/, "ws") + "/ws";
const SECRET = process.env.LINK_SHARED_SECRET ?? "dev-only-link-secret-change-me";
const WORKSTATION_ID = process.env.DEMO_WORKSTATION_ID ?? "dev-fence-link";
const CONTRACT_HASH = "sha256:3f23083ae31b977dad21b1ec9d6919c49e1067a27f7b7eea7bdd021b54770c0d";

async function until(fn, timeoutMs = 5000) {
  const start = Date.now();
  for (;;) {
    const value = fn();
    if (value) return value;
    if (Date.now() - start > timeoutMs) throw new Error("timed out waiting for condition");
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

function makeTransport(owner) {
  const calls = [];
  return {
    calls,
    transport: {
      name: `fence-${owner}`,
      async getRuntimeInfo() {
        return {
          runtime_version: "0.3.26-e2e",
          runtime_commit: owner,
          runtime_generation: `gen-${owner}`,
          contract_epoch: 1,
          contract_hash: CONTRACT_HASH,
          herdr_version: "0.8.2",
          herdr_protocol: "20",
        };
      },
      async getHealth() { return { healthy: true }; },
      async dispatchRequest(req) {
        calls.push(req);
        return { ok: true, result: { owner, request_id: req.request_id } };
      },
      async cancelRequest() {},
    },
  };
}

function makeLink(transport) {
  return new HerdrLink({
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
}

async function main() {
  const firstRuntime = makeTransport("first");
  const first = makeLink(firstRuntime.transport);
  const firstExitP = first.connect();
  await until(() => first.getStatus().phase === "online");

  const secondRuntime = makeTransport("second");
  const second = makeLink(secondRuntime.transport);
  const secondExitP = second.connect();
  await until(() => second.getStatus().phase === "online");

  const firstExit = await Promise.race([
    firstExitP,
    new Promise((_, reject) => setTimeout(() => reject(new Error("old link was not fenced")), 3000)),
  ]);
  assert.equal(firstExit.kind, "superseded");
  assert.equal(firstRuntime.calls.length, 0);

  const response = await fetch(`${EDGE_HTTP}/mcp`, {
    method: "POST",
    headers: { "content-type": "application/json", "x-herdr-workstation": WORKSTATION_ID },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 11,
      method: "tools/call",
      params: { name: "herdr_inspect", arguments: {} },
    }),
  });
  const body = await response.json();
  assert.equal(body.result?.structuredContent?.owner, "second");
  assert.equal(firstRuntime.calls.length, 0);
  assert.equal(secondRuntime.calls.length, 1);

  await second.close({ reason: "fence_e2e_complete", drainMs: 250 });
  const secondExit = await secondExitP;
  assert.equal(secondExit.kind, "stopped");
  console.log("link fencing e2e OK", JSON.stringify({ firstExit: firstExit.kind, servedBy: "second" }));
}

main().catch((error) => {
  console.error("link fencing e2e failed:", error);
  process.exitCode = 1;
});
