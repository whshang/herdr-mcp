import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  PUBLIC_CONTRACT_HASH as SELF_UPDATE_HASH,
  PUBLIC_CONTRACT_PROFILE,
  REQUIRED_TOOL_COUNT as SELF_UPDATE_TOOL_COUNT,
} from "../bin/herdr-self-update";
import {
  CONTRACT_EPOCH as DOMAIN_EPOCH,
  CONTRACT_HASH as DOMAIN_HASH,
  REQUIRED_TOOL_COUNT as DOMAIN_TOOL_COUNT,
  RUNTIME_VERSION as DOMAIN_RUNTIME_VERSION,
} from "../bin/herdr-cloudflare-domain";
import { EPOCH2_CONTRACT } from "../edge/cloudflare/dist/contracts/epoch2.js";
import { EPOCH3_CONTRACT } from "../edge/cloudflare/dist/contracts/epoch3.js";
import { PUBLIC_CONTRACT } from "../edge/cloudflare/dist/contracts/public.js";
import { RUNTIME_EXECUTION_CONTRACT } from "../edge/cloudflare/dist/contracts/runtime.js";
import {
  CONTRACT_EPOCH as EDGE_EPOCH,
  CONTRACT_HASH as EDGE_HASH,
  MCP_SERVER_VERSION,
} from "../edge/cloudflare/dist/version.js";

const packageJson = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
const contractFixture = JSON.parse(
  await readFile(new URL("../contracts/epoch2.json", import.meta.url), "utf8"),
);

test("user keeps runtime execution separate from public epochs | Given independent public and runtime contracts | When current identities are read | Then epoch 7 runtime remains distinct from public epochs", () => {
  const frozenHash = contractFixture.contract_hash;
  const frozenEpoch = contractFixture.contract_epoch;
  const frozenCount = contractFixture.tool_count;

  assert.equal(PUBLIC_CONTRACT, EPOCH3_CONTRACT);
  assert.deepEqual(EPOCH2_CONTRACT, contractFixture);
  assert.equal(frozenEpoch, 2);
  assert.equal(frozenCount, 18);
  assert.equal(EPOCH2_CONTRACT.tools.some((tool) => tool.name === "herdr_skill"), true);
  assert.equal(EPOCH2_CONTRACT.tools.some((tool) => tool.name.startsWith("herdr_mcp.")), false);

  // Edge and Rust advertise the same current epoch-7 runtime identity.
  assert.equal(RUNTIME_EXECUTION_CONTRACT.contract_epoch, 7);
  assert.equal(RUNTIME_EXECUTION_CONTRACT.tool_count, 18);
  assert.equal(RUNTIME_EXECUTION_CONTRACT.contract_hash, "sha256:6fa3ce18830cacf928fd9b5a22bcbead949510dea33c3d2e799e2c9cbad075e1");

  assert.equal(EDGE_EPOCH, 3);
  assert.equal(EDGE_HASH, EPOCH3_CONTRACT.contract_hash);
  assert.equal(EPOCH3_CONTRACT.tool_count, 19);
  assert.equal(EPOCH3_CONTRACT.tools.some((tool) => tool.name === "herdr_devices"), true);

  // The legacy Node self-update path stays pinned to the frozen epoch-2 catalog.
  assert.equal(SELF_UPDATE_HASH, frozenHash);
  assert.equal(SELF_UPDATE_TOOL_COUNT, frozenCount);
  assert.equal(PUBLIC_CONTRACT_PROFILE, "epoch2");

  // The cutover/domain probe follows the current runtime execution contract.
  assert.equal(DOMAIN_EPOCH, 7);
  assert.match(DOMAIN_HASH, /^sha256:[0-9a-f]{64}$/);
  assert.equal(DOMAIN_TOOL_COUNT, 18);
});

test("public MCP identity version follows package/runtime release version", () => {
  assert.equal(MCP_SERVER_VERSION, packageJson.version);
  assert.equal(DOMAIN_RUNTIME_VERSION, packageJson.version);
});
