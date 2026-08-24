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
import { PUBLIC_CONTRACT } from "../edge/cloudflare/dist/contracts/public.js";
import {
  CONTRACT_EPOCH as EDGE_EPOCH,
  CONTRACT_HASH as EDGE_HASH,
  MCP_SERVER_VERSION,
} from "../edge/cloudflare/dist/version.js";

const packageJson = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));

test("epoch-2 public contract identity is identical across runtime, link, edge and operational CLIs", () => {
  const expectedHash = EPOCH2_CONTRACT.contract_hash;
  const expectedEpoch = EPOCH2_CONTRACT.contract_epoch;
  const expectedCount = EPOCH2_CONTRACT.tool_count;

  assert.equal(PUBLIC_CONTRACT, EPOCH2_CONTRACT);
  assert.equal(expectedEpoch, 2);
  assert.equal(expectedCount, 18);
  assert.equal(EPOCH2_CONTRACT.tools.some((tool) => tool.name === "herdr_skill"), true);

  assert.equal(PUBLIC_CONTRACT_EPOCH, expectedEpoch);
  assert.equal(PUBLIC_CONTRACT_HASH, expectedHash);
  assert.equal(EDGE_EPOCH, expectedEpoch);
  assert.equal(EDGE_HASH, expectedHash);
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
