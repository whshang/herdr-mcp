import { test } from "node:test";
import assert from "node:assert/strict";
import { EPOCH1_CONTRACT_HASH, readLinkDaemonConfig } from "../dist/link/daemon.js";

function env(overrides = {}) {
  return {
    HERDR_EDGE_URL: "wss://herdr-edge-dev.example/ws",
    HERDR_WORKSTATION_ID: "dev-w1",
    HERDR_LINK_TOKEN: "link-secret",
    HERDR_MCP_TOKEN: "runtime-secret",
    ...overrides,
  };
}

test("daemon config uses frozen epoch-1 hash and loopback MCP default", () => {
  const cfg = readLinkDaemonConfig(env());
  assert.equal(cfg.contractHash, EPOCH1_CONTRACT_HASH);
  assert.equal(cfg.runtimeEndpoint, "http://127.0.0.1:8772/mcp");
  assert.equal(cfg.edgeUrl, "wss://herdr-edge-dev.example/ws");
  assert.equal(cfg.workstationId, "dev-w1");
});

test("daemon config fails closed on missing credentials", () => {
  for (const key of ["HERDR_LINK_TOKEN", "HERDR_MCP_TOKEN"]) {
    const input = env();
    delete input[key];
    assert.throws(() => readLinkDaemonConfig(input), new RegExp(`${key} is required`));
  }
});

test("daemon config rejects non-websocket edge and contract drift", () => {
  assert.throws(() => readLinkDaemonConfig(env({ HERDR_EDGE_URL: "https://example.com/ws" })), /wss:\/\/ or ws:\/\//);
  assert.throws(
    () => readLinkDaemonConfig(env({ HERDR_CONTRACT_HASH: `sha256:${"0".repeat(64)}` })),
    /contract hash differs from frozen epoch 1/,
  );
});

test("daemon config validates workstation id", () => {
  assert.throws(() => readLinkDaemonConfig(env({ HERDR_WORKSTATION_ID: "bad/id" })), /WORKSTATION_ID is invalid/);
});
