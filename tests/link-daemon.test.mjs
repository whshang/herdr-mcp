import { test } from "node:test";
import assert from "node:assert/strict";
import {
  LEGACY_EPOCH1_CONTRACT_HASH,
  PUBLIC_CONTRACT_EPOCH,
  PUBLIC_CONTRACT_HASH,
  readLinkDaemonConfig,
} from "../dist/link/daemon.js";

function env(overrides = {}) {
  return {
    HERDR_EDGE_URL: "wss://herdr-edge-dev.example/ws",
    HERDR_WORKSTATION_ID: "dev-w1",
    HERDR_LINK_TOKEN: "link-secret",
    HERDR_MCP_TOKEN: "runtime-secret",
    ...overrides,
  };
}

test("daemon config uses public epoch-2 identity and loopback MCP default", () => {
  const cfg = readLinkDaemonConfig(env());
  assert.equal(cfg.contractEpoch, PUBLIC_CONTRACT_EPOCH);
  assert.equal(cfg.contractHash, PUBLIC_CONTRACT_HASH);
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

test("daemon config rejects non-websocket edge and invalid contract pairs", () => {
  assert.throws(() => readLinkDaemonConfig(env({ HERDR_EDGE_URL: "https://example.com/ws" })), /wss:\/\/ or ws:\/\//);
  assert.throws(
    () => readLinkDaemonConfig(env({ HERDR_CONTRACT_HASH: `sha256:${"0".repeat(64)}` })),
    /not a supported public or rollback contract/,
  );
  assert.throws(
    () => readLinkDaemonConfig(env({ HERDR_CONTRACT_EPOCH: "1", HERDR_CONTRACT_HASH: PUBLIC_CONTRACT_HASH })),
    /not a supported public or rollback contract/,
  );
});

test("daemon config accepts the frozen epoch-1 pair for supervised rollback", () => {
  const cfg = readLinkDaemonConfig(env({ HERDR_CONTRACT_EPOCH: "1", HERDR_CONTRACT_HASH: LEGACY_EPOCH1_CONTRACT_HASH }));
  assert.equal(cfg.contractEpoch, 1);
  assert.equal(cfg.contractHash, LEGACY_EPOCH1_CONTRACT_HASH);
});

test("daemon config validates workstation id", () => {
  assert.throws(() => readLinkDaemonConfig(env({ HERDR_WORKSTATION_ID: "bad/id" })), /WORKSTATION_ID is invalid/);
});
