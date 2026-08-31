import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { PUBLIC_CONTRACT_EPOCH, PUBLIC_CONTRACT_HASH } from "../dist/link/daemon.js";
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

test("public epoch 3 is independent while runtime/link operational contract stays epoch 2", () => {
  const expectedHash = contractFixture.contract_hash;
  const expectedEpoch = contractFixture.contract_epoch;
  const expectedCount = contractFixture.tool_count;

  assert.equal(PUBLIC_CONTRACT, EPOCH3_CONTRACT);
  assert.equal(RUNTIME_EXECUTION_CONTRACT, EPOCH2_CONTRACT);
  assert.deepEqual(EPOCH2_CONTRACT, contractFixture);
  assert.equal(expectedEpoch, 2);
  assert.equal(expectedCount, 18);
  assert.equal(EPOCH2_CONTRACT.tools.some((tool) => tool.name === "herdr_skill"), true);
  assert.equal(EPOCH2_CONTRACT.tools.some((tool) => tool.name.startsWith("herdr_mcp.")), false);

  assert.equal(PUBLIC_CONTRACT_EPOCH, expectedEpoch);
  assert.equal(PUBLIC_CONTRACT_HASH, expectedHash);
  assert.equal(EDGE_EPOCH, 3);
  assert.equal(EDGE_HASH, EPOCH3_CONTRACT.contract_hash);
  assert.equal(EPOCH3_CONTRACT.tool_count, 19);
  assert.equal(EPOCH3_CONTRACT.tools.some((tool) => tool.name === "herdr_devices"), true);
  assert.equal(SELF_UPDATE_HASH, expectedHash);
  assert.equal(SELF_UPDATE_TOOL_COUNT, expectedCount);
  assert.equal(PUBLIC_CONTRACT_PROFILE, "epoch2");
  assert.equal(DOMAIN_EPOCH, expectedEpoch);
  assert.equal(DOMAIN_HASH, expectedHash);
  assert.equal(DOMAIN_TOOL_COUNT, expectedCount);
});

test("public MCP identity version follows package/runtime release version", () => {
  assert.equal(MCP_SERVER_VERSION, packageJson.version);
  assert.equal(DOMAIN_RUNTIME_VERSION, packageJson.version);
});
