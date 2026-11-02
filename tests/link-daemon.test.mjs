/**
 * Legacy Node Link compatibility coverage. Rust owns the production Link runtime;
 * retain this suite only while the Node compatibility entrypoint remains supported.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  LEGACY_EPOCH1_CONTRACT_HASH,
  PUBLIC_CONTRACT_EPOCH,
  PUBLIC_CONTRACT_HASH,
  readLinkDaemonConfig,
} from "../dist/link/daemon.js";
import { LOCAL_MCP_CONTRACT_EPOCH } from "../dist/link/local-mcp-transport.js";
import { RuntimeGenerationManager } from "../dist/link/runtime-generation.js";

function env(overrides = {}) {
  return {
    HERDR_EDGE_URL: "wss://herdr-edge-dev.example/ws",
    HERDR_WORKSTATION_ID: "dev-w1",
    HERDR_LINK_TOKEN: "link-secret",
    HERDR_MCP_TOKEN: "runtime-secret",
    ...overrides,
  };
}

test("user gets a startable config | Given the default daemon env | When readLinkDaemonConfig runs | Then the current epoch-5 identity and loopback MCP endpoint are returned", () => {
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

test("user cannot start a rollback contract | Given the frozen epoch-1 pair | When readLinkDaemonConfig runs | Then it is rejected before any manager construction", () => {
  assert.throws(
    () => readLinkDaemonConfig(env({ HERDR_CONTRACT_EPOCH: "1", HERDR_CONTRACT_HASH: LEGACY_EPOCH1_CONTRACT_HASH })),
    /requires contract epoch 5/,
  );
  // A legacy hash without an explicit epoch no longer defaults to a runnable pair either.
  assert.throws(
    () => readLinkDaemonConfig(env({ HERDR_CONTRACT_HASH: LEGACY_EPOCH1_CONTRACT_HASH })),
    /requires contract epoch 5/,
  );
});

test("daemon config validates workstation id", () => {
  assert.throws(() => readLinkDaemonConfig(env({ HERDR_WORKSTATION_ID: "bad/id" })), /WORKSTATION_ID is invalid/);
});

test("user starts the Node Link daemon | Given the default daemon env | When the runLinkDaemon manager wiring is constructed | Then construction succeeds and the advertised contract is epoch 5 over the v5 hash", async () => {
  // Regression for #819: the daemon failed at startup because the transport and
  // manager froze the runtime execution epoch at 2 while the daemon only accepted 5.
  assert.equal(LOCAL_MCP_CONTRACT_EPOCH, PUBLIC_CONTRACT_EPOCH);

  const cfg = readLinkDaemonConfig(env());
  const manager = new RuntimeGenerationManager({
    base: { generation: cfg.runtimeGeneration, endpoint: cfg.runtimeEndpoint },
    bearerToken: cfg.runtimeToken,
    contractHash: cfg.contractHash,
    contractEpoch: cfg.contractEpoch,
    defaultTimeoutMs: 30_000,
    maxTimeoutMs: 60_000,
    observationChecks: 3,
    observationIntervalMs: 500,
  });
  assert.equal(manager.activeGenerationId, cfg.runtimeGeneration);

  const info = await manager.getRuntimeInfo();
  assert.equal(info.contract_epoch, PUBLIC_CONTRACT_EPOCH);
  assert.equal(info.contract_hash, PUBLIC_CONTRACT_HASH);
});
